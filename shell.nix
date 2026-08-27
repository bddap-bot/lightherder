# Self-contained dev shell: `nix-shell --run "cargo build"`.
# nixpkgs is pinned by hash so the shell is reproducible without channels
# ($NIX_PATH is empty on some hosts, so `nix-shell -p` is not an option).
let
  pkgs = import (fetchTarball {
    url = "https://github.com/NixOS/nixpkgs/archive/d6c71932130818840fc8fe9509cf50be8c64634f.tar.gz";
    sha256 = "1klgyhj98j3gfsql5sn9rapyx62qk5g8adk5zh9mnc4d0fj61gdr";
  }) { };

  # wgpu and winit dlopen these at run time; nothing links them at build time,
  # so they belong on the library path and not in buildInputs.
  runtimeLibs = with pkgs; [
    vulkan-loader
    libxkbcommon
    wayland
    libx11
    libxcursor
    libxi
    libxrandr
  ];

  # The generator of the JavaScript that stands between the page and the
  # module. It must be the *exact* version of the `wasm-bindgen` crate the
  # module was compiled against — Cargo.toml pins the crate for this reason
  # and the tool refuses a mismatch rather than emitting something subtly
  # wrong. This nixpkgs carries 0.2.108, which predates the typed `Gpu*`
  # bindings wgpu 30's WebGPU backend compiles against, so the released binary
  # is taken directly: a plain fixed-output fetch, and the same artefact CI
  # installs.
  wasm-bindgen-cli = pkgs.stdenvNoCC.mkDerivation rec {
    pname = "wasm-bindgen-cli";
    version = "0.2.127";
    src = pkgs.fetchurl {
      url = "https://github.com/wasm-bindgen/wasm-bindgen/releases/download/${version}/wasm-bindgen-${version}-x86_64-unknown-linux-musl.tar.gz";
      hash = "sha256-YdSn3IWs+g0jVMzAuDYZKMflKnRtF/KOuqeV7T3BYUo=";
    };
    installPhase = "install -Dm755 wasm-bindgen $out/bin/wasm-bindgen";
  };
in
pkgs.mkShell {
  buildInputs = with pkgs; [
    cargo
    rustc
    clippy
    rustfmt
    # test-map.json's web/** rule lints web/build.sh with this.
    shellcheck
    # An external input that is a file or a device is an ffmpeg reading it,
    # and two of the tests run one.
    ffmpeg
    # wasm32 has no system linker to fall back on.
    lld
    wasm-bindgen-cli
  ];

  LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath runtimeLibs;
}
