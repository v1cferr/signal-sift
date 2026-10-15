# Home Manager module: runs SignalSift as a systemd *user* service.
#
# A user service, not a system one: the daemon only reads public content and
# writes to the user's XDG directories, so it needs no privileges, and it
# starts and stops with the graphical session that consumes it (GLANCE).
#
# Usage from a flake-based Home Manager configuration:
#
#   imports = [ inputs.signal-sift.homeManagerModules.default ];
#   services.signalsift.enable = true;
#   services.signalsift.settings.runtime.profile = "balanced";
self:
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.services.signalsift;
  toml = pkgs.formats.toml { };
  configFile = toml.generate "signalsift.toml" cfg.settings;
in
{
  options.services.signalsift = {
    enable = lib.mkEnableOption "the SignalSift content intelligence daemon";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.signalsift;
      defaultText = lib.literalExpression "signal-sift.packages.\${system}.signalsift";
      description = "SignalSift package to run.";
    };

    settings = lib.mkOption {
      inherit (toml) type;
      default = { };
      example = {
        runtime.profile = "gaming";
        media.subtitle_languages = [
          "pt"
          "en"
        ];
      };
      description = ''
        Configuration written to the daemon's config file. See
        config/signalsift.example.toml for every key and its default.
      '';
    };

    extraPackages = lib.mkOption {
      type = lib.types.listOf lib.types.package;
      default = [
        pkgs.yt-dlp
        pkgs.ffmpeg-headless
      ];
      defaultText = lib.literalExpression "[ pkgs.yt-dlp pkgs.ffmpeg-headless ]";
      description = ''
        Tools put on the service's PATH. Add whisper-cpp or llama-cpp here
        (with `vulkanSupport = true`) to enable local transcription and inference.
      '';
    };

    memoryMax = lib.mkOption {
      type = lib.types.str;
      default = "6G";
      description = ''
        Hard memory limit for the service cgroup. It covers child processes
        too, so it must leave room for a speech or language model while one
        runs; the daemon alone stays far below it.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    systemd.user.services.signalsift = {
      Unit = {
        Description = "SignalSift content intelligence daemon";
        Documentation = "https://github.com/v1cferr/signal-sift";
        After = [ "network-online.target" ];
      };

      Service = {
        ExecStart = "${lib.getExe cfg.package} --config ${configFile} serve";
        Restart = "on-failure";
        RestartSec = 10;
        Environment = [
          "PATH=${lib.makeBinPath cfg.extraPackages}"
          # Fewer glibc malloc arenas: less retained memory for a mostly idle
          # process whose few threads rarely contend.
          "MALLOC_ARENA_MAX=2"
        ];

        # Yield to the desktop: background work should never be what makes a
        # game or the compositor stutter.
        Nice = 10;
        CPUWeight = 20;
        IOWeight = 20;
        MemoryMax = cfg.memoryMax;

        # Hardening that works in user units without extra privileges.
        NoNewPrivileges = true;
        LockPersonality = true;
        RestrictRealtime = true;
        RestrictSUIDSGID = true;
        RestrictAddressFamilies = [
          "AF_INET"
          "AF_INET6"
          "AF_UNIX"
        ];
        SystemCallArchitectures = "native";
        UMask = "0077";
      };

      Install.WantedBy = [ "default.target" ];
    };
  };
}
