self:
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.programs.mtsw-shot;
  json = pkgs.formats.json { };
in
{
  options.programs.mtsw-shot = {
    enable = lib.mkEnableOption "mtsw-shot, a screenshot and annotation tool for Wayland";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      defaultText = lib.literalExpression "mtsw-shot.packages.\${system}.default";
      description = "The mtsw-shot package to install.";
    };

    settings = lib.mkOption {
      inherit (json) type;
      default = { };
      example = lib.literalExpression ''
        {
          accept_actions = [ "copy" "save" ];
          save.path = "~/Pictures/Screenshots";
          drawing.color = "#ff0000";
          ui.show_magnifier = true;
          shortcuts.pencil = "Q";
        }
      '';
      description = ''
        Written to {file}`$XDG_CONFIG_HOME/mtsw-shot/config.json`. Every key is optional;
        run `mtsw-shot config --default` to see all of them.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    home.packages = [ cfg.package ];

    xdg.configFile."mtsw-shot/config.json" = lib.mkIf (cfg.settings != { }) {
      source = json.generate "mtsw-shot-config.json" cfg.settings;
    };
  };
}
