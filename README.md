# lightherder

A GPU video-feedback instrument: a software realization of an analog
video-feedback rig, where cameras are pointed at the monitors they are drawing
to. Rust + wgpu, no CPU pixel work in the loop.

A graph of monitors and cameras: a routing matrix mixes any camera onto any
monitor, beam splitters let one camera watch a blend of monitors, and each
monitor keeps its own colour controls. Each path carries its own analog
character — the lens's bloom, composite chroma bleed, grain — and each monitor
its own amplifier rail. Cameras can also be aimed at things that are not
monitors: test patterns, video files, capture devices. Any knob can be set
turning itself, and the whole panel saves to and recalls from eight slots.
Later increments add a browser build.

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
frames flattens on the CPU into a handful of *taps* — (source layer,
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

## Analog character

Four things a real rig does that a clean multiply does not, each hung where
the physics puts it and each per node rather than per instrument — which is
the point. One loop in a graph can glow and smear while the one beside it
stays sharp, and that is most of what makes two structures read as two.

Three of them belong to the camera's signal path. **Bloom** is the lens
scattering a fraction of the light into a halo instead of focusing it: a
redistribution, never an addition, because a term that adds light is a term
the loop multiplies. **Chroma bleed** is composite bandwidth — NTSC carries
colour on a subcarrier with a fraction of luma's bandwidth, so the colour
arrives smeared along the scanline while the detail it belongs to does not.
**Grain** is the sensor and the cable, monochrome and signed and present in
the dark, which is what keeps a loop that has decayed to black from staying
there.

The fourth belongs to the monitor: **headroom**, where its video amplifier
runs out of rails. Below half of it the signal is untouched; above, it bends
asymptotically onto it, the two arms meeting at the knee in both value and
slope. This is the difference between an analog feedback rig and a runaway
multiply — push the loop gain past unity with the rail wide open and the
middle of the spiral becomes a flat white disc, bring the rail down onto the
signal and the same overdrive compresses into a structure you can still see.

Bloom and bleed ride the taps that were already there, so no intermediate
texture appears. Their offsets are worked out on the CPU through each tap's
own affine: a halo is round in the *camera's* image and a bleed runs along
the *camera's* scanline, whatever angle or zoom that camera is watching from.
The bleed needs no matrix — NTSC's luma weights sum to one, so adding the
same amount to all three channels moves luma by exactly that and leaves the
subcarrier alone, and "this point's luma, the neighbourhood's colour" is one
dot product. A path with no character takes no extra samples at all.

None of it is on in the presets that shipped before it: a clean path and a
wide-open rail are an exact identity, guarded over a hundred passes for the
same reason the colour stage is. The `analog` preset is the single loop with
all four turned up.

## External inputs

A camera can be aimed at something that is not a monitor. An input is a
further layer of the same source bank, so `look` indexes the monitors and
then the inputs, and everything already built acts on it unchanged — the
switcher routes it, a beam splitter blends it with a monitor, the camera's
zoom and turn frame it, the lens blooms it. Nothing in the shader knows which
kind of source it sampled. What an input is *not* is part of the loop: no
camera draws to one, so it takes no routing column, and it is light entering
the graph rather than light going round it — the same role the seed spot has.

```toml
inputs = [
  { pattern = "bars" },                                # or "grid"
  { file = "clip.mp4" },                               # looped, at its own rate
  { capture = { format = "v4l2", device = "/dev/video0" } },
]
```

A file and a capture device are one implementation — an `ffmpeg` reading
something and writing raw RGBA down a pipe, scaled and letterboxed to the
monitor size — so anything ffmpeg can open is an input. That includes its own
generators (`{ format = "lavfi", device = "testsrc2" }`) and a screen
(`{ format = "x11grab", device = ":0.0" }`), which is why the drawn patterns
stop at two: no process, no thread, no decode, and levels a test can assert
without pinning an ffmpeg version. They are also still — motion is the
camera's job — so a pattern is uploaded once rather than every frame.

A source that has not produced its first frame by the time the window would
open is an error on the terminal, not a black layer, and one that ends
mid-performance says so in the log and leaves its last frame on the layer.
ffmpeg only has to be on `PATH` if a graph actually asks for a file or a
device; `shell.nix` has one.

Injection level is just the camera's gain, and near unity a little goes a
long way. The `external` preset hands over a hundredth of what its camera
sees, into a loop at 0.985 with the seed switched off: the trickle goes round
seventy times before it fades, so every photon on that monitor came in from
outside.

### The keyer

Each camera's path carries a keyer beside its gain and its character: what
this path refuses to hand on. Two keys that multiply, both off by default. The
**luma key** passes everything at or above its threshold and finishes cutting
one softness below it, so a subject in front of a dark room feeds the rig and
the room feeds nothing. The **chroma key** cuts the pixels leaning toward its
key colour — named as a hue, the way this instrument names every colour —
with a tolerance for how much of it a pixel may carry; at the top of its
travel the key is off, so grey and the far hues always pass. All four are
ordinary knobs: on the keys, mappable from a MIDI surface, saved in slots,
and as automatable as the rest of the panel. On the camera rather than on the
input, because that is where the gain and the framing already are — a camera
aimed at an input through its key is the webcam rig, and a key on a loop
camera is a gate on the feedback itself, which is its own instrument to play.

The `webcam` preset is `external` with the bars swapped for
`/dev/video0` and the luma key on: plug a camera in, recall, play.

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

## Automation

There is no separate kinetics. A camera on a motor is its rotation swept
through a full turn, which is a ramp at full depth on the rotation knob — so
continuous camera rotation and an LFO on any parameter are one mechanism, and
the instrument has one of it rather than two that drift apart. Any knob takes
one; several on one knob sum, which is how you get a beat.

```toml
motion = [
  { knob = "rotation", shape = "ramp", rate = 0.05, depth = 3.14159 },
  { knob = "hue", shape = "sine", rate = 0.2, depth = 0.4, phase = 0.25 },
  { knob = "pan x", rate = 0.1, depth = 0.3, focus = { camera = 1 } },
]
```

`knob` is the name the startup help prints, and that string *is* the knob's
serde, so there is no second spelling to get wrong — `zoom`, `rotation`,
`pan x`, `pan y`, `loop gain`, `loop gain, red`/`green`/`blue`, `bloom`,
`bloom radius`, `chroma bleed`, `noise`, `seed`, `hue`, `saturation`,
`brightness`, `contrast`, `gamma`, `headroom`. `rate` is in cycles per second
and stops at 30 Hz — half the sixty frames a second the instrument is drawn
at, since past Nyquist an LFO does not go faster, it goes somewhere else.
`depth` is half the swing in the knob's own units, capped
at the knob's own travel — or at a half turn for the knobs that wrap, where a
ramp then makes exactly one revolution per cycle. `phase` offsets one against
another: a quarter cycle apart on the two pan axes is a circle. `focus` picks
the node, and only the half the knob reads may be set, because a monitor knob
has no camera.

An LFO does not own its knob. It is an offset added to whatever the hand left
there, recomputed from the stored value every frame. So a swing cannot
compound, the keys keep working on a knob while it moves, switching one off
leaves its knob exactly where the hand did, and what a slot saves is the panel
rather than wherever the swing happened to be at the moment of writing. Every
offset goes through the same limit a key press does, so nothing an LFO does
can put a value somewhere a hand could not.

The `kinetic` preset is the single loop with one ramp on it, right round every
twenty seconds. Two details it earned on hardware: its base rotation is
`single`'s rather than zero, so switching the motor off leaves `single`'s
spiral rather than a camera parked square on — and its amplifier's rail sits
below white, because even *passing* through square-on the trail piles up on
the seed faster than it winds away. Measured on an RTX 2080: with the rail
wide open the flare reaches 255 as the sweep passes 0.05 rad, and 155 with the
rail at 0.9. Switching the motor off leaves the rail down; it is part of the
preset, not part of the motor.

## Preset slots

`f1` to `f8` recall; hold shift to store. A slot is a config file and nothing
else, written to `$XDG_CONFIG_HOME/lightherder/slot-N.toml`, read back through
the same loader the command line uses — so a saved performance opens in an
editor, keeps in a repository, or comes straight back as the graph the
instrument starts on. There is no second format for a saved state, because a
saved state is a graph.

A recall keeps the loops running: it changes the knobs the next pass reads,
not the light already on the glass. What it may not change is what would have
to be rebuilt to serve it — the monitor bank and the processes feeding the
inputs — so a slot with a different number of monitors, or different inputs,
is refused with the reason; start it from the command line with that file instead. Cameras,
routing, every knob and all the automation are free to differ.

## The control surface

The instrument is played from a **Korg nanoKONTROL2**, plugged in whenever —
before it starts or halfway through a piece. There is nothing to set up: while
there is no surface, the app reads `/dev/snd` once a second looking for one
whose card is named in `/proc/asound/cards`, and when the cable comes out the
read ends and it goes back to looking. ALSA raw MIDI and no library — a
controller is `/dev/snd/midiC<card>D0`, and reading it gives the wire bytes.

Out of the box, with no configuration:

| control | is |
|---|---|
| faders 1–8 | the focused **monitor**: seed, hue, saturation, brightness, contrast, gamma, headroom, and the crosspoint — how much of the focused camera it shows |
| rotaries 1–8 | the focused **camera**: zoom, rotation, pan x, pan y, loop gain, bloom, chroma bleed, noise |
| S 1–8 | focus camera 1–8 |
| M 1–8 | recall preset slot 1–8 |
| R 1–8 | store preset slot 1–8 |
| marker set, prev, next | next camera, next monitor, blank the monitors |
| ■ stop | reset every knob |
| ▶ | automation on the last control touched: off / sine / ramp |
| ◀◀ ▶▶ | its rate, slower and faster |
| ⏮ ⏭ | its swing, narrower and wider |
| ⟳ cycle | the on-screen controls overlay, on or off |

So the left hand works one monitor, the right hand one camera, and the top
fader is the switcher crosspoint joining the two the hands are on. A strip's
three buttons are the three things you do to what that strip stands for, in
order of how much they commit: Solo points the knobs at camera n, Mute plays
slot n back, Record writes over it. The first two markers step the focus the
way `n` and `m` do, which is how a graph deeper than eight cameras is reached
and the only way to change which monitor the faders are on. Nothing on the
surface is guarded: it has no shift, so a single press on the Mute row walks
over the live panel and one on the Record row walks over a slot. Nothing is
bound to quit: a slipped finger during a performance must not be able to stop
the instrument. Eight of the twenty-four knobs are not on the surface, which
has sixteen controls and no more: the three per-channel gain offsets, which
colour a rigid gain that is itself on a rotary; the bloom radius, which sizes
a halo whose amount is; and the keyer's four, which wait for a hand that keys
more than it bleeds and swaps this map for its own. They all stay on the keys.

**A fader does not take its knob over until it has passed through where the
knob already is.** A fader sends where it is standing, so without that,
plugging in mid-piece throws every knob to wherever its fader was left — with
the headroom fader slamming a monitor to white. Sweep the fader and it picks
its knob up on the way past, and from then on the fader is the knob. It lets
go on a recall, a reset and a change of focus — the three ways the panel moves
without a fader moving with it — and on an unplug, after which nothing knows
where a fader is standing.

The buttons are read on the way down, which assumes the surface's buttons are
**momentary** rather than latching — Korg's editor calls it Button Behavior.
A latching button plays on every second press.

### Mapping config

`$XDG_CONFIG_HOME/lightherder/midi.toml`, beside the preset slots. If it is not
there you get the layout above; if it is there and will not load, the
instrument says why and does not start.

```toml
# Matched case-insensitively against the card's line in /proc/asound/cards.
device = "nanoKONTROL"

[[fader]]
cc = 0
knob = "hue"        # any knob name the printed help uses

[[button]]
cc = 41
key = "p"           # any key label the printed help uses

[[button]]
cc = 64
key = "shift f1"
```

A fader names a **knob** and spans its whole travel — for the two knobs that
wrap, rotation and hue, that is one full revolution from bottom to top. A
button names a **key**, spelled the way the startup help spells it, and does
exactly what pressing that key does. Naming a key rather than an action of its
own is what keeps the surface from growing a second vocabulary beside the
keyboard's: everything a button can reach is on the help the instrument
already prints, and a binding added to the keys is playable from the panel the
same day.

Every channel is listened to, so a surface set to some other MIDI channel
still works. A control number may only be bound once, and a key label that no
key answers to is refused at load with the list of the ones that do.

## Run it

```
nix-shell --run "cargo run --release"                    # the single loop
nix-shell --run "cargo run --release analog"             # the same, with the signal path on
nix-shell --run "cargo run --release kinetic"            # the camera on a motor
nix-shell --run "cargo run --release crossed"            # two crossed structures
nix-shell --run "cargo run --release insanity"           # four, all-to-all
nix-shell --run "cargo run --release external"           # a test pattern driving the loop
nix-shell --run "cargo run --release webcam"             # /dev/video0 through the luma key
nix-shell --run "cargo run --release my-graph.toml"      # your own
```

It comes up covering the display, because an instrument on a stage is the only
thing on its screen. `f11` switches that off and on while it runs, which is how
you get at the rest of the machine without stopping the piece; `esc` quits, and
nothing on the factory MIDI map does — a slipped finger mid-performance must
not be able to stop the instrument.

```
nix-shell --run "cargo run --release -- --windowed crossed"
```

| | |
| --- | --- |
| `--windowed` | a window rather than the whole display |
| `--resolution 3840x2160` | how big every monitor is (default 1920x1080) |
| `--cheatsheet` | the controls — keys and surface both — and exit |
| `--bench` | what a frame costs, off screen, and exit |

Through `cargo run` they need the `--` above, which is cargo's and not this
program's; a built binary takes them directly.

The resolution is every monitor's size, and so the resolution the whole loop
runs at. The window's shape has nothing to do with it: the bank is tiled into
the window, each monitor letterboxed in its cell rather than stretched. Nor is
it part of a graph — every position here is in screen units and every weight a
ratio, so it changes how much detail the loop carries and — the grain aside,
which is hashed per texel and so is finer on a bigger monitor — nothing about
what it does. On a 4K display ask for `3840x2160` and what is on the glass is
the loop's own detail rather than an upscale of a smaller one.

A graph file is the same shape as the presets — `cargo run` on a bad file
prints exactly what is wrong. The smallest useful one:

```toml
cameras = [{ look = [1.0], framing = { zoom = 0.994, rotation = 0.05 } }]
monitors = [{ seed_brightness = 0.1 }]
routing = [[0.98]]
```

`look` is the camera's beam splitter — a weight per source, the monitors
first and then any `inputs`. `routing[m][c]` is how much of camera `c` monitor
`m` shows, and anything omitted — framing, gain, colour — is neutral.

The `shell.nix` pins nixpkgs, puts the Vulkan loader and windowing libraries
on `LD_LIBRARY_PATH`, which wgpu and winit open at run time, and carries the
`ffmpeg` the file and capture inputs run. Without Nix, a Rust toolchain recent
enough for wgpu 30 and winit 0.30 and a working Vulkan/Metal/DX12 driver will
do, plus ffmpeg if you want those inputs.

## Deploy

**The frame rate is a tempo, not a smoothness setting.** The loop evolves one
pass per frame — the camera pulls back 0.6% and turns 0.05 rad per *frame*, and
the trail decays per frame — so a spiral drawn in a second at sixty is drawn in
a twenty-fifth of one at fifteen hundred. The present mode is pinned to Fifo
for that reason rather than taken from the adapter, which offers something
faster and would be a different piece. The log prints the rate once a second:
deployed there is no terminal in front of the instrument, and a rate that has
left sixty is the first thing to know about a graph the machine cannot hold.

**When the display belongs to another user's session** — as it does on the
machine this was built for — the instrument runs as that user, who cannot read
the checkout. Stage the release binary and `shell.nix` where they can:

```
install -Dm755 target/release/lightherder /srv/lightherder/lightherder
install -Dm644 shell.nix                  /srv/lightherder/shell.nix
```

and start it through `nix-shell`, so the Vulkan loader and the windowing
libraries are the pinned ones it was built and tested against rather than a
list of paths copied out to go stale:

```
sudo -u USER env XDG_RUNTIME_DIR=/run/user/UID WAYLAND_DISPLAY=wayland-0 \
    DISPLAY=:0 HOME=/home/USER \
    nix-shell /srv/lightherder/shell.nix --run \
    "/srv/lightherder/lightherder --resolution 3840x2160 analog"
```

Its own log is how you know it worked, since nothing else on that machine can
see the screen — and it takes two lines, because they are two different things:
`1 monitors of 3840x2160` is the bank, and `window 3840x2160 (covering the
display), presenting Fifo at Rgba8UnormSrgb` is the window. A 4K window over a
1080p bank prints the second and not the first. Then sixty a second, from the
first frame rather than from before the pipelines were built.

### What a frame costs

On a display the loop runs at the vertical blank whatever it costs, so a window
reporting sixty says only that a frame fit — not by how much. `--bench` runs
the same passes with nothing pacing them: 600 frames after a warm-up, the graph
stepped and presented into a target the size of the display.

| graph | 1920x1080 | 3840x2160 |
| --- | --- | --- |
| `single` | 0.16 ms | 0.38 ms |
| `kinetic` | 0.14 | 0.42 |
| `external` | 0.16 | 0.45 |
| `analog` | 0.22 | 0.72 |
| `crossed` (2 monitors) | 0.23 | 0.72 |
| `insanity` (4 monitors, all-to-all) | 0.64 | 2.14 |

A 60 Hz frame is 16.7 ms, so the heaviest graph that ships uses an eighth of one
at 4K. Measured on an RTX 2080. What the numbers leave out is a frame's edges
rather than its loop: handing the frame to the compositor, and the upload of a
live input, which for a video file or a capture device is a conversion and two
writes of a whole frame every frame. The graphs above have none — a drawn
pattern is uploaded once.

The bank itself is what grows: two copies of every monitor and input at eight
bytes a texel, half a gigabyte for `insanity` at 4K, and refused past two.

## Keys

The binding list prints on startup, together with the control surface under
whatever map is in force; `--cheatsheet` prints the same card without starting
anything. On the glass, `` ` `` — or the surface's cycle button — toggles a
controls overlay in the bottom-right corner: the panel drawn as it is
actually mapped, each control captioned in a couple of words. Every knob logs
its new value on change. Keys are physical positions, so the punctuation
below assumes a US layout.

| key | effect |
| --- | --- |
| `-` `=` | zoom out / in, per pass |
| `,` `.` | rotation, per pass |
| arrows | pan x, pan y |
| `[` `]` | loop gain, all channels at once |
| `1`…`6` | loop gain, red / green / blue (down, up each) |
| `g` `h` | bloom, i.e. how much the lens scatters |
| `j` `k` | bloom radius |
| `y` `u` | chroma bleed |
| `i` `o` | noise, i.e. the grain |
| `b` `l` | key threshold: the luma the path passes in full |
| `f9` `f10` | key softness, both keys' soft edge |
| `home` `end` | key hue: the colour the chroma key cuts |
| `pgdn` `pgup` | key tolerance; at the top, the chroma key is off |
| `;` `'` | seed |
| `a` `s` | hue, per pass |
| `d` `f` | saturation |
| `z` `x` | brightness, i.e. black level |
| `c` `v` | contrast |
| `q` `w` | gamma |
| `e` `t` | headroom, i.e. where the amplifier's rails are |
| `/` `\` | the crosspoint: how much of the focused camera the focused monitor shows |
| `p` | automation on the last knob turned: off / sine / ramp |
| `7` `8` | its rate, slower / faster |
| `9` `0` | its swing, narrower / wider |
| `n` | focus the next camera |
| `num1`…`num8` | focus camera 1–8 outright |
| `m` | focus the next monitor |
| `f1`…`f8` | recall preset slot |
| shift `f1`…`f8` | store preset slot |
| space | blank every monitor |
| `r` | reset every knob |
| `f11` | cover the display, or stop covering it |
| `` ` `` | the controls overlay, on or off |
| esc | quit |

The knobs act on the focused camera (framing, gain and character) and the
focused monitor (seed, colour and headroom); `n` and `m` walk the two focuses
through the graph and `num1`…`num8` pick a camera outright, and the log line
names them. Those eight are keypad keys, so on a board with no keypad `n` is
the only way to the cameras. Routing and splitter weights
are config; the crosspoint that joins the two focused nodes is not — `/` and
`\` sweep it, and it is fader 8 on the surface.

`p` acts on the last knob turned rather than on a switch of its own, because
twenty knobs would otherwise want twenty switches and the knob just swept
is the one you mean; the log line names which. Shift is read by the slot keys
and nothing else — recall is one press, store is the press you have to mean.
Both are irreversible, since a recall walks over a live panel nothing has
stored; the modifier is there because a hand mid-piece reaches for a slot far
more often than it writes one.

The colour and character knobs start neutral, so the instrument out of the box
is the loop described above and nothing else. Turn one against it: `s` held
down is the quickest way to see what a stage inside the loop does, and `h`
held down is the quickest way to see what the loop does to a stage — a lens
that scatters a tenth of the light per pass has spread it everywhere by the
tenth pass.

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
the shipped presets settle without clipping. So does the character stage: the
lens widens the spot without changing how much light is in the frame, the
bleed carries colour sideways while leaving luma where it was, the grain
differs frame to frame and arrives on an unlit monitor, the rail bends a peak
onto the curve it claims while leaving everything under its knee alone, and
two paths in one graph take their character separately. External inputs get
the same: what was written to an input's layer is what a camera aimed at it
sees, it is current in whichever bank the cameras read, blanking the monitors
leaves it alone, and a splitter and a zoom act on it exactly as on a monitor.
The automation is checked where it ends up mattering: a full revolution of
`kinetic` sampled the whole way round, holding an image at every angle it
passes through and still moving a revolution in — frame against frame, since
a rotation moves light around without making any, so the totals barely budge.
On a machine with no adapter each one prints the reason straight to the
process's stderr and returns; libtest still counts them as passed.

The input decoding is tested without a GPU: the drawn patterns against the
colours and spacings they name, the ffmpeg command lines against the options
that make them loop and letterbox rather than race and stretch, a capture
source against what ffmpeg was told to generate, a real file written and
decoded, and a file that is not there refused at once rather than after the
first-frame timeout. Outside the pinned shell the ones that need ffmpeg print
a skip, on the same terms as the GPU tests.

So is the automation's arithmetic: a sine that averages to nothing over its
cycle and a ramp that climbs to exactly one swing, a full-depth ramp on the
rotation knob covering one revolution in even steps and closing, two LFOs a
quarter cycle apart tracing a circle, a knob whose stored value has not moved
after ten thousand modulated frames, and every knob in the instrument driven
at its widest swing without putting the graph anywhere `validate` refuses.
The slots round-trip every preset through a real file, and a slot the
instrument never wrote is validated like any other config.

The surface is tested without one plugged in, at every layer and then through
all of them at once. The decoder against the ways a fader sweep actually
arrives — three bytes split across reads, running status with the status byte
left off, a clock byte landing between a control number and its value, a scene
dump that must not read as a hundred knob moves, and notes and bends that are
not knobs. The pickup against a fader that has to reach its knob before it
moves it, one already standing on it, and one that loses its grip on an
unplug. The map against a duplicate binding, a key nothing answers to, and a
literal file rather than a round trip, because a round trip agrees with itself
whatever the keys are called. The card search against a `/proc/asound/cards`
with two other cards that also have raw MIDI devices. And the whole path —
discovery, the open, the reader thread, the decode, the map and the pickup —
against a device that is not there when the instrument starts, appears, sends
a sweep down a pipe, and goes away again, which is what hot-plug is.

## In a browser

The same instrument, on WebGPU, at
<https://bddap-bot.github.io/lightherder/> — the graph is chosen the way a
page takes an argument, `?preset=insanity`, and the keys are the keys. What
is not there is what a browser has no way to give it: the ALSA control
surface, an ffmpeg input, and the preset slots, which are files.

`web/build.sh` builds `web/dist` — the module, its glue and the page — and
every push to `main` runs it and publishes the result. Locally:

    nix-shell --run ./web/build.sh
    python3 -m http.server -d web/dist

The `wasm-bindgen` crate is pinned to the patch because the generator of that
glue must be the same version; the dev shell carries that exact binary and
`web/build.sh` refuses anything else.
