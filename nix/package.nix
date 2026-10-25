{
  lib,
  rustPlatform,
  makeWrapper,
  fontconfig,
  libnotify,
  curl,
  xdg-utils,
}:

let
  cargoToml = lib.importTOML ../Cargo.toml;
in
rustPlatform.buildRustPackage {
  pname = cargoToml.package.name;
  version = cargoToml.package.version;

  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../src
    ];
  };

  cargoLock.lockFile = ../Cargo.lock;

  nativeBuildInputs = [ makeWrapper ];

  # Pure Rust: no native libraries are linked. The tools below are only used at runtime
  # (font lookup and the default notify/upload/open commands); they are appended to PATH
  # so the user's own versions take precedence.
  postInstall = ''
    wrapProgram $out/bin/mtsw-shot \
      --suffix PATH : ${
        lib.makeBinPath [
          fontconfig
          libnotify
          curl
          xdg-utils
        ]
      }
  '';

  meta = {
    description = cargoToml.package.description;
    homepage = "https://github.com/matswuuu/mtsw-shot";
    license = lib.licenses.mit;
    mainProgram = "mtsw-shot";
    platforms = lib.platforms.linux;
  };
}
