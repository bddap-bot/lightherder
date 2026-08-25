//! The knobs on the instrument. No windowing, no GPU — a MIDI surface drives
//! the same values a keyboard does.

use serde::{Deserialize, Serialize};

use crate::affine::Framing;

/// The colour controls on one monitor's front panel, in the order an analog
/// signal meets them: chroma decode, video amplifier, phosphor.
///
/// None of these is the loop gain wearing a hat. The gain is a per-channel
/// multiply of the light coming *back*, and is what puts any chroma into a
/// white seed's trail at all; these turn the chroma that is already there.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Colour {
    /// Phase of the chroma subcarrier, in radians. Turning it a little each
    /// pass walks a feedback trail through the spectrum.
    pub hue: f32,
    /// Amplitude of the chroma subcarrier. 0 is monochrome; above 1 pushes
    /// colours past the gamut the camera handed over.
    pub saturation: f32,
    /// Black level: light added to every texel, which no gain can do. This is
    /// the knob that lifts a dying loop back off the floor.
    pub brightness: f32,
    /// Gain about mid-grey, which is what gives it a fixed point the loop
    /// gain has not got. See the shader for why that is the distinction that
    /// matters.
    pub contrast: f32,
    /// Phosphor transfer exponent. Above 1 the dark end is crushed and the
    /// trails thin out; below 1 they lift and smear.
    pub gamma: f32,
}

impl Default for Colour {
    fn default() -> Self {
        Colour::NEUTRAL
    }
}

/// FCC NTSC luma and the two colour-difference axes. Row 0 is luma; rows 1
/// and 2 are the real and imaginary parts of one chroma subcarrier, which is
/// what lets hue be a phase.
const DECODE: [[f64; 3]; 3] = [
    [0.299, 0.587, 0.114],
    [0.5959, -0.2746, -0.3213],
    [0.2115, -0.5227, 0.3112],
];

/// Its exact inverse, to more digits than anyone publishes. The published
/// four-figure version is off by 1e-4, which sounds like nothing until it
/// runs inside a loop that feeds itself: a hundred passes of it costs a fifth
/// of the blue channel.
const ENCODE: [[f64; 3]; 3] = [
    [1.0, 0.9560502263958943, 0.6207549413271234],
    [1.0, -0.27205234368892417, -0.6472057134551779],
    [1.0, -1.1067043153243328, 1.704421283696311],
];

/// NTSC luma, in the precision the shader wants it. Row 0 of [`DECODE`] and
/// nowhere else: the chroma bleed needs it too, and a second copy in WGSL
/// would be a constant nothing could keep in step with this one.
pub fn luma_row() -> [f32; 3] {
    std::array::from_fn(|i| DECODE[0][i] as f32)
}

impl Colour {
    /// Every stage off, so the pass writes back what the camera gave it.
    pub const NEUTRAL: Colour = Colour {
        hue: 0.0,
        saturation: 1.0,
        brightness: 0.0,
        contrast: 1.0,
        gamma: 1.0,
    };

    /// The 3x3 the shader multiplies RGB by: decode, turn the chroma by hue
    /// and scale it by saturation, encode back. Indexed `m[row][col]`.
    ///
    /// Composed here, in f64, once a frame — not left as three steps in the
    /// shader. Chained in f32 per fragment the three matrices leave a
    /// ten-thousandth of the signal behind on a value near half scale, which
    /// is under half a step of the loop's half-float storage until it is not,
    /// and then the loop ratchets one step down per pass. Composed first, the
    /// neutral case is exactly the identity and there is nothing to ratchet.
    pub fn chroma_matrix(&self) -> [[f32; 3]; 3] {
        let (sin, cos) = (self.hue as f64).sin_cos();
        let saturation = self.saturation as f64;
        let (turn, lift) = (saturation * cos, saturation * sin);
        std::array::from_fn(|row| {
            // Turning and scaling the subcarrier is one complex multiply, so
            // it folds into the pair of chroma weights this row encodes with.
            let (i, q) = (ENCODE[row][1], ENCODE[row][2]);
            let (i, q) = (i * turn + q * lift, q * turn - i * lift);
            std::array::from_fn(|col| {
                (DECODE[0][col] + i * DECODE[1][col] + q * DECODE[2][col]) as f32
            })
        })
    }
}

/// What one camera's signal path does to the light on its way to the
/// switcher: the lens in front of the sensor, and the composite cable behind
/// it. It lives on the camera rather than globally because a graph's paths
/// are not alike — one loop can glow and smear while its neighbour stays
/// clean, which is most of what makes two structures read as two.
///
/// The monitor's end of the same story is its [`Monitor::headroom`]: these
/// are the signal's imperfections, that one is the amplifier's.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Character {
    /// Fraction of the light the lens scatters into a halo instead of
    /// focusing. Redistributed, not added: a term that adds light is a term
    /// the loop multiplies, and a few passes later it owns the monitor.
    pub bloom: f32,
    /// Radius of that halo in the camera's own image, in screen units where
    /// the monitor is 1.0 tall. In the camera's image, so a camera that
    /// zooms or turns takes its halo with it.
    pub bloom_radius: f32,
    /// How far colour smears along the scanline, same units. Composite video
    /// carries chroma on a subcarrier with a fraction of luma's bandwidth,
    /// so the colour arrives blurred while the detail it belongs to does not.
    pub chroma_bleed: f32,
    /// Amplitude of the grain the sensor and the cable add. Signed and
    /// monochrome — it is luma noise — and added whether or not any light
    /// arrived, which is what keeps a loop that has decayed to black from
    /// staying there.
    pub noise: f32,
}

impl Character {
    /// A perfect lens, unlimited bandwidth and no grain: the path hands on
    /// exactly what the camera saw. The radius is not zero because it is
    /// only an aim point — nothing reads it until `bloom` is turned up, and
    /// a radius of zero would make that knob look broken.
    pub const CLEAN: Character = Character {
        bloom: 0.0,
        bloom_radius: 0.03,
        chroma_bleed: 0.0,
        noise: 0.0,
    };
}

impl Default for Character {
    fn default() -> Character {
        Character::CLEAN
    }
}

/// One camera in the graph: what it sees, how it frames it, and how much of
/// the light it hands on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Camera {
    /// How this camera's view is magnified, turned and shifted relative to
    /// what it looks at.
    #[serde(default = "Framing::identity")]
    pub framing: Framing,
    /// Per-channel gain applied to everything this camera sees. With the
    /// seed off, an effective per-monitor gain below 1.0 dies out and above
    /// 1.0 blooms; with the seed on the loop settles instead, brighter the
    /// closer to 1.0. The channels differ to colour the trails.
    #[serde(default = "unity_gain")]
    pub gain: [f32; 3],
    /// What this path does to the light besides scale it.
    #[serde(default)]
    pub character: Character,
    /// The beam splitter in front of the lens: how much of each monitor this
    /// camera sees, indexed by monitor. `[1.0]`-style one-hots are a camera
    /// aimed straight at one monitor; two non-zero entries are a camera
    /// looking through beam-splitter glass at a pair.
    pub look: Vec<f32>,
}

fn unity_gain() -> [f32; 3] {
    [1.0; 3]
}

/// One monitor in the graph: its front panel and its seed spot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Monitor {
    pub colour: Colour,
    /// Brightness of this monitor's seed spot, which is the only thing
    /// keeping a sub-unity loop alive.
    pub seed_brightness: f32,
    /// Where this monitor's video amplifier runs out of rails. The signal is
    /// untouched below half of it and bends asymptotically onto it above, so
    /// a loop driven past unity gain compresses into a structure instead of
    /// clipping the whole monitor to flat white — which is the difference
    /// between an analog feedback rig and a runaway multiply.
    ///
    /// A real amplifier always has rails, so there is no setting that turns
    /// this off, and [`Monitor::KNEE_AT_WHITE`] is not one pretending to be.
    pub headroom: f32,
}

impl Default for Monitor {
    fn default() -> Self {
        Monitor {
            colour: Colour::NEUTRAL,
            seed_brightness: 0.0,
            headroom: Monitor::KNEE_AT_WHITE,
        }
    }
}

impl Monitor {
    /// Twice display white. The knee is at half the headroom, so it lands
    /// exactly on 1.0: nothing a monitor can actually show is touched, and
    /// the reserve above white — which the half-float bank exists to keep —
    /// compresses onto 2.0 rather than running. That reserve is a real
    /// change from before this rail existed, and it is the point of it: the
    /// loop is bounded now, at every setting of every other knob.
    pub const KNEE_AT_WHITE: f32 = 2.0;
}

/// The whole instrument for one frame: every camera, every monitor, and the
/// switcher routing the first onto the second. This is both the live state
/// the knobs mutate and the on-disk config format — one struct, so the two
/// cannot drift.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Params {
    pub cameras: Vec<Camera>,
    pub monitors: Vec<Monitor>,
    /// The routing matrix: `routing[m][c]` is how much of camera `c`'s output
    /// monitor `m` displays. A permutation matrix is a plain switcher; rows
    /// with several non-zero entries mix cameras on one monitor.
    pub routing: Vec<Vec<f32>>,
}

impl Default for Params {
    fn default() -> Self {
        crate::config::single()
    }
}

/// Which camera and which monitor the knobs act on. Named fields on purpose:
/// two bare `usize`s in a row would let a swapped pair compile and silently
/// edit the wrong node.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Focus {
    pub camera: usize,
    pub monitor: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Knob {
    Zoom,
    Rotation,
    TranslateX,
    TranslateY,
    /// All three channels at once, rigidly.
    Gain,
    GainR,
    GainG,
    GainB,
    /// The lens's scatter, and how wide it scatters.
    Bloom,
    BloomRadius,
    ChromaBleed,
    Noise,
    Seed,
    Hue,
    Saturation,
    Brightness,
    Contrast,
    Gamma,
    Headroom,
}

/// What a knob does when it runs out of room.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Limit {
    Clamp(f32, f32),
    Wrap,
}

impl Knob {
    /// Every `for knob in ALL` test is silently vacuous for a knob missing
    /// from this list, including the ones that exist to catch omissions.
    pub const ALL: [Knob; 19] = [
        Knob::Zoom,
        Knob::Rotation,
        Knob::TranslateX,
        Knob::TranslateY,
        Knob::Gain,
        Knob::GainR,
        Knob::GainG,
        Knob::GainB,
        Knob::Bloom,
        Knob::BloomRadius,
        Knob::ChromaBleed,
        Knob::Noise,
        Knob::Seed,
        Knob::Hue,
        Knob::Saturation,
        Knob::Brightness,
        Knob::Contrast,
        Knob::Gamma,
        Knob::Headroom,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Knob::Zoom => "zoom",
            Knob::Rotation => "rotation",
            Knob::TranslateX => "pan x",
            Knob::TranslateY => "pan y",
            Knob::Gain => "loop gain",
            Knob::GainR => "loop gain, red",
            Knob::GainG => "loop gain, green",
            Knob::GainB => "loop gain, blue",
            Knob::Bloom => "bloom",
            Knob::BloomRadius => "bloom radius",
            Knob::ChromaBleed => "chroma bleed",
            Knob::Noise => "noise",
            Knob::Seed => "seed",
            Knob::Hue => "hue",
            Knob::Saturation => "saturation",
            Knob::Brightness => "brightness",
            Knob::Contrast => "contrast",
            Knob::Gamma => "gamma",
            Knob::Headroom => "headroom",
        }
    }

    /// Whether the knob lives on a camera or on a monitor, which is what
    /// decides which focus it follows.
    pub const fn is_camera(self) -> bool {
        match self {
            Knob::Zoom
            | Knob::Rotation
            | Knob::TranslateX
            | Knob::TranslateY
            | Knob::Gain
            | Knob::GainR
            | Knob::GainG
            | Knob::GainB
            | Knob::Bloom
            | Knob::BloomRadius
            | Knob::ChromaBleed
            | Knob::Noise => true,
            Knob::Seed
            | Knob::Hue
            | Knob::Saturation
            | Knob::Brightness
            | Knob::Contrast
            | Knob::Gamma
            | Knob::Headroom => false,
        }
    }

    /// One key press worth of this knob. Spelled out rather than defaulted,
    /// so a knob added later cannot quietly inherit a step nobody chose.
    pub const fn increment(self) -> f32 {
        match self {
            // A full turn of the subcarrier is a gesture rather than a trim,
            // and at the default step it would take three thousand presses.
            Knob::Hue => 0.02,
            // Coarse enough to see: a thousandth of a radian, or of a decade
            // of phosphor curve, is imperceptible.
            Knob::Rotation
            | Knob::Seed
            | Knob::Saturation
            | Knob::Contrast
            | Knob::Gamma
            | Knob::Bloom
            | Knob::Headroom => 0.005,
            Knob::Zoom
            | Knob::TranslateX
            | Knob::TranslateY
            | Knob::Gain
            | Knob::GainR
            | Knob::GainG
            | Knob::GainB
            | Knob::Brightness
            // Radii, and a hundredth of the monitor's height is already a
            // visible smear: these want a finer step than the levels do.
            | Knob::BloomRadius
            | Knob::ChromaBleed
            | Knob::Noise => 0.002,
        }
    }

    pub const fn limit(self) -> Limit {
        match self {
            // Zero would divide by zero in the sampling transform.
            Knob::Zoom => Limit::Clamp(0.25, 4.0),
            // Spinning one way for long enough must not run the number away.
            Knob::Rotation => Limit::Wrap,
            Knob::TranslateX | Knob::TranslateY => Limit::Clamp(-1.0, 1.0),
            Knob::Gain | Knob::GainR | Knob::GainG | Knob::GainB => Limit::Clamp(0.0, 1.2),
            // A lens cannot scatter more light than it was given, and past
            // 1.0 the mix extrapolates away from the image it is blurring.
            Knob::Bloom => Limit::Clamp(0.0, 1.0),
            // A halo a quarter of the monitor high is already most of the
            // screen once the loop has run it round a few times.
            Knob::BloomRadius | Knob::ChromaBleed => Limit::Clamp(0.0, 0.25),
            // Grain is added every pass and then fed back, so it compounds:
            // a tenth of full scale per pass is already snow.
            Knob::Noise => Limit::Clamp(0.0, 0.25),
            Knob::Seed => Limit::Clamp(0.0, 1.0),
            // A phase: it comes back round instead of running away.
            Knob::Hue => Limit::Wrap,
            Knob::Saturation | Knob::Contrast => Limit::Clamp(0.0, 4.0),
            // Potent inside a loop, so the rails are close: a tenth of a unit
            // added every pass floods the monitor to white in under a second.
            Knob::Brightness => Limit::Clamp(-0.5, 0.5),
            // Zero would flatten every level to 1.0, and a phosphor curve
            // worth playing lives nowhere near either rail.
            Knob::Gamma => Limit::Clamp(0.25, 4.0),
            // Zero would divide by it. The bottom of the range squeezes the
            // whole picture into the darkest eighth, which is a sound worth
            // having; the top is well clear of anything a monitor displays.
            Knob::Headroom => Limit::Clamp(0.125, 8.0),
        }
    }
}

impl Params {
    /// Turn `knob` on the focused camera or monitor — its side of the graph
    /// decides which of the two indices it follows.
    pub fn nudge(&mut self, knob: Knob, delta: f32, focus: Focus) {
        // The rigid gain knob is the one that is not a single value: clamp its
        // step once against the tightest channel, so hitting the rail slides
        // all three together instead of flattening the colour offsets.
        if knob == Knob::Gain {
            let step = rigid_gain_step(&self.cameras[focus.camera].gain, delta);
            for channel in [Knob::GainR, Knob::GainG, Knob::GainB] {
                self.nudge(channel, step, focus);
            }
            return;
        }
        let field = self.knob_mut(knob, focus);
        *field = match knob.limit() {
            Limit::Clamp(low, high) => (*field + delta).clamp(low, high),
            Limit::Wrap => wrap_pi(*field + delta),
        };
    }

    /// The value a knob turns, for the knobs that are a single number.
    fn knob_mut(&mut self, knob: Knob, focus: Focus) -> &mut f32 {
        if knob.is_camera() {
            let cam = &mut self.cameras[focus.camera];
            match knob {
                Knob::Zoom => &mut cam.framing.zoom,
                Knob::Rotation => &mut cam.framing.rotation,
                Knob::TranslateX => &mut cam.framing.translate[0],
                Knob::TranslateY => &mut cam.framing.translate[1],
                Knob::GainR => &mut cam.gain[0],
                Knob::GainG => &mut cam.gain[1],
                Knob::GainB => &mut cam.gain[2],
                Knob::Bloom => &mut cam.character.bloom,
                Knob::BloomRadius => &mut cam.character.bloom_radius,
                Knob::ChromaBleed => &mut cam.character.chroma_bleed,
                Knob::Noise => &mut cam.character.noise,
                Knob::Gain => unreachable!("nudge() splits Gain into its channels"),
                _ => unreachable!("is_camera() said so"),
            }
        } else {
            let mon = &mut self.monitors[focus.monitor];
            match knob {
                Knob::Seed => &mut mon.seed_brightness,
                Knob::Hue => &mut mon.colour.hue,
                Knob::Saturation => &mut mon.colour.saturation,
                Knob::Brightness => &mut mon.colour.brightness,
                Knob::Contrast => &mut mon.colour.contrast,
                Knob::Gamma => &mut mon.colour.gamma,
                Knob::Headroom => &mut mon.headroom,
                _ => unreachable!("is_camera() said not"),
            }
        }
    }

    /// The focused camera and monitor, every knob's value in one line: the
    /// only readout the instrument has.
    pub fn describe(&self, focus: Focus) -> String {
        let cam = &self.cameras[focus.camera];
        let mon = &self.monitors[focus.monitor];
        format!(
            // Two lines rather than one: at nineteen knobs a single line
            // wraps in a terminal, and consecutive presses stop lining up —
            // which was the only thing a single line was buying.
            "cam {}/{}: zoom {:.3}  rot {:+.3}  pan {:+.3},{:+.3}  gain {:.3},{:.3},{:.3}  \
             bloom {:.3}  radius {:.3}  bleed {:.3}  noise {:.3}\n\
             mon {}/{}: seed {:.3}  hue {:+.3}  sat {:.3}  bright {:+.3}  contrast {:.3}  \
             gamma {:.3}  headroom {:.3}",
            focus.camera + 1,
            self.cameras.len(),
            cam.framing.zoom,
            cam.framing.rotation,
            cam.framing.translate[0],
            cam.framing.translate[1],
            cam.gain[0],
            cam.gain[1],
            cam.gain[2],
            cam.character.bloom,
            cam.character.bloom_radius,
            cam.character.chroma_bleed,
            cam.character.noise,
            focus.monitor + 1,
            self.monitors.len(),
            mon.seed_brightness,
            mon.colour.hue,
            mon.colour.saturation,
            mon.colour.brightness,
            mon.colour.contrast,
            mon.colour.gamma,
            mon.headroom,
        )
    }
}

fn rigid_gain_step(gain: &[f32; 3], delta: f32) -> f32 {
    let Limit::Clamp(low, high) = Knob::Gain.limit() else {
        unreachable!("gain clamps")
    };
    let travel = gain
        .iter()
        .map(|c| if delta >= 0.0 { high - c } else { c - low })
        .fold(f32::INFINITY, f32::min)
        .max(0.0);
    delta.abs().min(travel) * delta.signum()
}

/// Into `(-pi, pi]`, so a knob spun in one direction never runs away.
fn wrap_pi(radians: f32) -> f32 {
    use core::f32::consts::{PI, TAU};
    let wrapped = (PI - radians).rem_euclid(TAU);
    PI - wrapped
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::f32::consts::PI;

    /// The single-loop preset, which is where one of every knob lives.
    fn p() -> Params {
        Params::default()
    }

    fn nudge(p: &mut Params, knob: Knob, delta: f32) {
        p.nudge(knob, delta, Focus::default());
    }

    #[test]
    fn every_knob_moves_something() {
        for knob in Knob::ALL {
            let mut params = p();
            nudge(&mut params, knob, 0.01);
            assert_ne!(params, p(), "{knob:?} did nothing");
        }
    }

    #[test]
    fn knobs_stop_at_their_limits() {
        let mut params = p();
        for _ in 0..10_000 {
            for knob in Knob::ALL {
                nudge(&mut params, knob, 1.0);
            }
        }
        let (cam, mon) = (&params.cameras[0], &params.monitors[0]);
        assert_eq!(cam.framing.zoom, 4.0);
        assert_eq!(cam.gain, [1.2; 3]);
        assert_eq!(cam.framing.translate, [1.0, 1.0]);
        assert_eq!(cam.character.bloom, 1.0);
        assert_eq!(cam.character.bloom_radius, 0.25);
        assert_eq!(cam.character.chroma_bleed, 0.25);
        assert_eq!(cam.character.noise, 0.25);
        assert_eq!(mon.headroom, 8.0);
        assert_eq!(mon.seed_brightness, 1.0);
        assert_eq!(mon.colour.saturation, 4.0);
        assert_eq!(mon.colour.brightness, 0.5);
        assert_eq!(mon.colour.contrast, 4.0);
        assert_eq!(mon.colour.gamma, 4.0);

        for _ in 0..10_000 {
            for knob in Knob::ALL {
                nudge(&mut params, knob, -1.0);
            }
        }
        let (cam, mon) = (&params.cameras[0], &params.monitors[0]);
        assert_eq!(cam.framing.zoom, 0.25);
        assert_eq!(cam.gain, [0.0; 3]);
        assert_eq!(cam.framing.translate, [-1.0, -1.0]);
        assert_eq!(cam.character.bloom, 0.0);
        assert_eq!(cam.character.bloom_radius, 0.0);
        assert_eq!(cam.character.chroma_bleed, 0.0);
        assert_eq!(cam.character.noise, 0.0);
        assert_eq!(mon.headroom, 0.125);
        assert_eq!(mon.seed_brightness, 0.0);
        assert_eq!(mon.colour.saturation, 0.0);
        assert_eq!(mon.colour.brightness, -0.5);
        assert_eq!(mon.colour.contrast, 0.0);
        assert_eq!(mon.colour.gamma, 0.25);
    }

    #[test]
    fn the_rigid_gain_knob_moves_the_way_it_is_pushed() {
        let mut params = p();
        let before = params.cameras[0].gain;
        nudge(&mut params, Knob::Gain, -0.01);
        for (after, before) in params.cameras[0].gain.iter().zip(before) {
            assert!(*after < before, "down should lower {before}, got {after}");
        }
        nudge(&mut params, Knob::Gain, 0.02);
        for (after, before) in params.cameras[0].gain.iter().zip(before) {
            assert!(*after > before, "up should raise {before}, got {after}");
        }
    }

    #[test]
    fn the_rigid_gain_knob_keeps_its_colour_offsets_at_the_rail() {
        let mut params = p();
        let gain = params.cameras[0].gain;
        let spread = [gain[1] - gain[0], gain[2] - gain[1]];
        for _ in 0..10_000 {
            nudge(&mut params, Knob::Gain, 0.01);
        }
        let gain = params.cameras[0].gain;
        assert_eq!(gain[2], 1.2, "the leading channel should reach the top");
        assert!((gain[1] - gain[0] - spread[0]).abs() < 1e-4);
        assert!((gain[2] - gain[1] - spread[1]).abs() < 1e-4);
    }

    #[test]
    fn a_knob_follows_its_own_side_of_the_graph() {
        // Two cameras and two monitors: a camera knob nudged at focus (1, 0)
        // lands on camera 1 and nowhere else, and a monitor knob on monitor 0.
        let mut params = crate::config::crossed();
        let before = params.clone();
        params.nudge(
            Knob::Zoom,
            0.01,
            Focus {
                camera: 1,
                monitor: 0,
            },
        );
        params.nudge(
            Knob::Hue,
            0.02,
            Focus {
                camera: 1,
                monitor: 0,
            },
        );
        assert_eq!(params.cameras[0], before.cameras[0]);
        assert_ne!(params.cameras[1].framing, before.cameras[1].framing);
        assert_ne!(params.monitors[0].colour, before.monitors[0].colour);
        assert_eq!(params.monitors[1], before.monitors[1]);
    }

    #[test]
    fn rotation_wraps_instead_of_running_away() {
        let mut params = p();
        params.cameras[0].framing.rotation = 0.0;
        for _ in 0..10_000 {
            nudge(&mut params, Knob::Rotation, 0.5);
            let rotation = params.cameras[0].framing.rotation;
            assert!(rotation > -PI && rotation <= PI);
        }
        assert!((params.cameras[0].framing.rotation - wrap_pi(5000.0)).abs() < 1e-2);
    }

    #[test]
    fn wrap_pi_keeps_the_boundaries_it_promises() {
        assert!((wrap_pi(PI) - PI).abs() < 1e-6);
        assert!((wrap_pi(-PI) - PI).abs() < 1e-6);
        assert!(wrap_pi(0.0).abs() < 1e-6);
        assert!((wrap_pi(PI + 0.1) - (-PI + 0.1)).abs() < 1e-5);
    }

    #[test]
    fn a_channel_knob_moves_only_its_channel() {
        let mut params = p();
        let before = params.cameras[0].gain;
        nudge(&mut params, Knob::GainG, 0.1);
        let gain = params.cameras[0].gain;
        assert_eq!(gain[0], before[0]);
        assert_eq!(gain[2], before[2]);
        assert!((gain[1] - (before[1] + 0.1)).abs() < 1e-6);
    }

    #[test]
    fn the_log_line_shows_every_knob() {
        // The log line is the only readout the instrument has, so a knob
        // missing from it is a knob that cannot be played.
        for knob in Knob::ALL {
            let mut params = p();
            let before = params.describe(Focus::default());
            nudge(&mut params, knob, 0.05);
            assert_ne!(
                params.describe(Focus::default()),
                before,
                "{} is not in the log line",
                knob.name()
            );
        }
    }

    #[test]
    fn the_log_line_names_the_focus() {
        let params = crate::config::crossed();
        assert!(params
            .describe(Focus {
                camera: 1,
                monitor: 0
            })
            .contains("cam 2/2"));
        assert!(params
            .describe(Focus {
                camera: 1,
                monitor: 0
            })
            .contains("mon 1/2"));
    }

    /// Settings that between them exercise both signs of the phase, a
    /// saturation under 1 and one over.
    const SOME_COLOURS: [Colour; 4] = [
        Colour::NEUTRAL,
        Colour {
            hue: 1.1,
            saturation: 0.3,
            ..Colour::NEUTRAL
        },
        Colour {
            hue: -2.0,
            saturation: 2.5,
            ..Colour::NEUTRAL
        },
        Colour {
            saturation: 0.0,
            ..Colour::NEUTRAL
        },
    ];

    #[test]
    fn the_encode_matrix_is_the_decode_matrix_inverted() {
        // Transcribed rather than computed, and it is the only reason the
        // neutral matrix comes out identity, so it gets checked.
        for (row, weights) in ENCODE.iter().enumerate() {
            for col in 0..3 {
                let product: f64 = weights
                    .iter()
                    .zip(DECODE)
                    .map(|(weight, axis)| weight * axis[col])
                    .sum();
                let expected = if row == col { 1.0 } else { 0.0 };
                assert!(
                    (product - expected).abs() < 1e-12,
                    "row {row} times column {col} is {product}, not {expected}"
                );
            }
        }
    }

    #[test]
    fn the_neutral_chroma_matrix_is_exactly_the_identity() {
        // Nearly is not enough. The stage runs on every pass of a loop that
        // feeds itself, so a residual does not stay a residual.
        for (row, weights) in Colour::NEUTRAL.chroma_matrix().iter().enumerate() {
            for (col, weight) in weights.iter().enumerate() {
                let expected = if row == col { 1.0 } else { 0.0 };
                assert!(
                    (weight - expected).abs() < 1e-9,
                    "m[{row}][{col}] is {weight}"
                );
            }
        }
    }

    #[test]
    fn the_chroma_matrix_holds_grey_and_luma_whatever_the_knobs_say() {
        // Grey has no chroma to turn, so every row sums to one; and the knobs
        // move light between the channels without changing how much there is,
        // so luma survives them. Both hold for every setting, which is what
        // makes hue a phase rather than a mixing knob.
        for colour in SOME_COLOURS {
            let m = colour.chroma_matrix();
            for row in m {
                let grey: f32 = row.iter().sum();
                assert!((grey - 1.0).abs() < 1e-5, "{colour:?}: row {row:?}");
            }
            for (col, luma) in DECODE[0].iter().enumerate() {
                let out: f32 = m
                    .iter()
                    .zip(DECODE[0])
                    .map(|(row, weight)| weight as f32 * row[col])
                    .sum();
                assert!(
                    (out - *luma as f32).abs() < 1e-5,
                    "{colour:?}: luma weight {col} became {out}"
                );
            }
        }
    }

    #[test]
    fn saturation_at_zero_leaves_luma_and_nothing_else() {
        let m = Colour {
            saturation: 0.0,
            ..Colour::NEUTRAL
        }
        .chroma_matrix();
        for row in m {
            for (weight, luma) in row.iter().zip(DECODE[0]) {
                assert!((weight - luma as f32).abs() < 1e-5, "row {row:?}");
            }
        }
    }

    #[test]
    fn hue_wraps_instead_of_running_away() {
        let mut params = p();
        for _ in 0..10_000 {
            nudge(&mut params, Knob::Hue, 0.5);
            let hue = params.monitors[0].colour.hue;
            assert!(hue > -PI && hue <= PI);
        }
    }
}
