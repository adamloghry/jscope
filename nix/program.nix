{ defaultPackage }:
{ config, lib, ... }:
let
  cfg = config.programs.jscope;
in
{
  options.programs.jscope = {
    enable = lib.mkEnableOption "JournalScope, the interactive systemd journal viewer";
    package = lib.mkOption {
      type = lib.types.package;
      default = defaultPackage;
      defaultText = lib.literalExpression "journalscope.packages.<system>.default";
      description = "The jscope executable to install.";
    };
    backlog = lib.mkOption {
      type = lib.types.ints.u32;
      default = 200;
      description = "Matching journal entries to read before following.";
    };
    pinnedUnits = lib.mkOption {
      type = lib.types.listOf lib.types.nonEmptyStr;
      default = [];
      example = [ "postgresql.service" "nginx.service" ];
      description = "Up to two distinct services to pin on startup.";
    };
    mutedUnits = lib.mkOption {
      type = lib.types.listOf lib.types.nonEmptyStr;
      default = [];
      description = "Services whose lower-severity entries are initially muted.";
    };
  };
  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = builtins.length (lib.unique cfg.pinnedUnits) <= 2;
        message = "programs.jscope.pinnedUnits supports at most two distinct services";
      }
      {
        assertion = builtins.length (lib.unique cfg.mutedUnits) <= 128;
        message = "programs.jscope.mutedUnits supports at most 128 distinct services";
      }
    ];
    environment.systemPackages = [ cfg.package ];
    environment.etc."jscope/defaults.json".text = builtins.toJSON {
      inherit (cfg) backlog mutedUnits pinnedUnits;
    };
  };
}
