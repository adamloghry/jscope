{ pkgs, binary, module }:

pkgs.testers.runNixOSTest {
  name = "jscope-systemd-follow";
  requiredFeatures.kvm = false;
  qemu.forceAccel = false;

  nodes.machine = { config, lib, pkgs, ... }: {
    imports = [ module ];
    programs.jscope = {
      generationMarkers.enable = true;
      enable = true;
      backlog = 8;
      pinnedUnits = [ "jscope-target.service" ];
      mutedUnits = [ "jscope-foreign.service" ];
    };
    specialisation.marker-test.configuration.environment.etc."jscope-specialisation".text = "test";
    # A parent closure already contains every specialisation. Keep hello exclusively
    # in the parent's own profile so switching to the child really removes it.
    environment.systemPackages = [ pkgs.python3 ]
      ++ lib.optional (config.specialisation != { }) pkgs.hello;
    nix.enable = true;
    services.journald.storage = "persistent";

    users.users.observer = {
      isNormalUser = true;
      extraGroups = [ "systemd-journal" ];
    };

    systemd.services.jscope-target = {
      serviceConfig.Type = "oneshot";
      script = ''
        phase=$(cat /run/jscope-phase)
        printf '<4>JSCOPE_%s_PRIORITY_EXCLUDED\n' "$phase"
        printf '<3>JSCOPE_%s_KEEP\n' "$phase"
      '';
    };
    systemd.services.jscope-foreign = {
      serviceConfig.Type = "oneshot";
      script = ''
        printf '<3>JSCOPE_UNIT_EXCLUDED\n'
      '';
    };
  };

  testScript = ''
    import json

    def markers():
        machine.succeed("journalctl --sync")
        return [
            json.loads(line) for line in machine.succeed(
                "journalctl -b SYSLOG_IDENTIFIER=jscope-activation JSCOPE_MARKER=nixos-activation-v1 -o json"
            ).splitlines() if line.startswith("{")
        ]

    def send_target(phase: str) -> None:
        machine.succeed(f"echo {phase} > /run/jscope-phase")
        machine.succeed("systemctl start jscope-target.service; journalctl --sync")

    def exercise_follow(phase: str, backlog: int, host_default: bool = False) -> None:
        probe = f"jscope-{phase.lower()}"
        work = f"/tmp/{probe}"
        machine.succeed(f"install -d -o observer -g users {work}")
        machine.succeed(
            f"systemd-run --unit={probe} -p User=observer -p WorkingDirectory={work} "
            "${pkgs.python3}/bin/python ${./vm-probe.py} ${binary}/bin/jscope "
            f"{phase} {backlog}" + (" default" if host_default else "")
        )

        def await_probe(marker: str) -> None:
            machine.wait_until_succeeds(
                f"test -e {work}/{marker} || systemctl is-failed {probe}.service --quiet",
                timeout=60,
            )
            if machine.execute(f"test -e {work}/{marker}")[0] != 0:
                raise RuntimeError(machine.succeed(f"journalctl -u {probe}.service --no-pager"))

        await_probe("ready")
        machine.succeed("systemctl start jscope-foreign.service; journalctl --sync")
        send_target(phase)
        machine.succeed(
            f"journalctl -b -u jscope-target.service -o cat | grep JSCOPE_{phase}_PRIORITY_EXCLUDED"
        )
        machine.succeed(
            "journalctl -b -u jscope-foreign.service -o cat | grep JSCOPE_UNIT_EXCLUDED"
        )
        await_probe("passed")

    machine.start()
    machine.wait_for_unit("systemd-journald.service")
    machine.wait_for_unit("systemd-journal-flush.service")
    machine.wait_for_unit("multi-user.target")
    base = machine.succeed("readlink -e /run/current-system").strip()
    special = machine.succeed("readlink -e /run/current-system/specialisation/marker-test").strip()
    assert [m["JSCOPE_CURRENT_SYSTEM"] for m in markers()] == [base]
    assert "JSCOPE_PREVIOUS_SYSTEM" not in markers()[0]
    machine.succeed(f"{special}/bin/switch-to-configuration dry-activate")
    machine.succeed(f"{base}/bin/switch-to-configuration test")
    assert len(markers()) == 1
    machine.succeed(f"{special}/bin/switch-to-configuration test")
    machine.succeed(f"{base}/bin/switch-to-configuration test")
    assert [(m.get("JSCOPE_PREVIOUS_SYSTEM"), m["JSCOPE_CURRENT_SYSTEM"]) for m in markers()] == [
        (None, base), (base, special), (special, base)
    ]
    machine.succeed(
        "runuser -u observer -- "
        "${pkgs.python3.withPackages (p: [ p.pyte ])}/bin/python ${./vm-diff-probe.py} "
        "${../tests/terminal.py} ${binary}/bin/jscope "
        f"{base} {special} ${pkgs.hello.version}"
    )
    machine.succeed("command -v jscope && jscope --version")
    defaults = json.loads(machine.succeed("cat /etc/jscope/defaults.json"))
    assert defaults == {"backlog": 8, "pinnedUnits": ["jscope-target.service"], "mutedUnits": ["jscope-foreign.service"]}
    send_target("PRIOR_BOOT")
    machine.shutdown()

    machine.start()
    machine.wait_for_unit("systemd-journald.service")
    machine.wait_for_unit("systemd-journal-flush.service")
    machine.succeed(
        "journalctl -b -1 -u jscope-target.service -p err -o cat | grep JSCOPE_PRIOR_BOOT_KEEP"
    )
    machine.wait_for_unit("multi-user.target")
    assert [m["JSCOPE_CURRENT_SYSTEM"] for m in markers()] == [base]
    send_target("PREEXISTING")
    exercise_follow("BACKLOG_FOLLOW", 10)
    exercise_follow("ZERO_FOLLOW", 0)
    exercise_follow("HOST_DEFAULT", 8, host_default=True)
  '';
}
