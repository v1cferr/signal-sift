{
  # ═══════════════════════════════════════════════════════════════════════════
  # SignalSift — toolchain, package and NixOS/Home Manager service module.
  #
  # The Rust toolchain comes straight from nixpkgs instead of rust-overlay or
  # fenix: one input fewer to keep locked, and the stable rustc that nixpkgs
  # ships is recent enough for every dependency we use. If a crate ever needs
  # a newer compiler, that is the moment to add an overlay, not before.
  #
  # Media tools (yt-dlp, ffmpeg) are in the default shell because the PoC calls
  # them as subprocesses. Inference engines (whisper.cpp, llama.cpp) live in a
  # separate `inference` shell: their Vulkan builds are compiled from source,
  # and nobody who only wants to run `cargo test` should pay for that.
  # ═══════════════════════════════════════════════════════════════════════════
  description = "SignalSift — local-first content intelligence daemon";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs, ... }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      formatter = forAllSystems (pkgs: pkgs.nixfmt-tree);

      packages = forAllSystems (pkgs: {
        default = self.packages.${pkgs.stdenv.hostPlatform.system}.signalsift;
        signalsift = pkgs.callPackage ./deploy/nixos/package.nix { };
        # The inference engines with the Vulkan backend, from THIS flake's
        # pinned nixpkgs: the exact versions benchmarked in docs/benchmarks/.
        # The Home Manager module puts them on the service's PATH, so a host
        # on another channel still runs the measured builds (ADR 0011).
        whisper-cpp-vulkan = pkgs.whisper-cpp.override { vulkanSupport = true; };
        llama-cpp-vulkan = pkgs.llama-cpp.override { vulkanSupport = true; };
      });

      # `nix flake check` builds the package, which runs the Rust test suite
      # inside the sandbox: no network, no GPU, no host tools.
      checks = forAllSystems (pkgs: {
        package = self.packages.${pkgs.stdenv.hostPlatform.system}.signalsift;
      });

      homeManagerModules.default = import ./deploy/nixos/home-manager.nix self;

      devShells = forAllSystems (
        pkgs:
        let
          rustTools = with pkgs; [
            rustc
            cargo
            clippy
            rustfmt
            rust-analyzer
            cargo-deny # license and advisory checks, see docs/adr/0007
          ];
          mediaTools = with pkgs; [
            yt-dlp # subtitles and metadata; the PoC calls it as a subprocess
            ffmpeg-headless # audio extraction only, no X11/SDL closure
            sqlite # the CLI, for inspecting the database by hand
          ];
          # The host profile may export a global LD_LIBRARY_PATH built against a
          # different glibc. Inherited here, it makes nixpkgs binaries such as
          # ffmpeg fail with "GLIBC_2.43 not found". Nix binaries carry their own
          # RPATH, so the shell does not need it at all.
          shellHook = "unset LD_LIBRARY_PATH";
        in
        {
          default = pkgs.mkShell {
            name = "signal-sift";
            packages = rustTools ++ mediaTools;
            # rust-analyzer needs the std sources to resolve std items.
            RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
            inherit shellHook;
          };

          # Opt-in shell for benchmarks on the GPU. Both engines are built with
          # the Vulkan backend, which is what the Intel Arc B580 runs best
          # without pulling in the oneAPI/SYCL stack.
          inference = pkgs.mkShell {
            name = "signal-sift-inference";
            packages =
              rustTools
              ++ mediaTools
              ++ [
                self.packages.${pkgs.stdenv.hostPlatform.system}.whisper-cpp-vulkan
                self.packages.${pkgs.stdenv.hostPlatform.system}.llama-cpp-vulkan
                pkgs.vulkan-tools
              ];
            RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
            inherit shellHook;
          };
        }
      );
    };
}
