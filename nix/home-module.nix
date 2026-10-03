{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.niri-screenshare;
  settingsFormat = pkgs.formats.toml { };
in
{
  options.services.niri-screenshare = {
    enable = lib.mkEnableOption "the niri screen-sharing portal";

    package = lib.mkOption {
      type = lib.types.either (lib.types.enum [
        "native"
        "gtk"
      ]) lib.types.package;
      default = "native";
      apply =
        value:
        if builtins.isString value then pkgs.callPackage ./package.nix { withPicker = value; } else value;
      description = ''
        Picker implementation to build, or a custom niri-screenshare package.
      '';
    };

    settings = lib.mkOption {
      type = settingsFormat.type;
      default = { };
      example = {
        native_picker.style.hover_background = "#3a5068";
      };
      description = "Settings written to niri-screenshare/config.toml.";
    };
  };

  config = lib.mkIf cfg.enable {
    xdg.portal = {
      enable = true;
      extraPortals = [ cfg.package ];
      config.niri."org.freedesktop.impl.portal.ScreenCast" = "niri";
    };

    xdg.configFile."niri-screenshare/config.toml" = lib.mkIf (cfg.settings != { }) {
      source = settingsFormat.generate "niri-screenshare-config.toml" cfg.settings;
    };

    systemd.user.services.niri-screenshare = {
      Unit = {
        Description = "Portal service (niri backend)";
        PartOf = [ "graphical-session.target" ];
        After = [ "graphical-session.target" ];
        Requisite = [ "graphical-session.target" ];
      };
      Service = {
        Type = "dbus";
        BusName = "org.freedesktop.impl.portal.desktop.niri";
        ExecStart = lib.getExe cfg.package;
        Restart = "on-failure";
      };
      Install.WantedBy = [ "graphical-session.target" ];
    };
  };
}
