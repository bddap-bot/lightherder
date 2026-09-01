# lightherder

A GPU video-feedback instrument: a software realization of an analog
video-feedback rig, where cameras are pointed at the monitors they are drawing
to. Rust + wgpu, no CPU pixel work in the loop.

A graph of monitors and cameras: a routing matrix mixes any camera onto any
monitor, beam splitters let one camera watch a blend of monitors, and each
monitor keeps its own colour controls. Each path carries its own analog
character — the lens's bloom, composite chroma bleed, grain — and each monitor
its own amplifier rail. Every camera watches monitors and only monitors, so
every path in the graph is a loop; light from outside — test patterns, video
files, capture devices — enters where a real rig's does, on the switcher.
The instrument writes nothing down: the graph comes from the command line and
the panel lives as long as the run, the way the hardware it is modelled on
does.

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
grid — or one monitor of it on the whole display, which is the same tiling
with one tile in it.

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

A camera watches monitors. That is the instrument: a camera on a stand in a
room of monitors sees the light already going round, so every path in the
graph closes and there is no such thing as an aimable source. Light that the
graph did not make plugs into the **switcher** instead, beside the cameras,
which is where a real rig plugs it in.

An input is a further layer of the same source bank, and the switcher has a
second half addressed to those layers — `routing[m][c]` weights the cameras
onto monitor `m`, `routing_inputs[i][m]` weights input `i` onto the same
monitors. Two lists, each counted against its own kind, so a camera added to
a graph cannot silently take over an input's level: a list that no longer
matches its own kind is a refusal rather than a shift. Nothing in the shader
knows which kind of layer it sampled, and a monitor sums the two halves
without being told which was which.

What an input is *not* is part of the loop: no camera draws to one and none
watches one, so it is light entering the graph rather than light going round
it — the same role a white blob has. It arrives square on and whole, because
there is no lens between the switcher and it: framing, gain, bloom and the
keyer are a camera's, and an input passes none.

```toml
inputs = [
  { pattern = "bars" },
  { file = "clip.mp4" },                               # looped, at its own rate
  { capture = { format = "v4l2", device = "/dev/video0" } },
]
cameras = [{ look = [1.0] }]                           # watches the monitor, as every camera does
routing = [[0.98]]                                     # …and the switcher sends it back
routing_inputs = [[0.0], [0.5], [0.0]]                 # the clip, onto that monitor, at half
```

A file and a capture device are one implementation — an `ffmpeg` reading
something and writing raw RGBA down a pipe, scaled and letterboxed to the
monitor size — so anything ffmpeg can open is an input. That includes its own
generators (`{ format = "lavfi", device = "testsrc2" }`) and a screen
(`{ format = "x11grab", device = ":0.0" }`), which is why there is exactly one
drawn pattern: bars earn being drawn with levels a test can assert without
pinning an ffmpeg version, and geometry ffmpeg can draw itself. A pattern is
also still — motion is the camera's job — so it is uploaded once rather than
every frame.

A source that has not produced its first frame by the time the window would
open is an error on the terminal, not a black layer, and one that ends
mid-performance says so in the log and leaves its last frame on the layer.
ffmpeg only has to be on `PATH` if a graph actually asks for a file or a
device; `shell.nix` has one.

A graph file names what ffmpeg opens, so running someone else's is trusting
it with that much. A `file` may only be a path — the file protocol is forced,
so an `http://` URL is refused rather than fetched for as long as the
instrument runs — but a `capture` is a format and a device string handed
straight to ffmpeg, and `{ format = "lavfi", device = "movie=/private.mp4" }`
puts a local file on screen. Read a graph before you play it.

Injection level is the crosspoint the input is sent on — a knob like the
other crosspoint, on `9` and `0` against the focused monitor. Near unity a
little goes a long way: the
`external` preset sends a seventieth of the bars — 0.014 — onto a monitor
whose loop runs at 0.985 and whose glass is dark, so the trickle goes round
seventy times before it fades and every photon on that monitor came in from
outside. Turn it up mid-performance and the outside floods in; turn it to
zero and the loop keeps running on what it has.

### The keyer

Each camera's path carries a keyer beside its gain and its character: what
this path refuses to hand on. Two keys that multiply, both off by default. The
**luma key** passes everything at or above its threshold and finishes cutting
one softness below it, so a subject in front of a dark room feeds the rig and
the room feeds nothing. The **chroma key** cuts the pixels leaning toward its
key colour — named as a hue, the way this instrument names every colour —
with a tolerance for how much of it a pixel may carry; at the top of its
travel the key is off, so grey and the far hues always pass. All four are
ordinary knobs, set in the graph file and mappable from a MIDI surface. On the camera
because that is the only signal path the instrument has — the gain, the
framing and the character are all there, and what the
switcher hands a monitor from outside it hands over whole. A camera watches
monitors, so a key is a gate between one monitor and the next: on a loop's own
camera it gates the feedback, refusing the dark of a trail or one hue of it a
trip round.

Lifting a subject off its room is the same gate, one monitor earlier. The
`webcam` preset is two: the switcher puts `/dev/video0` whole on the first
monitor — a **window** on the room, with no camera routed to it and so no
loop of its own — and a camera watches *that* through its luma key, handing
the lit subject on and refusing the unlit room behind it. What survives
drives the second monitor's loop, which is `external`'s. Plug a camera in and
play. The key has to sit one monitor upstream because a key on the loop's own
camera would gate the trail it is building, so there would never
be a trail.

A monitor's **seed** is what lights its loop from outside: either a soft
white blob on the glass, or nothing of its own — dark glass, holding only
what the switcher paints on it. One or the other, not a level with an off
value, because the two are different rigs and the dark rig's level is already
played on the switcher. `;` swaps them, PLAY does on the surface, and the
button is
lit while the focused monitor has its blob. A blob is what starts a loop with
gain below 1.0, which decays to black with nothing feeding it. The spot sits
off-centre on purpose: a radially symmetric spot in the middle is a fixed
point of rotation, so a centred one would leave the rotation knob with
nothing visible to do. Half-float keeps
headroom above 1.0 so dozens of passes do not quantise into bands. Samples
that fall outside the monitor read as black rather than a smeared edge,
because a real camera aimed past the monitor sees an unlit room.

Out of the box the default knobs settle into a spiral: the camera pulls back
0.6% and turns 0.05 radians per pass, at a gain just under unity that is
spread across the channels, so the trail cools from white to blue as it winds
in.

## The control surface

The instrument is played from a **Korg nanoKONTROL2**, plugged in whenever —
before it starts or halfway through a piece. There is nothing to set up: while
there is no surface, the app reads `/dev/snd` once a second looking for one
whose card is named in `/proc/asound/cards`, and when the cable comes out the
read ends and it goes back to looking. ALSA raw MIDI and no library — a
controller is `/dev/snd/midiC<card>D0`, and reading it gives the wire bytes.

Out of the box, with no configuration. The select rows are the exception to
"out of the box": they are the graph's, and on the default `single` rig — one
camera, one monitor, no inputs — all three are dead.

| control | is |
|---|---|
| fader 1 | the **send**, on a rig that has an input; nothing on one that has not |
| faders 2–8 | the focused **monitor**: hue, saturation, brightness, contrast, gamma, headroom, and the crosspoint — how much of the focused camera it shows |
| rotaries 1–8 | the focused **camera**: zoom, rotation, pan x, pan y, loop gain, bloom, chroma bleed, noise |
| S 1–8 | focus camera 1–8, as many as the graph has a choice of |
| M 1–8 | focus monitor 1–8, likewise |
| R 1–4 | focus input 1–4, likewise |
| marker next | blank the monitors |
| ◀◀ rewind | put the last knob turned back to its identity |
| ■ stop | reset every knob |
| ▶ play | the focused monitor's seed: white blob or camera; lit while it is the blob |
| ⟳ cycle | the on-screen controls overlay, on or off |
| ▶▶ forward | the focused monitor on the whole display, or the tiled bank |
| \|◀ track prev, ▶\| track next | the tempo: slower / faster, four presses to halve or double it |
| ● marker set | write what the display is showing to a file |
| ● record | record the display for as long as it is held down |
| marker prev | nothing |

So the left hand works one monitor, the right hand one camera, and the two
crosspoints bracket the front panel: outside light enters at fader 1 and loop
light arrives at fader 8. The three
select rows point the knobs at a node, one kind of node each: Solo the
cameras, Mute the monitors, Record the inputs.

**A row is the choice its kind offers, and nothing else.** The surface is
built for the graph about to be played. A rig of one camera and two monitors
binds M1 and M2 and leaves the other twenty-two select buttons dead — unlit,
silent, and free for a `midi.toml` to claim. The Solo row is dead there
because one camera is no choice: a button that selects the only camera there
is selects what is already selected, and the rule is that a button is
owed to equipment, not spent on it. Dead is the point.

The loud cases are the other way round. More of a kind than a row is wide and
the config is refused at load rather than played with a node no hand can
bring the knobs to; a `midi.toml` that binds a select button on a node the
graph has not got is refused the same way, rather than lighting a button that
lies. Nothing is bound to quit — the window manager ends the instrument, and a
slipped finger on the surface must not be able to. Eight of the twenty-four
knobs are not on the factory map: the three per-channel gain offsets, which
colour a rigid gain that is itself on a rotary; the bloom radius, which sizes
a halo whose amount is; and the keyer's four, which wait for a hand that keys
more than it bleeds and swaps this map for its own. They are set in the graph
file, and a `midi.toml` may put any of them on a control.

**A fader does not take its knob over until it has passed through where the
knob already is.** A fader sends where it is standing, so without that,
plugging in mid-piece throws every knob to wherever its fader was left — with
the headroom fader slamming a monitor to white. Sweep the fader and it picks
its knob up on the way past, and from then on the fader is the knob. It lets
go on a reset and a change of focus — the two ways the panel moves without a
fader moving with it — and on an unplug, after which nothing knows where a
fader is standing.

The buttons are read on the way down, which assumes the surface's buttons are
**momentary** rather than latching — Korg's editor calls it Button Behavior.
A latching button plays on every second press.

### Putting one knob back, and playing the tempo

Stop puts the whole panel back to the graph as it was loaded. **Rewind puts
back the one knob you were just turning**, to its *identity* — the value at
which its stage does nothing to the light: zoom 1, no turn, no pan, unity
gain, a clean path, the keys off, a neutral front panel. The
crosspoint is the one knob with no such value — it is a weight in a sum
rather than a stage the light passes through, and its row *is* the monitor's
loop gain — so its identity is the connection not made. Unity there would put
a second camera on the monitor at full and take a `crossed` row to 2.0,
where zero loses that camera visibly and the fader puts it straight back.

Named by having been turned rather than by a control of its own, because
there are two dozen of them and no display to point at one with, and the knob a
hand wants back is the one that hand was just on. Which stops being true the
moment the panel moves without the hands, so a whole-panel reset and a change
of focus both clear the name along with the faders' grips — otherwise rewind
after either would put back a knob nobody has touched. Only that knob's fader
lets go, so a single-knob reset does not charge the rest of the panel a
pickup sweep.

**The track pair plays the tempo**: |◀ slower, ▶| faster, a press being the
fourth root of two so four of them halve or double the rate. It is the one
control that acts on the whole piece rather than on a node of the graph, and
the TRACK silkscreen is the one pair the surface prints as a pair — a minus
and a plus want a pair to sit on. `--rate` is where the piece starts; the
track pair is where it is played from there, and the rate line a second later
is the readout. Nothing latches: a tempo is heard, not held.

### The lit buttons

**The Solo button of the focused camera is lit, and so is the focused
monitor's**, so the panel says where each hand's knobs are without anyone
reading the log line. They follow the focus wherever it moves and go out when
the instrument does. A node the map bound no button to has none to light, and
lights none: a lamp on the wrong button is worse than no lamp. A latched mode
lights the button holding it by the same rule, off that
button's *action*, so a `midi.toml` that moves the overlay moves its lamp with
it.

That takes setting up, and the app does the setting up. A nanoKONTROL2 leaves
the factory in **LED Mode: Internal**, where a button lights itself while it is
held and ignores the host, and Korg's only supported way to change that is the
KONTROL Editor — Windows and macOS. So the app asks the surface for the scene
it is playing, sets the one byte that is the LED mode, and hands the same scene
back. Nothing else in the scene is touched, so a performer's own control
assignments survive it; nothing is written to the surface's flash, so a replug
is always the surface its owner set up. The cost is a handshake on every
connect, which is where it belongs.

**That switch is one switch for the whole panel**, which is why the app then
drives every button rather than the eight it came for: external mode takes
every row's lights, not just the Solo row's. So a button the map binds is lit
while it is held — exactly what internal mode did for it — and the focused
camera's is lit whether or not a finger is on it. What the instrument adds is
one lamp; what it takes away is nothing. A button the map binds nothing to
stays dark,
which is now what it means.

The mode goes back to Internal on the way out, so the surface lights its own
buttons again for whatever is played next. **On the way out of a clean exit**
— a killed process runs nothing, and external mode lives in the device's RAM,
so a `SIGTERM` leaves the mode set and the last lamp burning until the surface
is replugged or the instrument is run again. Which is why, on the way in, the
panel is blanked before any lamp is lit and the mode is put back even when it
was already found set: the next run repairs what the last one was killed
before finishing, rather than taking a dark panel on faith and lighting a
second lamp beside a stale one.

A surface that will not take the mode still plays exactly as it did before,
and says so once on the log. So does one whose device node will not open for
writing, and one that does not answer as a nanoKONTROL2 at all — that last is
written nothing after the inquiry, because what a control change does to a
device this does not recognise is not knowable from here.

None of it happens on a frame. The handshake waits on replies that may never
come and a MIDI write blocks when the wire is full, so the lights are a thread
of their own; the frame loop's whole part is to say which lamps it wants, down
a channel.

### Mapping config

`$XDG_CONFIG_HOME/lightherder/midi.toml`, named on the log at startup. If it
is not there you get the layout above; if it is there and will not load, the
instrument says why and does not start.

```toml
# Matched case-insensitively against the card's line in /proc/asound/cards.
device = "nanoKONTROL"

[[fader]]
cc = 0
knob = "hue"        # any knob name the card prints

[[button]]
cc = 41
command = "blank"   # any command name the card prints

[[button]]
cc = 90
command = "mon 1"       # focus monitor 1, off a control the surface has spare
```

A fader names a **knob** and spans its whole travel — for the two knobs that
wrap, rotation and hue, that is one full revolution from bottom to top. A
button names a **command**, spelled the way the overlay captions it: `blank`,
`reset`, `reset 1`, `seed`, `solo`, `help`, `snap`, `record`, `rate -`,
`rate +`, and `cam 1`…`cam 8`, `mon 1`…`mon 8`, `in 1`…`in 4` for the focus —
as many of each as a graph may legally hold.

Every channel is listened to, so a surface set to some other MIDI channel
still works. A control number may only be bound once, and a command name that
nothing answers to is refused at load with the list of the ones that do.

## Run it

```
nix-shell --run "cargo run --release"                    # the single loop
nix-shell --run "cargo run --release analog"             # the same, with the signal path on
nix-shell --run "cargo run --release crossed"            # two crossed structures
nix-shell --run "cargo run --release insanity"           # four, all-to-all
nix-shell --run "cargo run --release external"           # a test pattern driving the loop
nix-shell --run "cargo run --release webcam"             # /dev/video0 through the luma key
nix-shell --run "cargo run --release my-graph.toml"      # your own
```

It comes up covering the display, because an instrument on a stage is the only
thing on its screen; `--windowed` is how you get at the rest of the machine.
Quitting is the window manager's — closing the window, or a `TERM`. Nothing on
the surface stops the instrument: a slipped finger mid-performance must not be
able to.

```
nix-shell --run "cargo run --release -- --windowed crossed"
```

| | |
| --- | --- |
| `--windowed` | a window rather than the whole display |
| `--resolution 3840x2160` | how big every monitor is (default 1920x1080) |
| `--rate 30` | passes a second, the speed the piece plays at (default 60, 1 to 240) |
| `--cheatsheet` | the surface as it is mapped, and exit |
| `--bench` | what a frame costs, off screen, and exit |

Through `cargo run` they need the `--` above, which is cargo's and not this
program's; a built binary takes them directly.

The resolution is every monitor's size, and so the resolution the whole loop
runs at. The window's shape has nothing to do with it: the bank is tiled into
the window, each monitor letterboxed in its cell rather than stretched — and
enter solos the focused monitor onto the whole window and back, which is that
same tiling with one tile in it. Nor is it part of a graph — every position
here is in screen units and every weight a
ratio, so it changes how much detail the loop carries and — the grain aside,
which is hashed per texel and so is finer on a bigger monitor — nothing about
what it does. On a 4K display ask for `3840x2160` and what is on the glass is
the loop's own detail rather than an upscale of a smaller one.

A graph file is the same shape as the presets — `cargo run` on a bad file
prints exactly what is wrong. The smallest useful one:

```toml
cameras = [{ look = [1.0], framing = { zoom = 0.994, rotation = 0.05 } }]
monitors = [{ seed = { white_blob = 0.1 } }]
routing = [[0.98]]
```

`look` is the camera's beam splitter, a weight per monitor. `seed` is
`{ white_blob = <brightness> }` or `"dark"`, and defaults to `"dark"` — a
monitor lit only by what the switcher hands it. `routing[m][c]` is how much
of camera `c` monitor `m` shows and `routing_inputs[i][m]` how much of input
`i`, and anything omitted — framing, gain, colour, an empty switcher half — is
neutral.

The `shell.nix` pins nixpkgs, puts the Vulkan loader and windowing libraries
on `LD_LIBRARY_PATH`, which wgpu and winit open at run time, and carries the
`ffmpeg` the file and capture inputs run. Without Nix, a Rust toolchain recent
enough for wgpu 30 and winit 0.30 and a working Vulkan/Metal/DX12 driver will
do, plus ffmpeg if you want those inputs.

The adapter is opened before the window exists — which is what lets a browser
start the same instrument without blocking, and means nothing has checked that
the fastest card can reach the display. On a hybrid machine whose screen hangs
off the integrated GPU it cannot, and the instrument says which card and stops
rather than leaving a black window open: `WGPU_POWER_PREF=low` takes the
integrated adapter instead.

## Deploy

**The pass rate is a tempo, not a smoothness setting.** The loop evolves one
pass at a time — the camera pulls back 0.6% and turns 0.05 rad per *pass*, and
the trail decays per pass — so a spiral drawn in a second at sixty is drawn in
a quarter of one at 240, the top of the range. That makes the rate a control
rather than a property of the machine: the surface's track pair moves it while
the piece plays, and `--rate` starts it somewhere other than sixty.

**The display keeps its own clock, and it is vsync.** A pass is not a present.
Passes fall due on the wall clock at the tempo; the picture goes out on every
vertical blank, showing wherever the piece has got to — several passes at
once when the tempo is above the grid, the same bank twice when it is below.
The present is also what paces the loop — Fifo waits for the blank, and the
frame that went out asks for the next one — so the tempo's own deadline is
armed only when frames stop landing, behind a covered window or a surface gone
stale. The piece goes on playing there without a picture; it is not the
picture.

A torn frame is a wrong frame in a piece whose look is the point, so the
present mode is pinned to Fifo rather than taken from the adapter, which
offers something faster. Keeping the tempo out of the swapchain is what makes
that affordable: a display path granting 41 frames a second runs the sixty
passes anyway, one or two to a present.

The log prints both clocks once a second — `sim 60 Hz of 60, present 72 Hz` —
and deployed there is no terminal in front of the instrument, so that line is
the whole of what can be read. The two say different things. Passes under the
tempo is the machine or the graph, and the piece really is playing slow.
Presents are the display's own rate and say nothing about the piece.

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
1080p bank prints the second and not the first. Then the rate line a second
later, counted from the first pass rather than from before the pipelines were
built.

### What a frame costs

On a display a pass has a whole beat to fit inside, so a rate line at the tempo
says only that it fit — not by how much. `--bench` runs the same passes with
nothing pacing them: 600 frames after a warm-up, the graph
stepped and presented into a target the size of the display.

| graph | 1920x1080 | 3840x2160 |
| --- | --- | --- |
| `single` | 0.13 ms | 0.39 ms |
| `external` | 0.16 | 0.50 |
| `analog` | 0.21 | 0.71 |
| `webcam` (2 monitors) | 0.21 | 0.66 |
| `crossed` (2 monitors) | 0.25 | 0.75 |
| `insanity` (4 monitors, all-to-all) | 0.67 | 2.35 |

A beat at sixty is 16.7 ms, so the heaviest graph that ships uses a seventh of
one at 4K. Measured on an RTX 2080. What the numbers leave out is a frame's
edges rather than its loop: handing the frame to the compositor, and the
upload of a live input, which for a video file or a capture device is a
conversion and two writes of a whole frame every frame. `webcam` is measured
with none — `--bench` steps the graph, and a device that is not there uploads
nothing — so its row is the passes and not the wire.

The bank itself is what grows: two copies of every monitor and input at eight
bytes a texel, half a gigabyte for `insanity` at 4K, and refused past two.

## Playing it

**The control surface is the instrument.** There is no keyboard: if a control
is not on the board it does not exist, so every knob a hand turns and every
command a hand presses is on the panel above, and everything else is the graph
file. The card prints on startup and `--cheatsheet` prints it without starting
anything; the surface's cycle button toggles the same panel on the glass,
drawn as it is actually mapped and each control captioned in a couple of
words. Every knob logs its new value on change.

The knobs act on the focused camera (framing, gain and character), the focused
monitor (colour and headroom) and the focused input (the send). The three
select rows pick a node of any of the three outright, and the log line names
them — so a rig with nothing to choose plays camera one, monitor one and input
one, on the knobs the config gave them.

Splitter weights are config; the two crosspoints are not — fader 8 sweeps how
much of the focused camera the focused monitor shows, and fader 1 sweeps how
much of the focused input it shows, on a rig that has one.

The colour and character knobs start neutral, so the instrument out of the box
is the loop described above and nothing else. Turn one against it: the
saturation fader swept is the quickest way to see what a stage inside the loop
does, and the bloom rotary swept is the quickest way to see what the loop does
to a stage — a lens that scatters a tenth of the light per pass has spread it
everywhere by the tenth pass.

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
the same: what was written to an input's layer is what the monitor it is
patched to shows, it is current in whichever bank the cameras read, blanking
the monitors leaves it alone, the switcher sums it with a camera on one
monitor, and it arrives square on however the cameras are framed.
On a machine with no adapter each one prints the reason straight to the
process's stderr and returns; libtest still counts them as passed.

The input decoding is tested without a GPU: the drawn bars against the
levels they name, the ffmpeg command lines against the options that make them
loop, letterbox and stay off the network rather than race and stretch, a capture
source against what ffmpeg was told to generate, a real file written and
decoded, and a file that is not there refused at once rather than after the
first-frame timeout. Outside the pinned shell the ones that need ffmpeg print
a skip, on the same terms as the GPU tests.

A graph file is tested as a file: one naming every field of the format, off
its default in every one, loaded through the door the command line uses; and
one the instrument would refuse, refused there rather than at the GPU. There
is nothing to round-trip against, because nothing writes one.

The surface is tested without one plugged in, at every layer and then through
all of them at once. The decoder against the ways a fader sweep actually
arrives — three bytes split across reads, running status with the status byte
left off, a clock byte landing between a control number and its value, a scene
dump that must not read as a hundred knob moves, and notes and bends that are
not knobs. The pickup against a fader that has to reach its knob before it
moves it, one already standing on it, and one that loses its grip on an
unplug. The map against a duplicate binding, a command nothing
answers to, and a literal file rather than a round trip, because a round trip
agrees with itself whatever the fields are called. The card search against a `/proc/asound/cards`
with two other cards that also have raw MIDI devices. And the whole path —
discovery, the open, the reader thread, the decode, the map and the pickup —
against a device that is not there when the instrument starts, appears, sends
a sweep down a pipe, and goes away again, which is what hot-plug is.

The lights are tested at the file descriptor, over a socket pair standing in
for the device node: what the instrument writes really leaves a descriptor and
is really read back on the other side, byte for byte. Korg's seven-bit packing
against numbers worked out by hand, since a packer and its own inverse agree
on any bit order at all — the reversed one included. The handshake against a
surface that hands over an internal scene, which must come back with one byte
changed and 338 untouched; one already external, which must be sent no scene
to get it there; one whose scene disagrees with the channel it answered on,
which must not be written back at all; one that answers as some other maker's
device, which must be written nothing after the inquiry; and one that answers
nothing, which must give up on its own, in bounded time, and still leave the
surface played. The lamps against a focus that moves — out first, then on,
because two lit at once is a panel claiming the knobs are in two places —
against the same focus said sixty times, which must put nothing on the wire,
against a held button, which must light with the focus rather than instead of
it, against a lamp no button of the map answers to, which must never reach the
wire, and against the exit, which must put the lamps out and the mode back.
The two halves of the select row are lit one side at a time, so neither can
cover for the other; a latched mode is checked to light the button its command
is bound to, and to light nothing when the map binds that command nowhere; and
the first of two buttons bound to one node is the one that lights.

## In a browser

The same instrument, on WebGPU, at
<https://bddap-bot.github.io/lightherder/> — the graph is chosen the way a
page takes an argument, `?preset=insanity`. What is not there is what a
browser has no way to give it: the ALSA control surface and an ffmpeg input —
so a tab plays the graph it was handed and nothing turns a knob.

`web/build.sh` builds `web/dist` — the module, its glue and the page — and
every push to `main` runs it and publishes the result. Locally:

    nix-shell --run ./web/build.sh
    python3 -m http.server -d web/dist

The `wasm-bindgen` crate is pinned to the patch because the generator of that
glue must be the same version; the dev shell carries that exact binary and
`web/build.sh` refuses anything else.
