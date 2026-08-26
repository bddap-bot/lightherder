//! The knobs on the instrument. No windowing, no GPU — a MIDI surface drives
//! the same values a keyboard does.

use std::borrow::Cow;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::affine::Framing;
use crate::input::Input;
use crate::motion::{Lfo, Shape};

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
    /// The beam splitter in front of the lens: how much of each source this
    /// camera sees, indexed the way [`Params::sources`] counts them — the
    /// monitors, then the external inputs. `[1.0]`-style one-hots are a
    /// camera aimed straight at one source; two non-zero entries are a
    /// camera looking through beam-splitter glass at a pair.
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
    /// External sources the cameras can be aimed at alongside the monitors:
    /// test patterns, video files, capture devices. A source and nothing
    /// else — nothing draws to one and no knob turns one — so an input takes
    /// no routing column and no part in the loop's gain. It is light entering
    /// the graph, like the seed spot, rather than light going round it.
    #[serde(default)]
    pub inputs: Vec<Input>,
    /// The routing matrix: `routing[m][c]` is how much of camera `c`'s output
    /// monitor `m` displays. A permutation matrix is a plain switcher; rows
    /// with several non-zero entries mix cameras on one monitor.
    pub routing: Vec<Vec<f32>>,
    /// The knobs that turn themselves. Offsets on the values above rather
    /// than a second copy of them, so this is state the way a knob's angle is
    /// state — saved, recalled and edited like the rest of the panel.
    #[serde(default)]
    pub motion: Vec<Lfo>,
}

impl Default for Params {
    fn default() -> Self {
        crate::config::single()
    }
}

/// Which camera and which monitor the knobs act on. Named fields on purpose:
/// two bare `usize`s in a row would let a swapped pair compile and silently
/// edit the wrong node.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Focus {
    pub camera: usize,
    pub monitor: usize,
}

impl Focus {
    /// The half of the focus `knob` actually reads, with the other back at
    /// zero. A monitor knob at camera 3 and the same knob at camera 0 turn
    /// the very same value, so an automation list that told those apart would
    /// hold two entries neither of which the keys could reliably find.
    pub fn narrowed(self, knob: Knob) -> Focus {
        match knob.side() {
            Side::Camera => Focus { monitor: 0, ..self },
            Side::Monitor => Focus { camera: 0, ..self },
            // A crosspoint is the pair, so both indices name it and neither
            // can be dropped.
            Side::Edge => self,
        }
    }

    /// Back inside `params`, which may have fewer nodes than the graph this
    /// focus was walked on — a recalled preset can bring fewer cameras.
    /// `config::validate` is what guarantees there is a node to land on.
    pub fn clamped(self, params: &Params) -> Focus {
        Focus {
            camera: self.camera.min(params.cameras.len().saturating_sub(1)),
            monitor: self.monitor.min(params.monitors.len().saturating_sub(1)),
        }
    }
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
    /// The switcher crosspoint the two halves of the focus name between
    /// them: how much of the focused camera the focused monitor shows.
    Route,
}

/// Which node a knob's value belongs to, and so which half of a [`Focus`] it
/// reads. Every knob but the switcher's crosspoint lives on one node; that
/// one is an edge between a camera and a monitor, and reads both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Camera,
    Monitor,
    Edge,
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
    pub const ALL: [Knob; 20] = [
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
        Knob::Route,
    ];

    /// The one name a knob has: in the printed help, in an error, and in a
    /// config file — [`Knob`]'s serde is this function and its inverse, so a
    /// knob cannot be called one thing on the terminal and another on disk.
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
            Knob::Route => "route",
        }
    }

    pub fn from_name(name: &str) -> Option<Knob> {
        Knob::ALL.into_iter().find(|knob| knob.name() == name)
    }

    /// Which of the focus's two indices the knob reads.
    pub const fn side(self) -> Side {
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
            | Knob::Noise => Side::Camera,
            Knob::Seed
            | Knob::Hue
            | Knob::Saturation
            | Knob::Brightness
            | Knob::Contrast
            | Knob::Gamma
            | Knob::Headroom => Side::Monitor,
            Knob::Route => Side::Edge,
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
            | Knob::Headroom
            // A crosspoint runs 0 to 1 and gets swept end to end, so it wants
            // the coarse step rather than the trim one.
            | Knob::Route => 0.005,
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
            // A crosspoint is a fraction of a camera. Above 1.0 it would be
            // an amplifier, which is what the loop gain already is.
            Knob::Route => Limit::Clamp(0.0, 1.0),
        }
    }
}

/// By name, not by variant: a config file naming a knob should read the way
/// the help prints it, and deriving would give it a second spelling to drift
/// from.
impl Serialize for Knob {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.name())
    }
}

impl<'de> Deserialize<'de> for Knob {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Knob, D::Error> {
        let name = String::deserialize(deserializer)?;
        Knob::from_name(&name).ok_or_else(|| {
            let known = Knob::ALL.map(Knob::name).join(", ");
            serde::de::Error::custom(format!("no knob called {name:?}; there are {known}"))
        })
    }
}

impl Params {
    /// Everything a camera can look at: the monitors, then the inputs. The
    /// index space of [`Camera::look`] and the layer count of the source
    /// bank, which is why `Feedback::new` takes the graph rather than a
    /// count it could be handed the wrong one of.
    pub fn sources(&self) -> usize {
        self.monitors.len() + self.inputs.len()
    }

    /// Put `knob` at `value` outright, which is what a fader does: it sends
    /// where it is standing rather than which way it moved.
    ///
    /// Through [`Params::nudge`] rather than by writing the field, so a
    /// fader is subject to the very same rails, wrap and rigid three-channel
    /// step a key press is — there is nowhere a fader can put a knob that a
    /// hand could not.
    pub fn set(&mut self, knob: Knob, value: f32, focus: Focus) {
        self.nudge(knob, value - self.knob(knob, focus), focus);
    }

    /// Where `knob` is standing. The rigid gain reads as the mean of its
    /// three channels, which is the number its step slides: setting it to
    /// that mean is what leaves the colour offsets alone.
    pub fn knob(&self, knob: Knob, focus: Focus) -> f32 {
        let cam = &self.cameras[focus.camera];
        let mon = &self.monitors[focus.monitor];
        match knob {
            Knob::Zoom => cam.framing.zoom,
            Knob::Rotation => cam.framing.rotation,
            Knob::TranslateX => cam.framing.translate[0],
            Knob::TranslateY => cam.framing.translate[1],
            Knob::Gain => cam.gain.iter().sum::<f32>() / 3.0,
            Knob::GainR => cam.gain[0],
            Knob::GainG => cam.gain[1],
            Knob::GainB => cam.gain[2],
            Knob::Bloom => cam.character.bloom,
            Knob::BloomRadius => cam.character.bloom_radius,
            Knob::ChromaBleed => cam.character.chroma_bleed,
            Knob::Noise => cam.character.noise,
            Knob::Seed => mon.seed_brightness,
            Knob::Hue => mon.colour.hue,
            Knob::Saturation => mon.colour.saturation,
            Knob::Brightness => mon.colour.brightness,
            Knob::Contrast => mon.colour.contrast,
            Knob::Gamma => mon.colour.gamma,
            Knob::Headroom => mon.headroom,
            Knob::Route => self.routing[focus.monitor][focus.camera],
        }
    }

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

    /// The graph as the automation has it `seconds` into the performance:
    /// every knob offset by whatever is driving it, and nothing else changed.
    ///
    /// Recomputed from the stored knobs rather than applied to them, so a
    /// swing cannot compound and turning one off leaves its knob exactly
    /// where the hand did. A graph with no automation is handed straight
    /// back, which is what makes this free for the presets that have none.
    pub fn modulated(&self, seconds: f64) -> Cow<'_, Params> {
        if self.motion.is_empty() {
            return Cow::Borrowed(self);
        }
        let mut out = self.clone();
        for (i, lfo) in self.motion.iter().enumerate() {
            // Totalled per knob before anything is applied, rather than
            // nudged one at a time. `nudge` clamps, so a sequence of offsets
            // stops one of them against a rail that the total would have
            // cleared — and which one depends on the order the list happens
            // to be in. Skipping the ones already totalled is what keeps a
            // knob from being nudged twice.
            if self.motion[..i]
                .iter()
                .any(|before| before.same_target(lfo))
            {
                continue;
            }
            let offset: f32 = self.motion[i..]
                .iter()
                .filter(|other| other.same_target(lfo))
                .map(|other| other.offset(seconds))
                .sum();
            out.nudge(lfo.knob, offset, lfo.focus);
        }
        Cow::Owned(out)
    }

    /// Whether `other` can take over an instrument already running `self`.
    ///
    /// What a recall cannot change is what would have to be rebuilt to serve
    /// it: the bank the monitors live in, and the processes feeding the
    /// inputs. Rebuilding either blanks every loop, and a recall that stops
    /// the image is the one thing a performance cannot afford — so a slot may
    /// bring different knobs, routing, automation and even a different number
    /// of cameras, which only ever reach the GPU as taps, but a slot with a
    /// different bank is a different instrument and is started, not recalled.
    pub fn same_bank_as(&self, other: &Params) -> bool {
        self.monitors.len() == other.monitors.len() && self.inputs == other.inputs
    }

    /// The automation on `knob` at `focus`, if any. The first: a config may
    /// stack several on one knob and they sum — two offsets are a beat, not a
    /// fight — but the keys drive one.
    pub fn motion_of(&self, knob: Knob, focus: Focus) -> Option<&Lfo> {
        self.motion_index(knob, focus).map(|i| &self.motion[i])
    }

    fn motion_index(&self, knob: Knob, focus: Focus) -> Option<usize> {
        let focus = focus.narrowed(knob);
        self.motion
            .iter()
            .position(|lfo| lfo.knob == knob && lfo.focus == focus)
    }

    /// The automation switch, one key: off, then a sine, then a ramp, then
    /// off again. A cycle rather than a switch and a shape selector, because
    /// with two shapes those are the same three states and one binding is
    /// cheaper than two.
    ///
    /// `seconds` is the instrument's clock, and every state this reaches is
    /// seated to start from rest at it — switching a swing on, or changing
    /// its shape, must not jump the knob to wherever a cycle running since
    /// startup has got to.
    pub fn motion_cycle(&mut self, knob: Knob, focus: Focus, seconds: f64) {
        let i = match self.motion_index(knob, focus) {
            None => {
                self.motion.push(Lfo::new(knob, focus, Shape::Sine));
                self.motion.len() - 1
            }
            Some(i) => match self.motion[i].shape {
                Shape::Sine => {
                    self.motion[i].shape = Shape::Ramp;
                    i
                }
                Shape::Ramp => {
                    self.motion.remove(i);
                    return;
                }
            },
        };
        self.motion[i].restart(seconds);
    }

    /// Move the rate of the automation on `knob` by `steps` presses. Nothing
    /// driving that knob is nothing to speed up, not an error.
    pub fn motion_rate(&mut self, knob: Knob, focus: Focus, steps: f32, seconds: f64) {
        if let Some(i) = self.motion_index(knob, focus) {
            self.motion[i].scale_rate(steps, seconds);
        }
    }

    pub fn motion_depth(&mut self, knob: Knob, focus: Focus, steps: f32) {
        if let Some(i) = self.motion_index(knob, focus) {
            self.motion[i].nudge_depth(steps);
        }
    }

    /// The value a knob turns, for the knobs that are a single number.
    fn knob_mut(&mut self, knob: Knob, focus: Focus) -> &mut f32 {
        // One match rather than a branch on the side and a match inside each:
        // the crosspoint reads both indices, so there is no side to branch on
        // first, and the two `unreachable!` arms that split cost bought are
        // gone with it.
        match knob {
            Knob::Zoom => &mut self.cameras[focus.camera].framing.zoom,
            Knob::Rotation => &mut self.cameras[focus.camera].framing.rotation,
            Knob::TranslateX => &mut self.cameras[focus.camera].framing.translate[0],
            Knob::TranslateY => &mut self.cameras[focus.camera].framing.translate[1],
            Knob::GainR => &mut self.cameras[focus.camera].gain[0],
            Knob::GainG => &mut self.cameras[focus.camera].gain[1],
            Knob::GainB => &mut self.cameras[focus.camera].gain[2],
            Knob::Bloom => &mut self.cameras[focus.camera].character.bloom,
            Knob::BloomRadius => &mut self.cameras[focus.camera].character.bloom_radius,
            Knob::ChromaBleed => &mut self.cameras[focus.camera].character.chroma_bleed,
            Knob::Noise => &mut self.cameras[focus.camera].character.noise,
            Knob::Seed => &mut self.monitors[focus.monitor].seed_brightness,
            Knob::Hue => &mut self.monitors[focus.monitor].colour.hue,
            Knob::Saturation => &mut self.monitors[focus.monitor].colour.saturation,
            Knob::Brightness => &mut self.monitors[focus.monitor].colour.brightness,
            Knob::Contrast => &mut self.monitors[focus.monitor].colour.contrast,
            Knob::Gamma => &mut self.monitors[focus.monitor].colour.gamma,
            Knob::Headroom => &mut self.monitors[focus.monitor].headroom,
            Knob::Route => &mut self.routing[focus.monitor][focus.camera],
            Knob::Gain => unreachable!("nudge() splits Gain into its channels"),
        }
    }

    /// The focused camera and monitor, every knob's value in one line: the
    /// only readout the instrument has.
    pub fn describe(&self, focus: Focus) -> String {
        let cam = &self.cameras[focus.camera];
        let mon = &self.monitors[focus.monitor];
        // Every one of them, not the focused node's: automation the readout
        // does not mention is a knob moving for no visible reason.
        let motion: String = self
            .motion
            .iter()
            .map(|lfo| format!("\n{}", lfo.describe()))
            .collect();
        format!(
            // Two lines rather than one: at twenty knobs a single line
            // wraps in a terminal, and consecutive presses stop lining up —
            // which was the only thing a single line was buying.
            "cam {}/{}: zoom {:.3}  rot {:+.3}  pan {:+.3},{:+.3}  gain {:.3},{:.3},{:.3}  \
             bloom {:.3}  radius {:.3}  bleed {:.3}  noise {:.3}\n\
             mon {}/{}: seed {:.3}  hue {:+.3}  sat {:.3}  bright {:+.3}  contrast {:.3}  \
             gamma {:.3}  headroom {:.3}\n\
             route {:.3}: how much of cam {} mon {} shows{}",
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
            self.routing[focus.monitor][focus.camera],
            focus.camera + 1,
            focus.monitor + 1,
            motion,
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
            // Both ways: a knob whose default sits on a rail — the switcher's
            // crosspoint does, at a full send — has room in one direction
            // only, and a knob that moves in neither is the broken one.
            let moved = [0.01f32, -0.01].map(|delta| {
                let mut params = p();
                nudge(&mut params, knob, delta);
                params != p()
            });
            assert!(moved.iter().any(|m| *m), "{knob:?} did nothing");
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
            // Away from whichever rail the default is on, so a knob with no
            // room upward is still moved.
            let delta = match knob.limit() {
                Limit::Clamp(_, high) if params.knob(knob, Focus::default()) >= high => -0.05,
                _ => 0.05,
            };
            nudge(&mut params, knob, delta);
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
    fn a_knob_is_the_same_name_on_disk_as_on_the_terminal() {
        for knob in Knob::ALL {
            assert_eq!(Knob::from_name(knob.name()), Some(knob));
            let written = toml::to_string(&Lfo::new(knob, Focus::default(), Shape::Sine)).unwrap();
            assert!(
                written.contains(&format!("knob = \"{}\"", knob.name())),
                "{knob:?} writes itself as {written}"
            );
        }
        assert_eq!(Knob::from_name("no such knob"), None);
    }

    #[test]
    fn no_two_knobs_share_a_name() {
        // The name is the knob's identity on disk, so a duplicate would make
        // one of the two unreachable from a config file.
        for (i, knob) in Knob::ALL.iter().enumerate() {
            let clash = Knob::ALL[..i]
                .iter()
                .any(|other| other.name() == knob.name());
            assert!(!clash, "{knob:?} shares its name");
        }
    }

    #[test]
    fn a_focus_narrows_to_the_half_its_knob_reads() {
        let focus = Focus {
            camera: 3,
            monitor: 5,
        };
        for knob in Knob::ALL {
            let narrowed = focus.narrowed(knob);
            let expected = match knob.side() {
                Side::Camera => Focus {
                    camera: 3,
                    monitor: 0,
                },
                Side::Monitor => Focus {
                    camera: 0,
                    monitor: 5,
                },
                // A crosspoint is the pair, so neither index is dropped.
                Side::Edge => focus,
            };
            assert_eq!(narrowed, expected, "{}", knob.name());
            assert_eq!(narrowed.narrowed(knob), narrowed, "{}", knob.name());
        }
    }

    #[test]
    fn the_automation_switch_cycles_and_the_keys_find_what_it_made() {
        let mut params = crate::config::crossed();
        let focus = Focus {
            camera: 1,
            monitor: 1,
        };
        for knob in Knob::ALL {
            params.motion_cycle(knob, focus, 0.0);
            let lfo = params.motion_of(knob, focus).expect("switched on");
            assert_eq!(lfo.shape, Shape::Sine);
            // Found from the other half of the focus too: a monitor knob's
            // automation must not go missing because the camera focus moved.
            let elsewhere = Focus {
                camera: 0,
                monitor: 0,
            };
            let moved = match knob.side() {
                Side::Camera => Focus {
                    monitor: 0,
                    ..focus
                },
                Side::Monitor => Focus { camera: 0, ..focus },
                Side::Edge => focus,
            };
            assert!(params.motion_of(knob, moved).is_some());
            assert_eq!(
                params.motion_of(knob, elsewhere).is_some(),
                moved == elsewhere
            );

            params.motion_cycle(knob, focus, 0.0);
            assert_eq!(params.motion_of(knob, focus).unwrap().shape, Shape::Ramp);
            params.motion_cycle(knob, focus, 0.0);
            assert!(params.motion_of(knob, focus).is_none(), "{knob:?} stuck on");
        }
        assert!(params.motion.is_empty(), "the list is not being emptied");
    }

    #[test]
    fn the_automation_keys_move_the_lfo_the_focus_names() {
        let mut params = crate::config::crossed();
        let (a, b) = (
            Focus {
                camera: 0,
                monitor: 0,
            },
            Focus {
                camera: 1,
                monitor: 0,
            },
        );
        params.motion_cycle(Knob::Zoom, a, 0.0);
        params.motion_cycle(Knob::Zoom, b, 0.0);
        let before = *params.motion_of(Knob::Zoom, b).unwrap();
        params.motion_rate(Knob::Zoom, a, 3.0, 0.0);
        params.motion_depth(Knob::Zoom, a, -1.0);
        let after = params.motion_of(Knob::Zoom, a).unwrap();
        assert!(after.rate > before.rate && after.depth < before.depth);
        assert_eq!(
            params.motion_of(Knob::Zoom, b),
            Some(&before),
            "hit camera 2"
        );
        // A knob nothing is driving is nothing to speed up.
        params.motion_rate(Knob::Hue, a, 3.0, 0.0);
        assert!(params.motion_of(Knob::Hue, a).is_none());
    }

    #[test]
    fn a_recall_may_change_everything_but_the_bank() {
        let running = crate::config::external();
        // The knobs, the routing, the automation and the camera count are
        // all a recall's to change: none of them is a texture or a process.
        let mut slot = running.clone();
        slot.monitors[0].colour.hue = 1.0;
        slot.cameras.pop();
        slot.routing[0].pop();
        slot.motion = vec![Lfo::new(Knob::Hue, Focus::default(), Shape::Ramp)];
        assert!(running.same_bank_as(&slot));

        // The bank is not: another monitor is another texture layer, and
        // another input is another process nothing has started.
        let mut more_monitors = running.clone();
        more_monitors.monitors.push(Monitor::default());
        assert!(!running.same_bank_as(&more_monitors));

        let mut other_input = running.clone();
        other_input.inputs = vec![crate::input::Input::Pattern(crate::input::Pattern::Grid)];
        assert!(!running.same_bank_as(&other_input));
        // Same count, different thing plugged in: the running ffmpeg is
        // still the old one, so a count alone would let this through.
        assert_eq!(other_input.inputs.len(), running.inputs.len());

        let mut no_inputs = running.clone();
        no_inputs.inputs.clear();
        assert!(!running.same_bank_as(&no_inputs));
    }

    #[test]
    fn a_focus_lands_inside_the_graph_it_is_pointed_at() {
        let focus = Focus {
            camera: 3,
            monitor: 3,
        };
        assert_eq!(focus.clamped(&crate::config::insanity()), focus);
        assert_eq!(focus.clamped(&Params::default()), Focus::default());
        // A graph with different counts on the two sides, from a focus with
        // different indices: symmetric cases cannot tell the two apart, and
        // this is the shape a recall actually lands on.
        let lopsided = crate::config::external(); // two cameras, one monitor
        assert_eq!(
            Focus {
                camera: 1,
                monitor: 3
            }
            .clamped(&lopsided),
            Focus {
                camera: 1,
                monitor: 0
            }
        );
        assert_eq!(
            Focus {
                camera: 5,
                monitor: 0
            }
            .clamped(&lopsided),
            Focus {
                camera: 1,
                monitor: 0
            }
        );
    }

    #[test]
    fn the_log_line_shows_every_running_lfo() {
        // Automation the readout does not mention is a knob moving for no
        // reason a performer can see.
        let mut params = crate::config::crossed();
        // On camera 2 and monitor 2, so the line has to name the node it is
        // really on rather than the 1 that a default focus prints either way.
        let far = Focus {
            camera: 1,
            monitor: 1,
        };
        for knob in Knob::ALL {
            params.motion_cycle(knob, far, 0.0);
        }
        // Read from the focus at the *other* corner: every LFO is listed, not
        // the focused node's.
        let line = params.describe(Focus::default());
        for knob in Knob::ALL {
            let lfo = params.motion_of(knob, far).expect("switched on");
            assert!(
                line.contains(&lfo.describe()),
                "the readout is missing {:?}\n{line}",
                lfo.describe()
            );
            assert!(match knob.side() {
                Side::Camera => line.contains("cam 2"),
                Side::Monitor => line.contains("mon 2"),
                Side::Edge => line.contains("cam 2 on mon 2"),
            });
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

    #[test]
    fn the_reader_and_the_writer_of_a_knob_are_the_same_field() {
        // `knob` matches the nineteen fields over again, next to the match
        // `knob_mut` makes. Nothing but this stops one of them being pointed
        // at the wrong number: a nudge that the reader does not see, or sees
        // on another knob, is the whole failure.
        for knob in Knob::ALL {
            let mut params = crate::config::crossed();
            let focus = Focus {
                camera: 1,
                monitor: 1,
            };
            let before: Vec<f32> = Knob::ALL
                .iter()
                .map(|other| params.knob(*other, focus))
                .collect();
            let step = knob.increment();
            params.nudge(knob, step, focus);
            for (other, was) in Knob::ALL.into_iter().zip(before) {
                let now = params.knob(other, focus);
                // The rigid gain and its three channels are one value seen
                // four ways, so turning any of them moves more than one.
                let gain_family =
                    |k: Knob| matches!(k, Knob::Gain | Knob::GainR | Knob::GainG | Knob::GainB);
                let expected = if other == knob {
                    was + step
                } else if gain_family(knob) && gain_family(other) {
                    continue;
                } else {
                    was
                };
                assert!(
                    (now - expected).abs() < 1e-6,
                    "turning {} moved {} from {was} to {now}",
                    knob.name(),
                    other.name()
                );
            }
        }
    }

    #[test]
    fn a_fader_puts_a_knob_exactly_where_it_says() {
        // What `set` is for, and the reason it goes through `nudge`: the
        // value lands, and it lands inside the rails.
        let mut params = p();
        let focus = Focus::default();
        params.set(Knob::Contrast, 2.5, focus);
        assert!((params.knob(Knob::Contrast, focus) - 2.5).abs() < 1e-6);
        // Past a rail is the rail, not the number asked for.
        params.set(Knob::Contrast, 99.0, focus);
        assert!((params.knob(Knob::Contrast, focus) - 4.0).abs() < 1e-6);
        // A phase comes back round instead.
        params.set(Knob::Hue, 3.0, focus);
        assert!((params.knob(Knob::Hue, focus) - 3.0).abs() < 1e-5);
    }

    #[test]
    fn setting_the_rigid_gain_slides_the_channels_and_keeps_their_offsets() {
        let mut params = p();
        let focus = Focus::default();
        let before = params.cameras[0].gain;
        let offsets = [before[1] - before[0], before[2] - before[0]];
        params.set(Knob::Gain, 0.5, focus);
        let after = params.cameras[0].gain;
        assert!((params.knob(Knob::Gain, focus) - 0.5).abs() < 1e-6);
        assert!((after[1] - after[0] - offsets[0]).abs() < 1e-6);
        assert!((after[2] - after[0] - offsets[1]).abs() < 1e-6);
    }

    #[test]
    fn the_crosspoint_knob_is_the_cell_both_halves_of_the_focus_name() {
        // The one knob that reads the whole focus. On a graph whose routing
        // matrix is not symmetric, so a transposed index would show.
        let mut params = crate::config::crossed();
        let focus = Focus {
            camera: 0,
            monitor: 1,
        };
        assert_eq!(params.routing[1][0], 1.0);
        assert_eq!(params.routing[0][1], 1.0);
        params.set(Knob::Route, 0.25, focus);
        assert!((params.routing[1][0] - 0.25).abs() < 1e-6);
        assert_eq!(params.routing[0][1], 1.0, "the transpose moved");
        assert!((params.knob(Knob::Route, focus) - 0.25).abs() < 1e-6);
        // And it follows both halves: the other corner is its own cell.
        let other = Focus {
            camera: 1,
            monitor: 0,
        };
        assert!((params.knob(Knob::Route, other) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn a_crosspoint_can_be_automated_on_the_pair_it_names() {
        // An LFO on an edge knob keeps both indices, where a camera or a
        // monitor knob drops one — so two crosspoints of one camera are two
        // automations rather than one that overwrites the other.
        let mut params = crate::config::crossed();
        let first = Focus {
            camera: 0,
            monitor: 0,
        };
        let second = Focus {
            camera: 0,
            monitor: 1,
        };
        params.motion_cycle(Knob::Route, first, 0.0);
        params.motion_cycle(Knob::Route, second, 0.0);
        assert_eq!(params.motion.len(), 2);
        assert!(params.motion_of(Knob::Route, first).is_some());
        assert!(params.motion_of(Knob::Route, second).is_some());
        // The readout names the pair, not one of them.
        let line = params.motion_of(Knob::Route, second).unwrap().describe();
        assert!(line.contains("cam 1 on mon 2"), "{line}");
    }
}
