# lightherder

A GPU video-feedback instrument: a software realization of an analog
video-feedback rig, where cameras are pointed at the monitors they are drawing
to. Rust + wgpu, no CPU pixel work in the loop.

This is the first increment — one monitor, one camera. Later increments add
analog colour control, a graph of many monitors and cameras with a routing
matrix between them, and MIDI control.

## How it works

A "monitor" is an offscreen `Rgba16Float` texture. A "camera" is a fullscreen
pass that samples that texture through an affine transform — zoom, rotation,
pan — multiplies by a per-channel gain and writes the result back to the
monitor. That output is the next frame's input, which is the whole trick:
magnify slightly per pass and the image tunnels inward, turn slightly and it
spirals.

A soft spot in the middle seeds the loop, since a loop with gain below 1.0 and
nothing feeding it decays to black. Half-float keeps headroom above 1.0 so
dozens of passes do not quantise into bands. Samples that fall outside the
monitor read as black rather than a smeared edge, because a real camera aimed
past the monitor sees an unlit room.

## Run it

```
nix-shell --run "cargo run --release"
```

The `shell.nix` pins nixpkgs and puts the Vulkan loader and windowing libraries
on `LD_LIBRARY_PATH`, which wgpu and winit open at runtime. Without Nix, any
Rust toolchain and a working Vulkan/Metal/DX12 driver will do.

## Keys

Parameters print to the log on every change.

| key | effect |
| --- | --- |
| `-` `=` | zoom out / in, per pass |
| `,` `.` | rotate, per pass |
| arrows | pan |
| `[` `]` | loop gain, all channels |
| `1`…`6` | loop gain per channel (r-, r+, g-, g+, b-, b+) |
| `;` `'` | seed brightness |
| space | blank the monitor |
| `r` | reset |
| esc | quit |

Zoom and rotation are the sensitive ones: a few thousandths either side of
`zoom 1.000` is the difference between an image that collapses to a point, one
that stands still, and one that blows outward.

## Tests

```
nix-shell --run "cargo test"
```

The transform and parameter tests are pure. The tests in `tests/` render on a
real GPU and read the pixels back, checking that the seed lights the monitor,
that the previous frame comes back round, and that the knobs reach the shader.
They skip loudly on a machine with no adapter.
