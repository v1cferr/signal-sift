{
  lib,
  rustPlatform,
  cacert,
}:

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
      (root + "/contracts") # OpenAPI document, checked against the router by a test
      (root + "/tests/fixtures") # synthetic feeds embedded by parser tests
    ];
  };

  cargoLock.lockFile = root + "/Cargo.lock";

  # The HTTP client (rustls platform verifier) refuses to start without CA
  # certificates, and the build sandbox has none. At runtime the host's
  # /etc/ssl/certs is used; tests get the nixpkgs bundle.
  nativeCheckInputs = [ cacert ];
  preCheck = ''
    export SSL_CERT_FILE=${cacert}/etc/ssl/certs/ca-bundle.crt
  '';
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
