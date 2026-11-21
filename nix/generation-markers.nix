{ config, lib, pkgs, ... }:
let
  # The boot oneshot and the live switch hook share the journal field contract.
  emit = pkgs.writeShellScript "jscope-activation-marker" ''
    set -eu
    current=$1
    previous=''${2-}
    object=''${current##*/}
    hash=''${object%%-*}
    {
      printf 'SYSLOG_IDENTIFIER=jscope-activation\nPRIORITY=5\nJSCOPE_MARKER=nixos-activation-v1\n'
      printf 'MESSAGE=Activated %s [%s]\n' "''${object#*-}" "''${hash:0:8}"
      printf 'JSCOPE_CURRENT_SYSTEM=%s\n' "$current"
      if [[ -n "$previous" ]]; then
        printf 'JSCOPE_PREVIOUS_SYSTEM=%s\n' "$previous"
      fi
    } | ${pkgs.util-linux}/bin/logger --journald
    ${pkgs.coreutils}/bin/touch /run/jscope-activation-seen
  '';
in {
  options.programs.jscope.generationMarkers.enable = lib.mkEnableOption
    "structured JournalScope journal entries for boots and live NixOS closure changes";

  config = lib.mkIf config.programs.jscope.generationMarkers.enable {
    # activate sets systemConfig to the new toplevel and updates the current-system
    # link after running these snippets. Dry activation omits this snippet.
    system.activationScripts.jscopeGenerationMarker = {
      supportsDryActivation = false;
      text = ''
        (
          previous=$(readlink -e /run/current-system) || exit 0
          current=$(readlink -e "$systemConfig") || exit 0
          if [[ -e /run/booted-system && "$previous" != "$current" ]]; then
            ${emit} "$current" "$previous" || echo "jscope: could not record NixOS activation" >&2
          fi
        )
      '';
    };

    # Stage 2 activation may run before journald. Record the boot closure once
    # its journal is available. A first live opt-in has already written its marker.
    systemd.services.jscope-boot-marker = {
      description = "Record the booted NixOS closure in the journal";
      wantedBy = [ "multi-user.target" ];
      after = [ "systemd-journald.socket" ];
      requires = [ "systemd-journald.socket" ];
      unitConfig.ConditionPathExists = "!/run/jscope-activation-seen";
      serviceConfig.Type = "oneshot";
      script = ''
        ${emit} "$(${pkgs.coreutils}/bin/readlink -e /run/booted-system)"
      '';
    };
  };
}
