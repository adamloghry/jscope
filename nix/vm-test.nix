{ pkgs, binary }:

pkgs.testers.runNixOSTest {
  name = "jscope-systemd-follow";
  requiredFeatures.kvm = false;
  qemu.forceAccel = false;

  nodes.machine = { pkgs, ... }: {
    environment.systemPackages = [ binary pkgs.python3 ];
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
    def send_target(phase: str) -> None:
        machine.succeed(f"echo {phase} > /run/jscope-phase")
        machine.succeed("systemctl start jscope-target.service; journalctl --sync")

    def exercise_follow(phase: str, backlog: int) -> None:
        probe = f"jscope-{phase.lower()}"
        work = f"/tmp/{probe}"
        machine.succeed(f"install -d -o observer -g users {work}")
        machine.succeed(
            f"systemd-run --unit={probe} -p User=observer -p WorkingDirectory={work} "
            "${pkgs.python3}/bin/python ${./vm-probe.py} ${binary}/bin/jscope "
            f"{phase} {backlog}"
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
    send_target("PRIOR_BOOT")
    machine.shutdown()

    machine.start()
    machine.wait_for_unit("systemd-journald.service")
    machine.wait_for_unit("systemd-journal-flush.service")
    machine.succeed(
        "journalctl -b -1 -u jscope-target.service -p err -o cat | grep JSCOPE_PRIOR_BOOT_KEEP"
    )
    send_target("PREEXISTING")
    exercise_follow("BACKLOG_FOLLOW", 10)
    exercise_follow("ZERO_FOLLOW", 0)
  '';
}
