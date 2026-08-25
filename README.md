# lightherder

A GPU video-feedback instrument: a software realization of an analog
video-feedback rig, where cameras are pointed at the monitors they are drawing
to. Rust + wgpu, no CPU pixel work in the loop.

A graph of monitors and cameras: a routing matrix mixes any camera onto any
monitor, beam splitters let one camera watch a blend of monitors, and each
monitor keeps its own colour controls. Later increments add analog character,
external inputs, kinetics, MIDI control, and a browser build.

## How it works

A "monitor" is one layer of an offscreen `Rgba16Float` texture array. A
"camera" is a fullscreen pass that samples a layer through an affine
transform — zoom, rotation, pan — multiplies by a per-channel gain and writes
the result back. That output is the next frame's input, which is the whole
trick: pull the camera back a little each pass and the image walks inward,
turn it a little and it spirals.

The wiring between them is a graph. A routing matrix says how much of each
camera every monitor displays, and each camera's beam splitter says how much
of each monitor it sees. Both are just weights, and sampling is linear, so
the whole path from a monitor's next frame back to the bank of previous
frames flattens on the CPU into a handful of *taps* — (source monitor,
sampling transform, weight) — and each monitor is one render pass summing
its taps. There is no intermediate blend texture because none is needed. All
monitors step from the same previous frames, the simultaneous capture a rig
of real cameras performs, and the window shows the whole bank tiled in a
grid.

The graph comes from a preset or a TOML file. `crossed` is the classic
two-structure rig — each camera watches its own monitor through
beam-splitter glass that lets a quarter of the other bleed in, and the
switcher routes every camera to the opposite monitor, so each image is made
of its twin's past. `insanity` is four monitors all-to-all: every monitor a
quarter of every camera. One shape worth knowing before writing your own:
rotations in a mixed loop should all turn the same way, since paths whose
rotations cancel never wind away from the seed, and light that cannot leave
the seed spot piles up on it until the display clips.

Before that output is written, it passes the monitor's own front panel: the
chroma decode, the video amplifier and the phosphor, in that order. The decode
works in NTSC luma/chroma rather than RGB, which is what makes hue a *phase* —
the two chroma axes are the real and imaginary parts of one subcarrier, so hue
turns it and saturation scales it, and luma comes out untouched. Decode, turn
and encode compose into one 3x3, which the CPU works out once a frame: chained
per fragment instead they leave a ten-thousandth of the signal behind on every
pass, and a loop that feeds itself turns that into a colour cast. Then contrast
about mid-grey, brightness as a lift, and a power curve for the phosphor. All
of it is inside the loop, so every knob compounds once per pass: a few
hundredths of a radian of hue walks the trail through the spectrum, a gamma
above 1 crushes the dark end and thins it out — far enough and it takes the
seed with it, leaving a black monitor that looks like a loop that has died —
and a brightness above zero lifts the whole frame and floods it.

Contrast pivots about mid-grey rather than about black on purpose. A gain
about black is exactly what the loop gain already is, and the front panel is
not the place for a second one.

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
nix-shell --run "cargo run --release"                    # the single loop
nix-shell --run "cargo run --release crossed"            # two crossed structures
nix-shell --run "cargo run --release insanity"           # four, all-to-all
nix-shell --run "cargo run --release my-graph.toml"      # your own
```

A graph file is the same shape as the presets — `cargo run` on a bad file
prints exactly what is wrong. The smallest useful one:

```toml
cameras = [{ look = [1.0], framing = { zoom = 0.994, rotation = 0.05 } }]
monitors = [{ seed_brightness = 0.1 }]
routing = [[0.98]]
```

`look` is the camera's beam splitter (a weight per monitor), `routing[m][c]`
is how much of camera `c` monitor `m` shows, and anything omitted — framing,
gain, colour — is neutral.

The `shell.nix` pins nixpkgs and puts the Vulkan loader and windowing
libraries on `LD_LIBRARY_PATH`, which wgpu and winit open at run time. Without
Nix, a Rust toolchain recent enough for wgpu 30 and winit 0.30 and a working
Vulkan/Metal/DX12 driver will do.

The window can be any shape: every monitor is a fixed 1920x1080 regardless,
and the bank is tiled into a grid, each monitor letterboxed in its cell
rather than stretched.

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
| `a` `s` | hue, per pass |
| `d` `f` | saturation |
| `z` `x` | brightness, i.e. black level |
| `c` `v` | contrast |
| `q` `w` | gamma |
| `n` | focus the next camera |
| `m` | focus the next monitor |
| space | blank every monitor |
| `r` | reset every knob |
| esc | quit |

The knobs act on the focused camera (framing and gain) and the focused
monitor (seed and colour); `n` and `m` walk the two focuses through the
graph, and the log line names them. Routing and splitter weights are config,
not knobs — the MIDI increment puts them under fingers.

The colour knobs start neutral, so the instrument out of the box is the loop
described above and nothing else. Turn one against it: `s` held down is the
quickest way to see what a stage inside the loop does.

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
a non-square monitor, that the default knobs settle without clipping, and that
each colour knob does its own job — saturation greys without dimming, hue
moves light between the channels at constant luma, contrast leaves mid-grey
where it is while a gain would not, brightness lifts black itself, and gamma
bends the response instead of scaling it. The graph gets the same treatment:
a seed sent across the crossed wiring bounces between the monitors without
leaving a copy behind, mix weights deliver exactly the fraction they name, a
beam splitter delivers light from a monitor its routing row never touches,
insanity mode puts a quarter of one seed on all four monitors at once, and
the shipped presets settle without clipping. On a
machine with no adapter each one prints the reason straight to the process's
stderr and returns; libtest still counts them as passed.
