//! The knobs on the instrument. No windowing, no GPU — a MIDI surface drives
//! the same values a keyboard does.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::affine::Framing;
use crate::input::Input;

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

/// The keyer on one camera's path: what this camera refuses to hand on. Two
/// keys that multiply — a luma key that cuts the dark, for a subject against
/// a black backdrop, and a chroma key that cuts one colour, for a subject
/// against a sheet. On the camera rather than on the input, because gain,
/// framing and character already are: a key is one more thing a path does to
/// the light, and a camera aimed at an input through its key is the webcam
/// use without any second kind of source appearing anywhere.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Key {
    /// The luma the key passes in full. Cutting is complete one `softness`
    /// below it, so at 0 the key passes everything exactly — which is what
    /// lets 0 be the off state without a switch beside the knob.
    pub threshold: f32,
    /// Width of both keys' soft edge — luma units on the luma key, and the
    /// same number reused on the chroma projection, whose scale is close
    /// enough that a second knob would be sprawl. Zero is a hard edge; the
    /// shader keeps its smoothstep legal on its own.
    pub softness: f32,
    /// The colour the chroma key cuts, as a phase of the chroma subcarrier —
    /// the one way this instrument names a colour, so a key colour is a hue
    /// and not three more knobs.
    pub hue: f32,
    /// How much of the key colour a pixel may carry before it is cut,
    /// measured as its chroma's projection onto the key hue. At the top of
    /// its travel — [`Key::TOLERANT`], past anything a frame can carry — the
    /// key is off, which is what makes it the default.
    pub tolerance: f32,
}

impl Key {
    /// Above the widest chroma projection an RGB frame can carry (0.633, at
    /// the saturated corner nearest the I axis), so a tolerance at this rail
    /// cuts nothing — and the tap flattening zeroes the key vector there
    /// outright, so even an over-bright loop signal is passed.
    pub const TOLERANT: f32 = 0.7;

    /// Both keys off: the path hands on everything, exactly. The softness is
    /// not zero for the same reason `Character::CLEAN`'s bloom radius is not:
    /// it is only an aim point while the keys are off, and a default of zero
    /// would land the threshold knob hard-edged on its first press.
    pub const OFF: Key = Key {
        threshold: 0.0,
        softness: 0.05,
        hue: 0.0,
        tolerance: Key::TOLERANT,
    };
}

impl Default for Key {
    fn default() -> Key {
        Key::OFF
    }
}

/// The RGB row that measures "how much of hue `h`" a pixel's chroma carries:
/// the two subcarrier axes blended at the hue's phase. Composed in f64 from
/// [`DECODE`] like the chroma matrix, so the crate keeps one copy of the
/// axes — and grey lands on exactly zero, both rows summing to nothing.
pub fn key_weights(hue: f32) -> [f32; 3] {
    let (sin, cos) = (hue as f64).sin_cos();
    std::array::from_fn(|i| (cos * DECODE[1][i] + sin * DECODE[2][i]) as f32)
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
    /// What this path refuses to hand on.
    #[serde(default)]
    pub key: Key,
    /// The beam splitter in front of the lens: how much of each monitor this
    /// camera sees. `[1.0]`-style one-hots are a camera aimed straight at one
    /// monitor; two non-zero entries are a camera looking through
    /// beam-splitter glass at a pair.
    pub look: Vec<f32>,
    /// The same splitter, over the external inputs. Counted against the
    /// inputs and not concatenated onto `look`, so the two index spaces stay
    /// independent: adding a monitor to a graph cannot renumber what a camera
    /// is aimed at, because a list that no longer matches its own kind is a
    /// refusal rather than a shift.
    ///
    /// Defaulted, since most graphs have no inputs at all; a graph that has
    /// them and leaves this short is refused by `config::validate`.
    #[serde(default)]
    pub look_inputs: Vec<f32>,
}

fn unity_gain() -> [f32; 3] {
    [1.0; 3]
}

/// One camera on one monitor with every stage doing nothing to the light —
/// the graph [`Knob::identity`] reads each knob's neutral value out of.
///
/// A whole `Params` rather than the handful of constants it is made of, so a
/// knob's identity is read by the same [`Params::knob`] the surface and the
/// keys read its value by: a knob that later moves to another field cannot
/// be neutral here and live there.
fn identity_graph() -> Params {
    Params {
        cameras: vec![Camera {
            framing: Framing::identity(),
            gain: unity_gain(),
            character: Character::CLEAN,
            key: Key::OFF,
            look: vec![1.0],
            look_inputs: Vec::new(),
        }],
        monitors: vec![Monitor::default()],
        inputs: Vec::new(),
        // A crosspoint's identity is unity, like the loop gain's: the
        // switcher handing the camera on whole. Zero is the switcher turned
        // *off*, which is a choice about the graph rather than a stage doing
        // nothing to the light.
        routing: vec![vec![1.0]],
    }
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

impl Focus {
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
    /// The keyer: the luma it demands, the edge both keys blend over, and
    /// the colour the chroma key cuts with how much of it to tolerate.
    KeyThreshold,
    KeySoftness,
    KeyHue,
    KeyTolerance,
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

impl Limit {
    /// The two values a knob runs between. A phase runs -PI to PI, which is
    /// one full turn and the only place that says so — [`wrap_pi`] brings a
    /// value back into it and the control surface spans a fader across it,
    /// and a second spelling of half a turn is a number the two could differ
    /// on.
    pub const fn ends(self) -> (f32, f32) {
        match self {
            Limit::Clamp(low, high) => (low, high),
            Limit::Wrap => (-std::f32::consts::PI, std::f32::consts::PI),
        }
    }
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
    pub const ALL: [Knob; 24] = [
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
        Knob::KeyThreshold,
        Knob::KeySoftness,
        Knob::KeyHue,
        Knob::KeyTolerance,
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
            Knob::KeyThreshold => "key threshold",
            Knob::KeySoftness => "key softness",
            Knob::KeyHue => "key hue",
            Knob::KeyTolerance => "key tolerance",
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

    /// Whether the knob is a value of the graph or a grip on other knobs.
    /// The rigid gain is the only one of the latter: it reads as the mean of
    /// the three channel knobs and turns all three, so it is a reading rather
    /// than a field — which is what [`Params::knob_mut`]'s `unreachable!`
    /// says too. Anything walking the graph's *values* wants the fields, and
    /// would otherwise name a knob no config can write when a channel is at
    /// fault.
    pub const fn owns_a_field(self) -> bool {
        !matches!(self, Knob::Gain)
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
            | Knob::Noise
            | Knob::KeyThreshold
            | Knob::KeySoftness
            | Knob::KeyHue
            | Knob::KeyTolerance => Side::Camera,
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
            Knob::Hue | Knob::KeyHue => 0.02,
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
            | Knob::Route
            // Both are swept end to end hunting the backdrop's level, and
            // the soft edge is what forgives a coarse landing.
            | Knob::KeyThreshold
            | Knob::KeyTolerance => 0.005,
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
            | Knob::Noise
            | Knob::KeySoftness => 0.002,
        }
    }

    /// Where this knob stands with its stage doing nothing to the light:
    /// zoom 1, no turn, no pan, unity gain, a clean path, the keys off, a
    /// neutral front panel, no seed. This is what one knob is put back to
    /// without the rest of the panel going with it.
    ///
    /// Read out of [`identity_graph`] rather than written here as a second
    /// table of numbers: every value it holds is already a named constant a
    /// config file defaults to, and a table beside them is a copy to keep in
    /// step. The graph costs two small allocations, once per button press.
    pub fn identity(self) -> f32 {
        identity_graph().knob(self, Focus::default())
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
            // Luma runs 0 to 1 in a frame; the loop's reserve above white is
            // not something a keyer has any business waiting for.
            Knob::KeyThreshold => Limit::Clamp(0.0, 1.0),
            // A quarter of the scale is already a fog, not an edge.
            Knob::KeySoftness => Limit::Clamp(0.0, 0.25),
            // The top rail is the off state — see [`Key::TOLERANT`].
            Knob::KeyTolerance => Limit::Clamp(0.0, Key::TOLERANT),
            // A phase: it comes back round instead of running away.
            Knob::Hue | Knob::KeyHue => Limit::Wrap,
            Knob::Saturation | Knob::Contrast => Limit::Clamp(0.0, 4.0),
            // Potent inside a loop, so the rails are close: a tenth of a unit
            // added every pass floods the monitor to white in under a second.
            Knob::Brightness => Limit::Clamp(-0.5, 0.5),
            // Zero would flatten every level to 1.0, and below it
            // `pow(0, gamma)` is an infinity — the monitor's corners are
            // exactly 0 whenever the seed does not reach them, and one pass
            // later the chroma matrix turns that infinity into a NaN, which
            // never leaves a loop that feeds itself. The floor sits well
            // above either, because a phosphor curve worth playing lives
            // nowhere near the rails.
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
    /// layer count of the source bank, and the layer an input sits on is its
    /// index past the monitors — which is why `Feedback::new` takes the graph
    /// rather than a count it could be handed the wrong one of.
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
            Knob::KeyThreshold => cam.key.threshold,
            Knob::KeySoftness => cam.key.softness,
            Knob::KeyHue => cam.key.hue,
            Knob::KeyTolerance => cam.key.tolerance,
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
            Knob::KeyThreshold => &mut self.cameras[focus.camera].key.threshold,
            Knob::KeySoftness => &mut self.cameras[focus.camera].key.softness,
            Knob::KeyHue => &mut self.cameras[focus.camera].key.hue,
            Knob::KeyTolerance => &mut self.cameras[focus.camera].key.tolerance,
            Knob::Seed => &mut self.monitors[focus.monitor].seed_brightness,
            Knob::Hue => &mut self.monitors[focus.monitor].colour.hue,
            Knob::Saturation => &mut self.monitors[focus.monitor].colour.saturation,
            Knob::Brightness => &mut self.monitors[focus.monitor].colour.brightness,
            Knob::Contrast => &mut self.monitors[focus.monitor].colour.contrast,
            Knob::Gamma => &mut self.monitors[focus.monitor].colour.gamma,
            Knob::Headroom => &mut self.monitors[focus.monitor].headroom,
            Knob::Route => &mut self.routing[focus.monitor][focus.camera],
            // `owns_a_field` is the same fact, said where a walk can read it.
            Knob::Gain => unreachable!("nudge() splits Gain into its channels"),
        }
    }

    /// The focused camera and monitor, and every knob's value: the only
    /// readout the instrument has.
    pub fn describe(&self, focus: Focus) -> String {
        let cam = &self.cameras[focus.camera];
        let mon = &self.monitors[focus.monitor];
        format!(
            // A line per side of the graph rather than one for the lot: at
            // two dozen knobs a single line wraps in a terminal, and
            // consecutive presses stop lining up — which was the only thing
            // a single line was buying.
            "cam {}/{}: zoom {:.3}  rot {:+.3}  pan {:+.3},{:+.3}  gain {:.3},{:.3},{:.3}  \
             bloom {:.3}  radius {:.3}  bleed {:.3}  noise {:.3}  \
             key {:.3}/{:.3}  key hue {:+.3}  key tol {:.3}\n\
             mon {}/{}: seed {:.3}  hue {:+.3}  sat {:.3}  bright {:+.3}  contrast {:.3}  \
             gamma {:.3}  headroom {:.3}\n\
             route {:.3}: how much of cam {} mon {} shows",
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
            cam.key.threshold,
            cam.key.softness,
            cam.key.hue,
            cam.key.tolerance,
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
        )
    }
}

fn rigid_gain_step(gain: &[f32; 3], delta: f32) -> f32 {
    let (low, high) = Knob::Gain.limit().ends();
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
        assert_eq!(cam.key.threshold, 1.0);
        assert_eq!(cam.key.softness, 0.25);
        assert_eq!(cam.key.tolerance, Key::TOLERANT);
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
        assert_eq!(cam.key.threshold, 0.0);
        assert_eq!(cam.key.softness, 0.0);
        assert_eq!(cam.key.tolerance, 0.0);
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
    fn the_key_weights_spare_grey_and_the_tolerant_rail_clears_every_frame() {
        use core::f32::consts::TAU;
        for i in 0..=1000 {
            let k = key_weights(-PI + TAU * i as f32 / 1000.0);
            // Grey has no chroma, so no hue's key may touch it: both
            // subcarrier rows of DECODE sum to zero and so does any blend.
            let grey: f32 = k.iter().sum();
            assert!(grey.abs() < 1e-5, "grey projects {grey}");
            // The widest projection an RGB frame in 0..=1 can carry is the
            // sum of the positive weights, at the corner that lights exactly
            // those channels. TOLERANT clears it at every hue — that is the
            // claim its value makes, so it is held here.
            let most: f32 = k.iter().map(|w| w.max(0.0)).sum();
            assert!(most < Key::TOLERANT, "a frame can project {most}");
        }
    }

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
            let written = toml::Value::try_from(knob).unwrap();
            assert_eq!(written.as_str(), Some(knob.name()), "{knob:?} on disk");
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
        // `knob` matches the two dozen fields over again, next to the match
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
            // Away from whichever rail the default sits on — the key
            // tolerance's off state is its top rail, so it only has room
            // down.
            let step = match knob.limit() {
                Limit::Clamp(_, high) if params.knob(knob, focus) >= high => -knob.increment(),
                _ => knob.increment(),
            };
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
        // A phase comes back round instead — so the value asked for has to be
        // past the turn, or nothing wraps and `set` could be writing the
        // field straight rather than going through `nudge`.
        params.set(Knob::Hue, 4.0, focus);
        let wrapped = 4.0 - std::f32::consts::TAU;
        assert!(
            (params.knob(Knob::Hue, focus) - wrapped).abs() < 1e-5,
            "{} not {wrapped}",
            params.knob(Knob::Hue, focus)
        );
        // And the rigid gain's three-channel step is delegated here too, not
        // only in the test that names it.
        params.set(Knob::Gain, 0.4, focus);
        let gain = params.cameras[0].gain;
        assert!((gain.iter().sum::<f32>() / 3.0 - 0.4).abs() < 1e-6);
    }

    #[test]
    fn setting_the_rigid_gain_slides_the_channels_and_keeps_their_offsets() {
        let mut params = p();
        let focus = Focus::default();
        // A triple equal to none of its own channels: the shipped gains are
        // symmetric, so their mean *is* the middle channel and a reader
        // returning `gain[1]` cannot be told from one returning the mean.
        params.cameras[0].gain = [0.2, 0.9, 0.9];
        let before = params.cameras[0].gain;
        let offsets = [before[1] - before[0], before[2] - before[0]];
        assert!((params.knob(Knob::Gain, focus) - 2.0 / 3.0).abs() < 1e-6);
        params.set(Knob::Gain, 0.5, focus);
        let after = params.cameras[0].gain;
        // Against the array, not against `set`'s own idea of where it put it.
        assert!((after.iter().sum::<f32>() / 3.0 - 0.5).abs() < 1e-6);
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
    fn a_knob_past_its_rail_is_refused_rather_than_snapped_later() {
        // The bug one range per knob closes: a file loading at a value the
        // first press would clamp away, leaving the instrument showing a
        // state neither a key nor a fader can put it back into. Over
        // `Knob::ALL` rather than a list of the knobs that had the bug, so a
        // knob added later is covered the day it joins it.
        //
        // The rigid gain has no field to poison, and `validate` skips it for
        // the same reason — `owns_a_field` is where that is said once.
        for knob in Knob::ALL.into_iter().filter(|knob| knob.owns_a_field()) {
            // Camera 1 and monitor 2, and the whole line compared rather than
            // hunted for the knob's name: this walk is the only thing left
            // standing behind that message, and against a node named 1 and a
            // node named 2 a knob reported against the wrong half of the
            // focus is caught by the number as well as by the word.
            let focus = Focus {
                camera: 1,
                monitor: 2,
            };
            let name = knob.name();
            let node = match knob.side() {
                Side::Camera => format!("camera {}'s {name}", focus.camera),
                Side::Monitor => format!("monitor {}'s {name}", focus.monitor),
                Side::Edge => format!(
                    "camera {}'s {name} to monitor {}",
                    focus.camera, focus.monitor
                ),
            };
            let (low, high) = knob.limit().ends();
            for past in [low - 1.0, high + 1.0] {
                let mut params = crate::config::insanity();
                *params.knob_mut(knob, focus) = past;
                let why = crate::config::validate(&params)
                    .expect_err(&format!("{name} loaded at {past}"));
                assert_eq!(why, format!("{node} is {past}; it runs {low} to {high}"));
            }
        }
    }
}
