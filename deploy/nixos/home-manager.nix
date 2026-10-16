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
  own = self.packages.${pkgs.stdenv.hostPlatform.system};
  toml = pkgs.formats.toml { };
  configFile = toml.generate "signalsift.toml" cfg.settings;
in
{
  options.services.signalsift = {
    enable = lib.mkEnableOption "the SignalSift content intelligence daemon";

    package = lib.mkOption {
      type = lib.types.package;
      default = own.signalsift;
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
        own.whisper-cpp-vulkan
        own.llama-cpp-vulkan
      ];
      defaultText = lib.literalExpression "[ pkgs.yt-dlp pkgs.ffmpeg-headless signal-sift.packages.\${system}.whisper-cpp-vulkan signal-sift.packages.\${system}.llama-cpp-vulkan ]";
      description = ''
        Tools put on the service's PATH. The inference engines default to
        SignalSift's own Vulkan builds (the benchmarked versions); with
        runtime.require_gpu (the default) they must run on the GPU or the
        job fails instead of falling back to the CPU.
      '';
    };

    memoryMax = lib.mkOption {
      type = lib.types.str;
      default = "10G";
      description = ''
        Hard memory limit for the service cgroup. It covers child processes
        too: llama-server with gemma-4-E4B peaked at ~7 GiB RSS (most of it
        the memory-mapped model file), whisper.cpp at ~300 MiB. The daemon
        alone stays around 10-20 MiB.
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
