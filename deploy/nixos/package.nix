{ lib, rustPlatform }:

let
  root = ../..;
in
rustPlatform.buildRustPackage {
  pname = "signalsift";
  version = (lib.importTOML (root + "/Cargo.toml")).workspace.package.version;

  # Only what the build reads, so editing docs or deploy files does not
  # invalidate the package hash.
  src = lib.fileset.toSource {
    inherit root;
    fileset = lib.fileset.unions [
      (root + "/Cargo.toml")
      (root + "/Cargo.lock")
      (root + "/apps")
      (root + "/crates")
      (root + "/config/signalsift.example.toml") # embedded by a config test
    ];
  };

  cargoLock.lockFile = root + "/Cargo.lock";
  cargoBuildFlags = [
    "--package"
    "signalsift"
  ];

  meta = {
    description = "Local-first content intelligence daemon";
    homepage = "https://github.com/v1cferr/signal-sift";
    license = with lib.licenses; [
      mit
      asl20
    ];
    mainProgram = "signalsift";
    platforms = lib.platforms.linux;
  };
}
