# Deployed browser verification

These frames were captured from the deployed page in headed Chromium with a
non-fallback Vulkan adapter and individually viewed before publication.
The seed is Chromium's synthetic camera animation; its clock is elapsed test
video time. All controls use explicitly synthetic nanoKONTROL2 Web MIDI input.

- `xv-0-start.png`: five visible monitors, with monitor 1 outlined.
- `xv-1-overlay.png`: Cycle (CC 46) shows the graph and control card.
- `xv-2-solo.png`: Forward (CC 44) fills the view with monitor 1.
- `xv-3-knob.png`: back in tiled view, CC 0 shifts monitor 1 toward yellow-green;
  the other four remain green. The logged hue reaches +0.693.

See AGENTS.md for the headed capture setup. These frames establish rendering
and synthetic control input, not a physical-board or throughput benchmark.
