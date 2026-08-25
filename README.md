# lightherder

A GPU video-feedback instrument: a software realization of an analog
video-feedback rig, where cameras are pointed at the monitors they are drawing
to. Rust + wgpu, no CPU pixel work in the loop.

This is the first increment — one monitor, one camera. Later increments add
analog colour control, a graph of many monitors and cameras with a routing
matrix between them, MIDI control, and a browser build.

## How it works

A "monitor" is an offscreen `Rgba16Float` texture. A "camera" is a fullscreen
pass that samples that texture through an affine transform — zoom, rotation,
pan — multiplies by a per-channel gain and writes the result back. That output
is the next frame's input, which is the whole trick: pull the camera back a
little each pass and the image walks inward, turn it a little and it spirals.

A soft spot seeds the loop, since a loop with gain below 1.0 and nothing
feeding it decays to black. The spot sits off-centre on purpose: a radially
symmetric spot in the middle is a fixed point of rotation, so a centred seed
would leave the rotation knob with nothing visible to do. Half-float keeps
headroom above 1.0 so dozens of passes do not quantise into bands. Samples
that fall outside the monitor read as black rather than a smeared edge,
because a real camera aimed past the monitor sees an unlit room.

Out of the box the default knobs settle into a spiral: the camera pulls back
0.6% and turns 0.05 radians per pass, at a gain just under unity that is
spread across the channels, so the trail cools from white to blue as it winds
in.

## Run it

```
nix-shell --run "cargo run --release"
```

The `shell.nix` pins nixpkgs and puts the Vulkan loader and windowing
libraries on `LD_LIBRARY_PATH`, which wgpu and winit open at run time. Without
Nix, a Rust toolchain recent enough for wgpu 30 and winit 0.30 and a working
Vulkan/Metal/DX12 driver will do.

The window can be any shape: the monitor is a fixed 1920x1080 regardless, and
is letterboxed into the window rather than stretched.

## Keys

The binding list prints on startup, and every knob logs its new value on
change. Keys are physical positions, so the punctuation below assumes a US
layout.

| key | effect |
| --- | --- |
| `-` `=` | zoom out / in, per pass |
| `,` `.` | rotate, per pass |
| arrows | pan |
| `[` `]` | loop gain, all channels at once |
| `1`…`6` | loop gain per channel (r-, r+, g-, g+, b-, b+) |
| `;` `'` | seed brightness |
| space | blank the monitor |
| `r` | reset every knob |
| esc | quit |

Zoom and gain are the sensitive ones. A few thousandths either side of
`zoom 1.000` is the difference between an image that walks inward, one that
stands still, and one that blows outward. A gain over 1.0 stops the trail
decaying and blows the head of the spiral out into a hard white disc — a
couple of thousandths over takes a few seconds to get there, further over is
immediate — and the structure inside it is gone.

## Tests

```
nix-shell --run "cargo test"
```

The transform, parameter and letterbox tests are pure. The tests in `tests/`
render on a real GPU and read the pixels back, checking that the seed lights
the monitor where it says it does, that the previous frame comes back round,
that a pan moves the image the way the knob says, that the seed stays round on
a non-square monitor, and that the default knobs settle without clipping. On a
machine with no adapter each one prints the reason straight to the process's
stderr and returns; libtest still counts them as passed.
