# Landing contract

Edit by subtraction: resolve a problem by deleting code; a tactical patch over a symptom is not accepted. One implementation per thing, never two alive.

Delete code comments; keep only a why the code cannot show.

Every landing, including prose and tests, has two deployment requirements:

1. The installed TV binary has the same SHA-256 as `cargo build --release` of
   `main` HEAD. Configure its destination as described by
   [scripts/landing](scripts/landing). `scripts/landing deploy` installs by rename
   swap without launching; the TV picks it up on its next start.
2. The `pages` workflow succeeds on that commit. Report a failure before retrying.

`scripts/landing check`, the landing entry in [test-map.json](test-map.json),
checks both requirements.

## Boundaries

Keep this project independent. Reference other projects only as declared, versioned
dependencies, exposing names and versions rather than internals. Give shared services
neutral project-owned names. Exclude deployment-specific paths, addresses, service
or queue names, credentials, camera frames and private renders. Before landing,
inspect the diff for undeclared project references and deployment details.

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
