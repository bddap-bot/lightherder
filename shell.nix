# Self-contained dev shell: `nix-shell --run "cargo build"`.
# nixpkgs is pinned by hash so the shell is reproducible without channels
# ($NIX_PATH is empty on some hosts, so `nix-shell -p` is not an option).
let
  pkgs = import (fetchTarball {
    url = "https://github.com/NixOS/nixpkgs/archive/d6c71932130818840fc8fe9509cf50be8c64634f.tar.gz";
    sha256 = "1klgyhj98j3gfsql5sn9rapyx62qk5g8adk5zh9mnc4d0fj61gdr";
  }) { };
in
pkgs.mkShell {
  buildInputs = with pkgs; [
    cargo
    rustc
    clippy
    rustfmt

    pkg-config

    vulkan-loader
    vulkan-headers
    vulkan-validation-layers
    mesa # lavapipe software Vulkan ICD, for machines with no usable GPU

    libxkbcommon
    wayland
    libx11
    libxcursor
    libxi
    libxrandr
  ];

  # wgpu and winit dlopen these at runtime, so they must be found by the loader
  # rather than linked at build time.
  LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath (with pkgs; [
    vulkan-loader
    libxkbcommon
    wayland
    libx11
    libxcursor
    libxi
    libxrandr
  ]);
}
