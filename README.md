# lightherder

A GPU video-feedback instrument in Rust and wgpu: virtual cameras and monitors
recirculate images into spirals, rings and repeating patterns. This is an
unaffiliated software homage to Dave Blair's [The Light Herder](https://www.thelightherder.com/),
realizing its feedback rig in software; Blair's instrument creates its images
through analog video and optical feedback.

## What it looks like

Existing recipe comparisons: Blair's original on the left, this software on the
right ([sources](recipes/SOURCES.md)).

![Spiral feedback: original and software rendering](recipes/single-spiral.png)

![Ring feedback: original and software rendering](recipes/magenta-donut.png)

## Build and run

From the checkout, with Nix installed and a graphics driver available:

```sh
nix-shell --run 'cargo build --release'
nix-shell --run 'cargo run --release -- --windowed --seed bars'
```

This opens a window with a generated test pattern as its input. Close the window
to quit. Plug in a Korg nanoKONTROL2 to play the instrument on Linux.

The web build (`web/build.sh`) plays from the same board in a browser with Web
MIDI, such as Chrome or Edge, once the page is allowed MIDI access with system
exclusive. Its monitors share 1 GiB of GPU memory, so delay reaches 23 frames.

| Option | Effect |
| --- | --- |
| `--windowed` | Open a window; fullscreen is the default. |
| `--resolution WIDTHxHEIGHT` | Set each virtual monitor's size; default `1920x1080`. Delay (rotary 3) reaches 30 frames, or as many as fit in the monitors' 2 GiB of GPU memory: 7 at `3840x2160`. Larger monitors give up delay first, then the slowest shutters. |
| `--seed bars` | Use the built-in test pattern. |
| `--seed FORMAT:NAME` | Read an ffmpeg input, e.g. `lavfi:testsrc2`; default `v4l2:/dev/video0`. |
| `--bench` | Time 600 frames off screen and exit. |
| `--help` | Print command-line usage. |

## Presets and controls

The instrument starts at identity, passing the input through the monitor bank.
[Recipes](recipes/README.md) give control sequences for eight looks; there is no
preset selector. The nanoKONTROL2 controls below act on the selected camera,
monitor or switcher; Cycle shows their values on screen.

| Control | Action |
| --- | --- |
| S1–S3 | Select camera A, B or 3. |
| S4 | Switch the camera's feed into the monitor between its delay unit's delayed and live outputs; lit on delayed. |
| M1–M5 | Select upper A, lower A, upper B, lower B or the rotating monitor. |
| R1–R4 | Select switcher A, B, C or D. |
| Faders 1–6 | Monitor hue, saturation, brightness, contrast, temperature, sharpness. |
| Faders 7–8 | Switcher reversal period (0–60 passes; 0 disables it and undoes a reversal it still holds), crossfade from In1 (down) to In2 (up). |
| Rotaries 1–3 | Camera slide toward or away from its monitors, rotation, delay (0–30 frames). A and 3 are on one shaft, so they share slide and rotation; 3 has no delay unit. |
| Track ◀ / ▶ | Zoom the camera's lens out / in, 28–70 mm: a press changes the focal length by up to 3%, less at finer precision. The lens magnifies its own camera only; 3's is fixed. |
| Rotary 4 | Monitor frame rate: 60, 50, 30 or 24. |
| Rotary 5 | Precision: a full movement spans 1/64 to all of a continuous control's range. |
| Rotary 6 | Switcher key gain, 1–1000: raise to harden the key's edge. |
| Rotary 7 | Switcher cut length: the switcher reverses back this many passes after each reversal of its period (0–60; at 0, or at least the period, it waits for the next reversal). |
| Rotary 8 | Switcher key clip, 0–1 (0 is off): In1 shows through wherever In2's luma is under the clip or, on chroma, wherever In2 carries more than 1 − clip of a blue screen's colour. |
| R5 / Marker ◀ | Reverse the switcher crossfade / reverse while held. |
| S5 | Tap in: lighting S5 empties the switcher's one-bar pattern (128 passes, about 2.1 s); while it is lit, each R5 press adds a beat, and the switcher reverses on every beat, every bar. Press S5 again, or select another switcher, to keep the pattern; press S5 twice to clear it. |
| S6 | While lit, each beat R5 adds lands on the nearest sixteenth of the bar. |
| S7 | Toggle the switcher's key between luma and chroma; lit on chroma. |
| S8 | Step the camera's shutter through 1/60, 1/30 and 1/24, then back to 1/60; lit when slower than 1/60. The frame rate stays as rotary 4 sets it; each camera frame averages the light over the shutter's time, so whatever moves in its view smears. |
| R6 / R7 | Flip the monitor horizontally / vertically. |
| R8 | Select direct camera feed or switcher output on a structure monitor. |
| Rewind / Stop | Reset the last knob moved / reset all knobs and patterns. |
| Marker ▶ | Clear the monitors. |
| Forward / Cycle | Toggle solo monitor view / control overlay. |
| Marker Set / Record | Save a still / record while held, in `~/lightherder`. |
| Play | While Play is lit, record how faders 1–6 move on every monitor, keeping the last ten minutes; press again to loop those moves, and again to stop the loop and record anew. Moving or resetting a looping fader takes it out of the loop. |

All faders, and every rotary but 5, change values by movement. A switcher reverses on
each beat of its period and of its pattern, and keeps its period, cut length
and pattern while R1–R4 select another. To start feedback, select switcher B
with R2 and lower fader 8, then
select camera B with S2 and turn its slide and rotation slightly.

## Topology

Three virtual cameras only ever watch monitors: A and B each see a pair through
50/50 glass, and camera 3 watches the fifth, rotating monitor.
Four switchers route camera feeds back to the monitors, with the fifth monitor
always showing camera B.
Each switcher crossfades between In1, a camera, and In2: camera B on A, the next
switcher's output on B and C, and the external input on D, the one way outside light
enters. Each can key In2 over In1; only D's key starts on, keying the input over
camera 3 by luma.
Cameras A and B each feed a frame delay unit, and at each place their feeds
reach — three for A, four for B — the router takes the unit's delayed output
or its live one. All seven start on delayed, so a delay dialled in reaches
everywhere its camera goes until S4 switches a feed to live.
