# lightherder

A GPU video-feedback instrument: a software realization of an analog
video-feedback rig, where cameras are pointed at the monitors they are drawing
to. Rust + wgpu, no CPU pixel work in the loop.

The rig is **Dave Blair's 4K Light Herder**, and there is no other — three
cameras, five monitors, four switchers and one seed, with the freedoms his
schematic gives them and no others. Nothing on the command line names an
instrument, because there is one: `lightherder [options]`, and a bare word is
a typo rather than a piece to play. It reads no graph and writes nothing down;
the panel lives as long as the run, the way the hardware it is modelled on
does.

Every camera watches monitors and only monitors, so every path in the rig is a
loop. The one light the rig did not make is a camera on the room —
`/dev/video0` — plugged into the switcher, which is where a real rig plugs one
in.

## How it works

A "monitor" is one layer of an offscreen `Rgba16Float` texture array. A
"camera" is a fullscreen pass that samples a layer through an affine transform
— the slide and the turn of the shaft it stands on — multiplies by a
per-channel gain and writes the result back. That output is the next frame's
input, which is the whole trick: pull the camera back a little each pass and
the image walks inward, turn it a little and it spirals.

The wiring flattens. What a monitor shows is a weighted sum of the three
cameras and the seed; each camera sees a blend of monitors through the glass
in front of its lens; sampling is linear — so the whole path from a monitor's
next frame back to the bank of previous frames collapses on the CPU into a
handful of *taps* — (source layer, sampling transform, weight) — and each
monitor is one render pass summing its taps. There is no intermediate blend
texture because none is needed. All monitors step from the same previous
frames, the simultaneous capture a rig of real cameras performs, and the
window shows the whole bank tiled in a grid — or one monitor of it on the
whole display, which is the same tiling with one tile in it.

Half-float keeps headroom above 1.0, so dozens of passes do not quantise into
bands. A sample that falls outside a monitor reads as black rather than as a
smeared border texel, because a real camera aimed past the monitor sees an
unlit room.

## The rig

Two **structures**, A and B. Each is an upper and a lower monitor at a right
angle with 50/50 glass at 45° between them, and a camera looking into the
glass: it sees its upper monitor directly and its lower one in the reflection,
at half each. The fifth monitor turns on a shaft of its own and shows camera
B, always. **Camera 3** is fixed on that rotating monitor and sees nothing
else.

The cameras stand on **two shafts, not three**. Camera A and the rotating
monitor are belt-locked to one — they turn and slide in unison — and camera 3
watches that monitor, so what camera 3 sees moves with camera A off the one
number the two share. Camera B has its own shaft. `zoom` and `rotation` on
camera A and on camera 3 are therefore the same knob: the lock is that fact
and the pair of shafts behind it, not a second number kept in step with a
first.

Four **switchers**, the M/Es, each a crossfade between two feeds:

| | In1 | In2 |
| --- | --- | --- |
| A | camera A | camera B |
| B | camera B | C's program |
| C | camera A | D's program |
| D | camera 3 | the seed |

and one **router select** per structure monitor: its own camera direct, or its
switcher's program. One or the other, never a mix — mixing is the switcher's
job, one stage upstream. The rotating monitor has no select, and the button is
dead on it.

**Those eight levers are the whole of the routing state.** A crossfade is a
weighted sum, so the chain multiplies out into the matrix of camera and seed
shares the taps are built from — and that matrix is worked out every time it
is asked for and held nowhere. Switcher A and the four selects land on it
directly; B, C and D reach a monitor only as products of one another, so a
stored matrix would be a second state standing beside the levers that set it,
free to drift from them. At every setting of the eight, on every monitor, the
shares sum to exactly one: nothing on the rig's cabling amplifies.

It starts with every monitor on its program — on Direct the switchers would
feed nothing — both cross-links a quarter open, C half and D a tenth. So each
structure is made of the other and still keeps a shape of its own, and the
seed arrives on a B monitor at 0.0125. Structure A never takes the seed
directly: it reaches A only as light already round B's loop.

Each camera's gain is a per-channel loss down the cable and through the lens,
just under unity, which is what makes a loop settle rather than run. A's blue
survives best and B's red does, so the two structures' trails cool and warm
away from each other. Nothing on the rig turns one, so nothing here does
either.

Out of the box both shafts pull back — 0.6% a pass — and turn the same way at
their own rates, 0.05 and 0.08 radians, so the structures stay distinct and no
round trip cancels its own rotation.

## The front panel

Before a monitor's output is written it passes that monitor's own front panel:
the chroma decode, the video amplifier, and the amplifier's rails.

The decode works in NTSC luma/chroma rather than RGB, which is what makes hue
a *phase* — the two chroma axes are the real and imaginary parts of one
subcarrier, so hue turns it and saturation scales it, and luma comes out
untouched. Colour temperature is the phosphor's white point, a distance along
the Planckian locus from D65: the chroma of that white rides the luma into the
channels, so a grey warms to candlelight or cools to shade at the same
brightness, and the hue does not turn it — a turned chroma is not a turned
phosphor. Decode, turn, white and encode compose into one 3x3, which the CPU
works out once a frame: chained per fragment instead they leave a
ten-thousandth of the signal behind on every pass, and a loop that feeds
itself turns that into a colour cast.

Then contrast about mid-grey, and brightness as a lift. Contrast pivots about
mid-grey rather than about black on purpose: a gain about black is exactly
what the loop gain already is, and the front panel is not the place for a
second one.

Then the **rails**, at twice display white, where the video amplifier runs out
of them. Below half of that the signal is untouched; above, it bends
asymptotically onto it, the two arms meeting at the knee in both value and
slope. This is the difference between an analog feedback rig and a runaway
multiply: with the rail wide open the middle of an overdriven spiral is a flat
white disc, and with the rail on the signal the same overdrive compresses into
a structure you can still see. It is a constant of the instrument rather than
a knob — a real amplifier always has rails, and nothing on the rig turns
them. The knee lands exactly on 1.0, so nothing a monitor can actually show is
touched, and the reserve above white that the half-float bank exists to keep
compresses onto 2.0 rather than running.

Before any of that, on what the switcher hands the monitor: its **sharpness**,
an unsharp mask a texel wide — the detail an LCD's driver board puts back, the
difference between a texel and the mean of its four neighbours, added back by
the knob. At rest the stage is skipped outright, so a rested knob is exactly
inert inside a loop that would compound a residual.

All of it is inside the loop, so every knob compounds once per pass: a few
hundredths of a radian of hue walks the trail through the spectrum, and a
brightness above zero lifts the whole frame and floods it.

## The seed

A camera watches monitors. That is the instrument: a camera on a stand in a
room of monitors sees the light already going round, so every path closes and
there is no such thing as an aimable source. The light the rig did not make
arrives on the **switcher**, beside the cameras, as In2 of M/E D.

It is one physical camera, `/dev/video0`, and the monitors start dark — on
this rig the seed is what sparks the loops. It is a further layer of the same
source bank, so nothing in the shader knows which kind of layer it sampled,
and a monitor sums the cameras and the seed without being told which was
which. It is not part of any loop: nothing draws to it and no camera watches
it. It arrives square on and whole, because there is no lens between it and
the switcher — framing, gain and glass are a camera's, and the seed passes
none of them.

On its way in it meets the switcher's **luma key**: passing from mid-grey up
and cutting to nothing a little below, which is a lit subject against an unlit
room — what a camera pointed at a couch faces. The lit subject feeds the rig
and the room behind it does not. That is where a key can sit at all; on a
loop's own camera it would gate the trail it is building, so there would never
be a trail. It is fixed character, not a control: the board has no key.

How much of it reaches a monitor is the switchers' business, which is what
makes it playable without a knob of its own. Near unity a little goes a long
way — every photon on a monitor lit only through the chain came in from
outside — so sweeping M/E D floods the outside in, and running it back to In1
leaves the loops on what they have.

The pixels are an `ffmpeg` reading the device and writing raw RGBA down a
pipe, scaled and letterboxed to the monitor size. A source that has not
produced its first frame by the time the window would open is an error on the
terminal, not a black layer, and one that ends mid-performance says so in the
log and leaves its last frame on the layer. In a browser it is a `<video>`
playing the page's own camera, read back through a canvas into the same bytes.

## The control surface

The instrument is played from a **Korg nanoKONTROL2**, plugged in whenever —
before it starts or halfway through a piece. There is nothing to set up: while
there is no surface, the app reads `/dev/snd` once a second looking for one
whose card is named in `/proc/asound/cards`, and when the cable comes out the
read ends and it goes back to looking. ALSA raw MIDI and no library — a
controller is `/dev/snd/midiC<card>D0`, and reading it gives the wire bytes.

Out of the box, with no configuration. Eleven knobs on sixteen continuous
controls, so there is no page button and the five rotaries past the third are
dead.

| control | is |
|---|---|
| faders 1–6 | the focused **monitor**'s front panel: hue, saturation, brightness, contrast, temperature, sharpness |
| fader 7 | the focused **switcher**'s period: passes between reversals, 0 to 60, and 0 is the mode off |
| fader 8 | the focused **switcher**'s crossfade — the lever the piece is played on, nearest the hand that is already on the select rows |
| rotaries 1–3 | the focused **camera**: where it stands on its shaft (zoom, rotation) and how late its cable is (delay) |
| rotaries 4–8 | dead — free for a `midi.toml`, and no hand throws one by accident |
| S 1–3 | focus camera A, B, 3 |
| S 4, S 5 | dead |
| S 6, S 7 | **precision -**, **precision +**: halve or double what a full throw of a fader moves, on a ladder from a whole travel down to a sixteenth; a quarter to begin with; the log says which rung |
| S 8 | **clutch**: while held, every fader and rotary moves nothing, so a hand can bring one back from a rail; lit while held |
| M 1–5 | focus monitor: upper A, lower A, upper B, lower B, rotating |
| M 6–8 | dead |
| R 1–4 | focus switcher A, B, C, D |
| R 5 | **reverse**: the focused switcher's In1 and In2 trade places — the crossfade run to the other end of its travel |
| R 6, R 7 | **flip x**, **flip y**: mirror the focused monitor's router output left for right, top for bottom; lit while it is |
| R 8 | **select**: the focused monitor on its switcher's program or on its own camera; lit on program; dead on the rotating monitor, which has none |
| marker next | blank every monitor, so the loops restart from the seed alone |
| ◀◀ rewind | put the last knob turned back to its identity |
| ■ stop | reset every knob |
| ⟳ cycle | the on-screen controls overlay, on or off |
| ▶▶ forward | the focused monitor on the whole display, or the tiled bank |
| \|◀ track prev, ▶\| track next | the tempo: slower / faster, four presses to halve or double it |
| ● marker set | write what the display is showing to a file |
| ● record | record the display for as long as it is held down |
| marker prev | **cut**: the switcher's foot pedal — the press throws the focused switcher end to end and the release puts it back |
| ▶ play | nothing — the factory map binds it to no command, so it stays dark |

So the left hand works one monitor and one switcher, the right hand one
camera, and the three select rows point the knobs at a node, one kind each:
Solo the cameras, Mute the monitors, Record the switchers. Solo selects
because that is what a hand off a mixer reaches for it to do, and the other
two rows follow it downward in the order the light travels — the cameras that
film the glass, the glass, then what routes between them.

**A row is exactly as wide as its kind.** The rig is three cameras, five
monitors and four switchers, so five select buttons are dead — unlit, silent,
and free for a `midi.toml` to claim; the commands take the rest of the tails,
which is why R5–R8 and S6–S8 cost the transport nothing. A button is owed to
equipment, not spent on it. Dead is the point.

The loud cases are the other way round. A `midi.toml` that binds a select
button on a node the rig has not got is refused at load rather than lighting a
button that lies. Nothing is bound to quit — the window manager ends the
instrument, and a slipped finger on the surface must not be able to.

**A fader turns its knob by how far it moves, never to where it stands.** A
fader sends where it is, and what the instrument reads off that is the
distance since it was last heard from: a knob moves by that fraction of its
travel, scaled by the precision — a quarter by default, so a full throw covers
a quarter of the knob and one step of the 127 covers a five-hundredth. Nothing
jumps: a hot-plug, a change of focus, a reset, a cut or a beat of the period
all leave every fader turning on from wherever the knob now is. The rails
clamp — a step past one is dropped, not owed — and a fader that has run out of
travel is brought back under the **clutch**: while S8 is held, every fader and
rotary moves nothing, and letting go resumes from the new position. Rotation
and hue wrap instead of clamping, a whole revolution end to end. Zoom is a
ratio and a step multiplies instead of adding: a throw doubles it from
wherever it stands, one code moves it half a percent, and unity sits in the
middle of the travel, so the thousandths either side of 1.0 get the same hand
as the doublings above. The two whole-number knobs, the delay and the period,
are turned a frame at a time, ticking over at the half like a detent.

The buttons are read on the way down, which assumes the surface's buttons are
**momentary** rather than latching — Korg's editor calls it Button Behavior. A
latching button plays on every second press.

### Putting one knob back, and playing the tempo

Stop puts the whole panel back to the instrument as it started. **Rewind puts
back the one knob you were just turning**, to its *identity* — the value at
which its stage does nothing to the light: zoom 1, no turn, no delay, a
neutral front panel and no sharpening. The crossfade is the one knob with no
such value — it is not a stage the light passes through but where a sum
stands — so its identity is the end of its travel it started at, In1 whole.
The picture visibly loses the mix and the fader puts it straight back, which
is the error that corrects itself.

Named by having been turned rather than by a control of its own, because there
are eleven of them across twelve nodes and no display to point at one with, and
the knob a hand wants back is the one that hand was just on. Which stops being
true the moment the panel moves without the hands, so a whole-panel reset and
a change of focus onto another node of the same kind both clear the name —
otherwise rewind after either would put back a knob nobody has touched.

**The track pair plays the tempo**: |◀ slower, ▶| faster, a press being the
fourth root of two so four of them halve or double the rate. It is the one
control that acts on the whole piece rather than on a node of the rig, and the
TRACK silkscreen is the one pair the surface prints as a pair — a minus and a
plus want a pair to sit on. `--rate` is where the piece starts; the track pair
is where it is played from there, and the rate line a second later is the
readout. Nothing latches: a tempo is heard, not held.

### The lit buttons

**The focused camera's Solo button is lit, and so are the focused monitor's
and the focused switcher's**, so the panel says where each hand's knobs are
without anyone reading the log line. They follow the focus wherever it moves
and go out when the instrument does. A latched mode — the overlay, the solo,
the flips, and the select, which is the one bit nothing else on a fullscreen
display says — lights the button holding it by the same rule, off that
button's *action*, so a `midi.toml` that moves a binding moves its lamp with
it. A node or a mode the map bound no button to lights nothing rather than the
nearest button: a lamp on the wrong button is worse than no lamp.

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
drives every button rather than the few it came for: external mode takes every
row's lights, not just the Solo row's. So a button the map binds is lit while
it is held — exactly what internal mode did for it — and the focused camera's
is lit whether or not a finger is on it. What the instrument adds is one lamp;
what it takes away is nothing. A button the map binds nothing to stays dark,
which is now what it means.

The mode goes back to Internal on the way out, so the surface lights its own
buttons again for whatever is played next. That includes being told to stop:
`SIGTERM` and `SIGINT` — a Steam shortcut ending, `timeout`, Ctrl-C — end the
run the way closing the window does, so the lights are put back before the
process is gone. The signals are held and one thread waits on them, which is
why nothing before the instrument opens its inputs may start a thread of its
own; a second signal while the run is already stopping is swallowed rather
than cut it short, because `timeout` and a cgroup stop deliver the same one
twice. External mode lives in the device's RAM, so a `SIGKILL` or a power cut
still leaves the mode set and the last lamp burning until the surface is
replugged or the instrument is run again. Which is why, on the way in, the
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
instrument says why and does not start. It remaps the surface and nothing
else — there is no config file for the rig, because there is nothing in the
rig to choose.

```toml
# Matched case-insensitively against the card's line in /proc/asound/cards.
device = "nanoKONTROL"

[[fader]]
cc = 0
knob = "hue"        # any knob name the card prints

[[button]]
cc = 71
command = "select"

[[button]]
cc = 41
command = "blank"   # any command name the card prints

[[button]]
cc = 90
command = "mon 1"   # focus the upper A monitor, off a spare control
```

A fader names a **knob** and spans its whole travel — for the two knobs that
wrap, rotation and hue, that is one full revolution from bottom to top. The
knobs are `zoom`, `rotation`, `delay`, `hue`, `saturation`, `brightness`,
`contrast`, `temperature`, `sharpness`, `switcher` and `period`. A button
names a **command**, spelled the way the overlay captions it: `blank`,
`reset`, `reset 1`, `solo`, `help`, `snap`, `record`, `cut`, `reverse`,
`select`, `flip x`, `flip y`, `rate -`, `rate +`, `precision -`,
`precision +`, `clutch`, and `cam 1`–`cam 3`, `mon 1`–`mon 5`, `sw 1`–`sw 4`
for the focus.

Every channel is listened to, so a surface set to some other MIDI channel
still works. A control number may only be bound once, and a name that nothing
answers to is refused at load with the list of the ones that do — a surface
that quietly plays the wrong knobs is worse than one that will not start.

## Run it

```
nix-shell --run "cargo run --release"
```

It comes up covering the display, because an instrument on a stage is the only
thing on its screen; `--windowed` is how you get at the rest of the machine.
Quitting is the window manager's — closing the window, or a `TERM`. Nothing on
the surface stops the instrument: a slipped finger mid-performance must not be
able to.

| | |
| --- | --- |
| `--windowed` | a window rather than the whole display |
| `--resolution 3840x2160` | how big every monitor is (default 1920x1080) |
| `--rate 30` | passes a second, the speed the piece plays at (default 60, 1 to 240) |
| `--bench` | what a frame costs, off screen, and exit |

Through `cargo run` they need the `--` above, which is cargo's and not this
program's; a built binary takes them directly. Two of them at once — `--bench
--help` — is refused rather than answered silently with one, and a rate
outside the range is refused rather than clamped: a performer who typed 6000
meant something, and playing 240 instead answers neither the number nor the
mistake behind it.

The resolution is every monitor's size, and so the resolution the whole loop
runs at. The window's shape has nothing to do with it: the bank is tiled into
the window, each monitor letterboxed in its cell rather than stretched — and
FORWARD solos the focused monitor onto the whole window and back, which is
that same tiling with one tile in it. It is fixed for a run rather than
following the window, so resizing rescales the view instead of scrambling the
loops' state. Every position in the rig is in screen units and every weight a
ratio, so the size changes how much detail the loop carries and nothing about
what it does: it is a property of the machine this is deployed on rather than
of the piece. On a 4K display ask for `3840x2160` and what is on the glass is
the loop's own detail rather than an upscale of a smaller one.

The delay units reach two frames — a camera's `delay` rotary is whole frames
up to that, on top of the one pass every camera is behind by. The reach is
bought in bank rather than in taps: a frame of it is another copy of all five
monitors, and the cap holds about four at 4K.

The `shell.nix` pins nixpkgs, puts the Vulkan loader and windowing libraries
on `LD_LIBRARY_PATH`, which wgpu and winit open at run time, and carries the
`ffmpeg` the seed runs. Without Nix, a Rust toolchain recent enough for wgpu
30 and winit 0.30, a working Vulkan/Metal/DX12 driver, and an `ffmpeg` on
`PATH` will do — the seed is a capture device, so ffmpeg is not optional here.

The adapter is opened before the window exists — which is what lets a browser
start the same instrument without blocking, and means nothing has checked that
the fastest card can reach the display. On a hybrid machine whose screen hangs
off the integrated GPU it cannot, and the instrument says which card and stops
rather than leaving a black window open: `WGPU_POWER_PREF=low` takes the
integrated adapter instead.

## Deploy

**The pass rate is a tempo, not a smoothness setting.** The loop evolves one
pass at a time — the shafts pull back 0.6% and turn per *pass*, and the trail
decays per pass — so a spiral drawn in a second at sixty is drawn in a quarter
of one at 240, the top of the range. That makes the rate a control rather than
a property of the machine: the surface's track pair moves it while the piece
plays, and `--rate` starts it somewhere other than sixty.

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
tempo is the machine, and the piece really is playing slow. Presents are the
display's own rate and say nothing about the piece.

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
    "/srv/lightherder/lightherder --resolution 3840x2160"
```

Its own log is how you know it worked, since nothing else on that machine can
see the screen — and it takes two lines, because they are two different
things: `5 monitors of 3840x2160, 3 cameras, one seed` is the bank, and
`window 3840x2160 (covering the display), presenting Fifo at Rgba8UnormSrgb`
is the window. A 4K window over a 1080p bank prints the second and not the
first. Then the rate line a second later, counted from the first pass rather
than from before the pipelines were built.

### What a frame costs

On a display a pass has a whole beat to fit inside, so a rate line at the
tempo says only that it fit — not by how much. `--bench` runs the same passes
with nothing pacing them: 600 frames after a warm-up, the rig stepped and
presented into a target the size of the display. What it leaves out is a
frame's edges rather than its loop — handing the frame to the compositor, and
the upload of the seed, which is a conversion and two writes of a whole frame
every frame. It opens no input at all: the seed's layer stays black, which a
tap samples for what it would charge a picture, so the number is the passes
and not the wire.

The bank is what grows. At eight bytes a texel it holds every monitor twice —
a pass reads every layer while writing one — plus once more per frame of the
delay units' reach and per pass of the longest hold, and the seed's own layer
past the ring: 21 layers, which is 1.3 GiB at 4K against a 2 GiB cap. A ring
deeper than the cap is refused at load with both halves of why in the message,
since neither the resolution nor the depth alone is what went wrong.

## Playing it

**The control surface is the instrument.** There is no keyboard: if a control
is not on the board it does not exist. The card prints on startup;
the surface's cycle button shows the same panel on the glass, drawn as it is actually mapped and each
control captioned in a couple of words. Every knob logs its new value on
change, and the log line is the numeric readout: the focused camera, monitor
and switcher, every knob on them, and whether that monitor is on program or
direct. The glass shows one thing of the focus itself: in the tiled bank the
focused monitor's tile is framed with a thin line, so a glance finds which
glass the faders are on. A solo has nothing to pick out and draws none.

The knobs act on the focused camera (where it stands on its shaft, and its
delay), the focused monitor (its front panel) and the focused switcher (its
crossfade and its period). The three select rows pick a node of any of the
three outright.

The front panel starts neutral, so the instrument out of the box is the rig
described above and nothing else. Turn one against it: the saturation fader
swept is the quickest way to see what a stage inside the loop does.

Zoom is the sensitive one. A few thousandths either side of `zoom 1.000` is
the difference between an image that walks inward, one that stands still, and
one that blows outward — and since camera A and camera 3 share a shaft, a
thousandth there moves both readings at once.

The **period** is the original's mode on a switcher column: every that many
passes the switcher reverses itself, counted on one grid from the start of the
run rather than from when the period was dialled in, so every switcher in the
mode beats together. A pass is a beat of the tempo, so nothing in it reads a
clock. There is no latch beside the knob — the board is full, and a period at
its floor is the off switch.

## Tests

```
nix-shell --run "cargo test"
```

The transform, parameter, tempo and letterbox tests are pure. The rig is
checked as arithmetic: a monitor on direct shows its own camera whatever the
switchers say, the rotating monitor shows camera B whatever the setting, the
seed reaches a B monitor only through the whole chain, a B monitor on program
is that chain multiplied out, every feed sums to one at every setting of the
eight levers, the performance matrix is these rows — written out rather than
re-derived, so a wrong wire cannot agree with itself — and camera 3 moves with
camera A and cannot be moved alone.

The tests in `tests/` render on a real GPU and read the pixels back: that the
seed lights the monitor where it says it does, that the previous frame comes
back round, that the gain is applied once per pass, that what a camera sees
past a monitor's edge is black, that the seed stays round on a non-square
monitor, that the instrument settles without clipping, and that each colour
knob does its own job — saturation greys without dimming, hue moves light
between the channels at constant luma, contrast leaves mid-grey where it is
while a gain would not, brightness lifts black itself, the temperature leaves
a grey grey at rest and warms or cools it at the rails without moving its
luma, a level pushed below black comes back black, and the whole colour stage
is inert at its defaults and inside the loop. Sharpness leaves the frame byte
for byte at rest, steepens the seed's rim and a step both ways when turned up,
and reaches one texel and no further. The rails hold an overdriven loop: at a gain
well over unity it settles on them instead of running to an infinity.

The wiring gets the same treatment: the matrix sends each camera across, a
crossfade delivers the fractions it names, a beam splitter blends two monitors
into one camera, a structure takes half of the other through the cross-link, a
router-output flip mirrors what the monitor is handed, and a solo puts one
monitor on the whole target. So does the seed: it shows on the monitor the
switcher sends it to, its layer is current however the ring turns and sits
past the whole ring, blanking the monitors leaves it alone, the switcher sums
it with a camera on one monitor, it arrives whole however the cameras are set,
and the luma key cuts the dark, passes the bright and blends the edge between.
A camera with a frame delay hands on the frame it saw that many passes ago,
byte for byte, and blanking empties the ring under a flash still in flight. On
a machine with no adapter each one prints the reason straight to the
process's stderr and returns; libtest still counts them as passed.

The capture path is tested by writing a file and decoding it back. The input
decoding is tested without a GPU: the drawn test pattern against the levels
it names, the ffmpeg command line against the options that make it letterbox
and stay off the network rather than race and stretch, a capture source against
what
ffmpeg was told to generate, a pipe that keeps delivering past the two buffers
it owns, and a source that will not open refused at once rather than after the
first-frame timeout. Outside the pinned shell the ones that need ffmpeg print
a skip, on the same terms as the GPU tests.

The surface is tested without one plugged in, at every layer and then through
all of them at once. The decoder against the ways a fader sweep actually
arrives — three bytes split across reads, running status with the status byte
left off, a clock byte landing between a control number and its value, a scene
dump that must not read as a hundred knob moves, and notes and bends that are
not knobs. The turn against a fader's first word placing it and not moving
anything, a full throw at every rung of the precision ladder, the clutch
holding every control still and letting go without a jump, a whole-frame knob
owed a frame at a time, and an unplug that throws nothing and lets go of every
button a finger was on. The factory map is asserted pair by pair — coverage
alone let hue and brightness swap CCs and `blank` and `reset` swap buttons,
a surface whose silkscreen lies with every behaviour test still green — and
every select row is checked to be exactly as wide as its kind. The map is
tested against a duplicate binding, a command nothing answers to, and a
literal file rather than a round trip, because a round trip agrees with itself
whatever the fields are called. The card search runs against a
`/proc/asound/cards` with two other cards that also have raw MIDI devices. And
the whole path — discovery, the open, the reader thread, the decode, the map
and the turn — against a device that is not there when the instrument starts,
appears, sends a sweep down a pipe, and goes away again, which is what
hot-plug is.

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

## In a browser

The same instrument, on WebGPU, at
<https://bddap-bot.github.io/lightherder/>. The tempo is chosen the way a page
takes an argument, `?rate=30` — the same range `--rate` takes, refused rather
than clamped outside it — and nothing else is: there is one instrument. What
is not there is what a browser has no way to give it: the ALSA control
surface, so a tab plays and nothing turns a knob. The seed it does have: where
a terminal runs ffmpeg on `/dev/video0`, a page plays a `<video>` of its own
camera — asked for with `getUserMedia` — and reads it back through a canvas
into the same bytes, so the visitor is what sparks the loops.

`web/build.sh` builds `web/dist` — the module, its glue and the page — and
every push to `main` runs it and publishes the result. Locally:

    nix-shell --run ./web/build.sh
    python3 -m http.server -d web/dist

The `wasm-bindgen` crate is pinned to the patch because the generator of that
glue must be the same version; the dev shell carries that exact binary and
`web/build.sh` refuses anything else.
