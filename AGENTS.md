# Landing contract

Every push to `main` has two deploy targets, and a landing is complete only when both hold.
This binds EVERY landing, test-only and doc-only changes included: the TV binary must equal
the release build of HEAD, not merely behave like it. Both or neither — a lagging TV binary
is an unfinished landing, not a done one.

1. The TV binary equals `cargo build --release` of `main` HEAD (same sha256). Its install
   path belongs to the landing machine: `scripts/landing` reads it from
   `${XDG_CONFIG_HOME:-~/.config}/lightherder/tv-binary`, one absolute path, and refuses to
   run without it. `scripts/landing deploy` installs the binary by rename swap and launches
   nothing; the TV picks it up on its next start.
2. The `pages` workflow is green on that same sha. It runs on push; if it is red, say so
   rather than re-triggering blindly.
3. No code comments. Prose lives here or in the README.

`scripts/landing check` proves 1 and 2; it is the `landing` entry of `test-map.json`, which
the landing gate runs before it accepts a change.

## Boundaries

This lightherder repository names only its own components. Name another project only as a declared, versioned dependency, never through its internals. Give a needed shared service a neutral name owned by this project. Do not import the environment of machines running agents: hostnames, addresses, paths outside the repository, service or queue names, credentials, camera frames, or renders of private places. No person's name, schedule or presence enters the repository. Before landing, grep the diff for other projects' names and host details. Remove host details and undeclared project references; dependency declarations expose only the dependency's name and version.

## Build and test details

Use `nix-shell --run 'cargo test'` for the full native suite. The shell supplies
ffmpeg and the dynamically loaded Vulkan/windowing libraries; GPU tests report
`SKIPPED` when no adapter is available, and ffmpeg-dependent tests can skip when
ffmpeg is absent. Check the output as well as the exit code.

For a display connected to an integrated GPU, `WGPU_POWER_PREF=low` selects the
low-power adapter if the default adapter cannot present to that display.

`nix-shell --run './web/build.sh'` builds the browser module into `web/dist`.
The wasm-bindgen CLI must match the exact crate version in `Cargo.toml`;
`shell.nix` supplies it and `web/build.sh` checks the match.

For browser picture verification, run Chromium headed under `xvfb-run -a -s
'-screen 0 1920x1080x24'`, set `VK_ICD_FILENAMES` to the GPU vendor's Vulkan ICD,
and pass `--enable-features=Vulkan,VulkanFromANGLE,DefaultANGLEVulkan
--use-angle=vulkan --ignore-gpu-blocklist`. Headless Chromium can fall back to
SwiftShader and capture none of the WebGPU bank (white screenshots or black
canvas copies). Confirm a non-fallback adapter and view every captured frame
before citing it; use a synthetic camera seed and label synthetic MIDI input.
