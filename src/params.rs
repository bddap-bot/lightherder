//! The knobs on the instrument. No windowing, no GPU — a MIDI surface drives
//! the same values a fader does.

use std::fmt;

use serde::{Deserialize, Deserializer};

use crate::affine::Framing;
use crate::input::Input;

/// The colour controls on one monitor's front panel, in the order an analog
/// signal meets them: chroma decode, video amplifier, phosphor.
///
/// None of these is the loop gain wearing a hat. The gain is a per-channel
/// multiply of the light coming *back*, and is what puts any chroma into a
/// white seed's trail at all; these turn the chroma that is already there.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
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
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
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
/// keys that multiply — a luma key that cuts the dark, and a chroma key that
/// cuts one colour. It sits with the gain, the framing and the character
/// because it is one more thing a signal path does to the light, and the
/// camera is the only signal path this instrument has: what the switcher
/// hands a monitor from outside it hands over whole.
///
/// Every camera watches monitors, so every key here is a gate on the
/// feedback itself — the dark of a trail refused a trip round, or one hue of
/// it — which is its own instrument to play.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
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
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Camera {
    /// How this camera's view is magnified, turned and shifted relative to
    /// what it looks at.
    #[serde(default = "Framing::identity")]
    pub framing: Framing,
    /// Per-channel gain applied to everything this camera sees. On a monitor
    /// seeded by its cameras, an effective per-monitor gain below 1.0 dies
    /// out and above 1.0 blooms; with a blob on the glass the loop settles
    /// instead, brighter the closer to 1.0. The channels differ to colour
    /// the trails.
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
    ///
    /// Monitors, and nothing else. A camera here is a camera on a stand in a
    /// room of monitors, so the only things in front of its lens are glass
    /// and light already going round; light from outside arrives where a
    /// switcher takes it, on [`Params::routing`]. That is what makes every
    /// camera recursive by construction rather than by convention.
    pub look: Vec<f32>,
    /// The frame delay unit on this camera's cable: how many passes old the
    /// frames it hands on are, past the one pass every camera is behind by.
    /// Zero is the cable alone. What it does to the picture is the
    /// original's: a sudden movement comes back as an echoing pulse, a
    /// smooth one as a frozen smear.
    #[serde(default)]
    pub delay: u32,
}

impl Camera {
    /// The most delay a camera may be given: the thirty frames the
    /// original's delay units dial up to. A bound because the delay is
    /// bought in bank: every frame of it is another copy of every monitor.
    pub const MAX_DELAY: u32 = 30;
}

fn unity_gain() -> [f32; 3] {
    [1.0; 3]
}

/// One camera on one monitor with every stage doing nothing to the light —
/// the graph [`Knob::identity`] reads each knob's neutral value out of.
///
/// A whole `Params` rather than the handful of constants it is made of, so a
/// knob's identity is read by the same [`Params::knob`] the surface reads its
/// value by: a knob that later moves to another field cannot be neutral here
/// and live there.
fn identity_graph() -> Params {
    Params {
        cameras: vec![Camera {
            framing: Framing::identity(),
            gain: unity_gain(),
            character: Character::CLEAN,
            key: Key::OFF,
            look: vec![1.0],
            delay: 0,
        }],
        monitors: vec![Monitor::default()],
        // An input, so the send has a crosspoint to read its identity out of
        // rather than agreeing with the absent-crosspoint reading by
        // coincidence. Its kind does not matter — nothing opens this graph.
        inputs: vec![Input::Pattern(crate::input::Pattern::Bars)],
        // Zero, where every other identity here is the value that leaves
        // the light alone. A crosspoint has no such value: it is not a stage
        // the light passes through but a weight in a sum, and its row is the
        // monitor's loop gain. Unity would have been the reading by analogy
        // with the loop gain — and on `crossed`, whose focused crosspoint
        // loads at 0, it puts a second camera on that monitor at full and
        // takes the row to 2.0, straight into the rail. So: the connection
        // not made. The monitor visibly loses that camera and the fader puts
        // it back, which is the error that corrects itself. The send is the
        // same weight and gets the same reading.
        routing: vec![vec![0.0]],
        routing_inputs: vec![vec![0.0]],
    }
}

/// What lights one monitor from outside the loop it is already in.
///
/// A sum type and not a level with an off value, because the two are
/// different rigs rather than two settings of one: a blob on the glass is
/// light *entering* the graph, and a monitor without one holds only what the
/// switcher paints on it. A level can only tell those apart by a magic zero
/// nothing names — which is why `config::validate` refuses a blob of no
/// light rather than letting it be the dark rig spelled a second way. And
/// the dark rig's level is already played elsewhere, on the switcher's
/// crosspoints and the gains behind them, which is why this costs the
/// surface a button and not a fader.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Seed {
    /// A soft white spot on the glass at this brightness, which is above
    /// zero: the classic way to start a loop, since one with gain below 1.0
    /// and nothing entering it decays to black. Where the blob sits and how
    /// wide it is belong to [`crate::feedback`], the only place that draws
    /// it.
    WhiteBlob(f32),
    /// No light of its own: the glass is dark until the switcher paints
    /// something on it, and what it paints — cameras, external inputs, or a
    /// mix — is the crosspoints' business rather than the seed's. Named for
    /// the glass and not for what lands on it, because a monitor lit by a
    /// test pattern is as seedless as one lit by a camera.
    Dark,
}

impl Seed {
    /// A blob at the brightness every preset that has one runs at — the one
    /// copy of that number, and what the toggle brings back. A config that
    /// named its own level does not get it back by pressing the button
    /// twice: a toggle is not an undo.
    pub const BLOB: Seed = Seed::WhiteBlob(0.10);

    /// The brightest a blob may be: display white. A spot brighter than the
    /// monitor can show is one the amplifier's rail bends on the way in,
    /// which is a level nobody chose.
    pub const BRIGHTEST: f32 = 1.0;

    /// The other kind — the whole of what the button does.
    pub const fn toggled(self) -> Seed {
        match self {
            Seed::WhiteBlob(_) => Seed::Dark,
            Seed::Dark => Seed::BLOB,
        }
    }

    /// What the shader adds at the spot. Dark glass adds nothing there: every
    /// photon on it arrived through the taps, like every other photon going
    /// round.
    pub const fn brightness(self) -> f32 {
        match self {
            Seed::WhiteBlob(brightness) => brightness,
            Seed::Dark => 0.0,
        }
    }

    /// Whether this monitor puts light of its own on the glass — the one bit
    /// the surface's lamp reads. Off the level rather than off the variant,
    /// so a lamp cannot claim a blob the shader is not drawing.
    pub fn lit(self) -> bool {
        self.brightness() > 0.0
    }
}

impl fmt::Display for Seed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Seed::WhiteBlob(brightness) => write!(f, "white blob {brightness:.3}"),
            Seed::Dark => write!(f, "dark"),
        }
    }
}

/// One monitor in the graph: its front panel and what lights it.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Monitor {
    pub colour: Colour,
    /// What lights this monitor's loop from outside it.
    pub seed: Seed,
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
            seed: Seed::Dark,
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
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Params {
    pub cameras: Vec<Camera>,
    pub monitors: Vec<Monitor>,
    /// The light the switcher has that the graph did not make: test
    /// patterns, video files, capture devices. Plugged into the switcher and
    /// nothing else — nothing draws to one and no camera may watch one — so
    /// it is light entering the graph, like the seed spot, rather than light
    /// going round it. `routing_inputs` is where each one lands.
    #[serde(default)]
    pub inputs: Vec<Input>,
    /// The routing matrix: `routing[m][c]` is how much of camera `c`'s output
    /// monitor `m` displays. A permutation matrix is a plain switcher; rows
    /// with several non-zero entries mix cameras on one monitor.
    pub routing: Vec<Vec<f32>>,
    /// The other half of the same switcher: `routing_inputs[i][m]` is how
    /// much of input `i` monitor `m` shows. This is the whole of how outside
    /// light reaches the graph, and the level it enters at.
    ///
    /// Counted against its own kind rather than added as columns of
    /// `routing`, for the reason a camera's `look` is: a list that no longer
    /// matches its own kind is a refusal, where one shared index space would
    /// let a camera added to a graph quietly take over an input's weight.
    ///
    /// A row per *input* over the monitors, where `routing` is a row per
    /// monitor over the cameras. The count that disappears when a graph has
    /// no inputs is the one that had better be the row count: a graph
    /// without them writes `[]` rather than a rack of empty rows, and there
    /// is no empty-means-nothing case for a loader to get wrong.
    #[serde(default)]
    pub routing_inputs: Vec<Vec<f32>>,
}

impl Default for Params {
    fn default() -> Self {
        crate::config::single()
    }
}

/// One monitor's column of the switcher: what it shows of every camera and
/// of every input, and which monitor, so it can only ever go back where it
/// came from. Both halves together, because a cut takes the whole column
/// and a release owes the whole column back.
pub struct Crosspoints {
    monitor: usize,
    cameras: Vec<f32>,
    inputs: Vec<f32>,
}

/// A kind of node the focus points at, and so one of the surface's three
/// rows of select buttons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Node {
    Camera,
    Monitor,
    Input,
}

impl Node {
    /// Every `for node in ALL` walk is silently vacuous for a kind missing
    /// from this list, including the ones that exist to catch omissions.
    pub const ALL: [Node; 3] = [Node::Camera, Node::Monitor, Node::Input];

    pub const fn name(self) -> &'static str {
        match self {
            Node::Camera => "camera",
            Node::Monitor => "monitor",
            Node::Input => "input",
        }
    }

    /// The kind in the words the on-screen overlay's captions have room for.
    pub const fn short(self) -> &'static str {
        match self {
            Node::Camera => "cam",
            Node::Monitor => "mon",
            Node::Input => "in",
        }
    }
}

/// Which camera, which monitor and which input the knobs act on. Named
/// fields on purpose: bare `usize`s in a row would let a swapped pair compile
/// and silently edit the wrong node.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Focus {
    pub camera: usize,
    pub monitor: usize,
    pub input: usize,
}

impl Focus {
    pub fn at(self, node: Node) -> usize {
        match node {
            Node::Camera => self.camera,
            Node::Monitor => self.monitor,
            Node::Input => self.input,
        }
    }

    pub fn with(mut self, node: Node, index: usize) -> Focus {
        match node {
            Node::Camera => self.camera = index,
            Node::Monitor => self.monitor = index,
            Node::Input => self.input = index,
        }
        self
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
    Hue,
    Saturation,
    Brightness,
    Contrast,
    Gamma,
    Headroom,
    /// The switcher crosspoint the focus's camera and monitor name between
    /// them: how much of the focused camera the focused monitor shows.
    Route,
    /// The same, on the switcher's other kind of column: how much of the
    /// first input the focused monitor shows. This is the level outside
    /// light enters the graph at, and the only knob that is not there on
    /// every graph — a rig with no inputs has no send to turn.
    Send,
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

/// Which node a knob's value belongs to, and so which of a [`Focus`]'s
/// indices it reads. Most knobs live on one node and read one; the
/// switcher's two crosspoints are edges, and read the pair of indices their
/// own kind of edge joins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Camera,
    Monitor,
    /// A crosspoint between a camera and a monitor.
    Edge,
    /// A crosspoint between an input and a monitor. Its own side and not
    /// `Edge`'s second reading, because the pair it names is a different
    /// pair: a walk over the focuses would otherwise index cameras for it.
    InputEdge,
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
        Knob::Hue,
        Knob::Saturation,
        Knob::Brightness,
        Knob::Contrast,
        Knob::Gamma,
        Knob::Headroom,
        Knob::Route,
        Knob::Send,
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
            Knob::Hue => "hue",
            Knob::Saturation => "saturation",
            Knob::Brightness => "brightness",
            Knob::Contrast => "contrast",
            Knob::Gamma => "gamma",
            Knob::Headroom => "headroom",
            Knob::Route => "route",
            Knob::Send => "send",
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

    /// Whether turning one of these knobs moves a value the other reads.
    ///
    /// True of a knob and itself, and of the rigid gain and each of its three
    /// channels, which write the very same three floats — so a fader on one
    /// of them is holding a knob the other has just moved, and a reset of
    /// either has to let go of both. Not true of two channels: red and green
    /// are separate floats and a fader on one still agrees with its knob
    /// after the other moves.
    ///
    /// The one place this crate says which knobs overlap. [`Params::reset`]
    /// and [`crate::midi::Midi::release_knob`] both ask it, so the two cannot
    /// disagree about what a reset touched.
    pub const fn shares_a_field_with(self, other: Knob) -> bool {
        use Knob::{Gain, GainB, GainG, GainR};
        self as u8 == other as u8
            || matches!(
                (self, other),
                (Gain, GainR | GainG | GainB) | (GainR | GainG | GainB, Gain)
            )
    }

    /// Which of a [`Focus`]'s indices the knob reads.
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
            Knob::Hue
            | Knob::Saturation
            | Knob::Brightness
            | Knob::Contrast
            | Knob::Gamma
            | Knob::Headroom => Side::Monitor,
            Knob::Route => Side::Edge,
            Knob::Send => Side::InputEdge,
        }
    }

    /// Whether the graph has the node this knob acts on. Only a send can be
    /// missing — [`crate::config::validate`] refuses a graph with no camera
    /// or no monitor. The factory map leaves out a knob that is not on and
    /// [`crate::midi::Map`] refuses a hand-written binding of one, off this
    /// one answer.
    pub fn is_on(self, params: &Params) -> bool {
        self.side() != Side::InputEdge || params.count(Node::Input) > 0
    }

    /// Where this knob stands with its stage doing nothing to the light:
    /// zoom 1, no turn, no pan, unity gain, a clean path, the keys off and a
    /// neutral front panel. This is what one knob is put back to
    /// without the rest of the panel going with it.
    ///
    /// Read out of [`identity_graph`] rather than written here as a second
    /// table of numbers: every value it holds is already a named constant a
    /// config file defaults to, and a table beside them is a copy to keep in
    /// step. The graph costs five small allocations, once per button press.
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
            // A crosspoint is a fraction of what it is switching. Above 1.0
            // it would be an amplifier, which is what the loop gain already
            // is — and on a send it would be an input brighter than itself.
            Knob::Route | Knob::Send => Limit::Clamp(0.0, 1.0),
        }
    }
}

/// By name, not by variant: a config file naming a knob reads the way the
/// help prints it, and deriving would give it a second spelling to drift
/// from.
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
    /// The one walk over the three lists by kind, so a surface built from
    /// the graph and a check run against it cannot count it two ways.
    pub fn count(&self, node: Node) -> usize {
        match node {
            Node::Camera => self.cameras.len(),
            Node::Monitor => self.monitors.len(),
            Node::Input => self.inputs.len(),
        }
    }

    /// How many frames of every monitor the bank keeps as a ring: the one a
    /// pass is drawing, the one every camera reads, and one more per frame
    /// of the longest delay any camera asks for.
    pub fn history(&self) -> usize {
        2 + self
            .cameras
            .iter()
            .map(|camera| camera.delay as usize)
            .max()
            .unwrap_or(0)
    }

    /// The switcher's cut: the focused monitor shows the focused input whole
    /// — or, on a graph with no inputs, the focused camera — and nothing
    /// else. Returns the column as it stood, for [`Params::restore`].
    pub fn cut(&mut self, focus: Focus) -> Crosspoints {
        let monitor = focus.monitor;
        let prior = Crosspoints {
            monitor,
            cameras: self.routing[monitor].clone(),
            inputs: self.routing_inputs.iter().map(|row| row[monitor]).collect(),
        };
        let mut whole = Crosspoints {
            monitor,
            cameras: vec![0.0; prior.cameras.len()],
            inputs: vec![0.0; prior.inputs.len()],
        };
        match self.inputs.is_empty() {
            false => whole.inputs[focus.input] = 1.0,
            true => whole.cameras[focus.camera] = 1.0,
        }
        self.restore(&whole);
        prior
    }

    /// Put a column back where [`Params::cut`] took it from.
    pub fn restore(&mut self, points: &Crosspoints) {
        self.routing[points.monitor].clone_from(&points.cameras);
        for (row, level) in self.routing_inputs.iter_mut().zip(&points.inputs) {
            row[points.monitor] = *level;
        }
    }

    /// Put `knob` at `value` outright, which is what a fader does: it sends
    /// where it is standing rather than which way it moved.
    ///
    /// Through a delta rather than by writing the field, so the rails, the
    /// wrap and the rigid three-channel step live in one place.
    pub fn set(&mut self, knob: Knob, value: f32, focus: Focus) {
        self.nudge(knob, value - self.knob(knob, focus), focus)
    }

    /// Put `knob` back where its stage does nothing to the light.
    ///
    /// Every *field* the knob shares, rather than the knob itself, and that
    /// is the whole reason this is not `set(knob, knob.identity())`. The
    /// rigid gain is a reading of three floats and setting it slides all
    /// three by one step, which [`rigid_gain_step`] clamps to the tightest
    /// channel's remaining travel — so on a panel with red already on its
    /// 1.2 rail a reset of the loop gain moves nothing at all, and says it
    /// did. And a mean of 1.0 with the channel offsets left on is still a
    /// stage that tints the light, which is not what identity means.
    ///
    /// Each field goes through [`Params::set`], so the rails, the wrap and
    /// the reachability a fader has are unchanged.
    pub fn reset(&mut self, knob: Knob, focus: Focus) {
        for field in Knob::ALL {
            if field.owns_a_field() && knob.shares_a_field_with(field) {
                self.set(field, field.identity(), focus);
            }
        }
    }

    /// Where `knob` is standing. The rigid gain reads as the mean of its
    /// three channels, which is the number its step slides: setting it to
    /// that mean is what leaves the colour offsets alone.
    ///
    /// Every index is one the caller has already landed inside this graph,
    /// the send's included — see [`Knob::is_on`].
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
            Knob::Hue => mon.colour.hue,
            Knob::Saturation => mon.colour.saturation,
            Knob::Brightness => mon.colour.brightness,
            Knob::Contrast => mon.colour.contrast,
            Knob::Gamma => mon.colour.gamma,
            Knob::Headroom => mon.headroom,
            Knob::Route => self.routing[focus.monitor][focus.camera],
            Knob::Send => self.routing_inputs[focus.input][focus.monitor],
        }
    }

    fn nudge(&mut self, knob: Knob, delta: f32, focus: Focus) {
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

    /// Every index is one the caller has already landed inside this graph,
    /// the send's included — see [`Knob::is_on`].
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
            Knob::Hue => &mut self.monitors[focus.monitor].colour.hue,
            Knob::Saturation => &mut self.monitors[focus.monitor].colour.saturation,
            Knob::Brightness => &mut self.monitors[focus.monitor].colour.brightness,
            Knob::Contrast => &mut self.monitors[focus.monitor].colour.contrast,
            Knob::Gamma => &mut self.monitors[focus.monitor].colour.gamma,
            Knob::Headroom => &mut self.monitors[focus.monitor].headroom,
            Knob::Route => &mut self.routing[focus.monitor][focus.camera],
            Knob::Send => &mut self.routing_inputs[focus.input][focus.monitor],
            // `owns_a_field` is the same fact, said where a walk can read it.
            Knob::Gain => unreachable!("nudge() splits Gain into its channels"),
        }
    }

    /// The focused nodes and every knob's value: the only readout the
    /// instrument has.
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
             mon {}/{}: hue {:+.3}  sat {:.3}  bright {:+.3}  contrast {:.3}  \
             gamma {:.3}  headroom {:.3}  seed {}\n\
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
            cam.key.threshold,
            cam.key.softness,
            cam.key.hue,
            cam.key.tolerance,
            focus.monitor + 1,
            self.monitors.len(),
            mon.colour.hue,
            mon.colour.saturation,
            mon.colour.brightness,
            mon.colour.contrast,
            mon.colour.gamma,
            mon.headroom,
            mon.seed,
            self.routing[focus.monitor][focus.camera],
            focus.camera + 1,
            focus.monitor + 1,
            match Knob::Send.is_on(self) {
                false => String::new(),
                true => format!(
                    "\n{} {:.3}: how much of input {}/{} mon {} shows",
                    Knob::Send.name(),
                    self.knob(Knob::Send, focus),
                    focus.input + 1,
                    self.inputs.len(),
                    focus.monitor + 1,
                ),
            },
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

    /// The single-loop preset with an input sent onto its monitor, which is
    /// where one of every knob lives: the send is the one knob a graph can
    /// be without, so a walk over `Knob::ALL` on a rig without one is a walk
    /// with a hole in it.
    fn p() -> Params {
        let params = Params {
            inputs: vec![Input::Pattern(crate::input::Pattern::Bars)],
            routing_inputs: vec![vec![0.5]],
            ..Params::default()
        };
        crate::config::validate(&params).unwrap();
        params
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
    fn the_focus_moves_the_leg_it_names_and_no_other() {
        // Each kind reads and writes its own index. A leg that quietly did
        // nothing would leave its whole select row pressing buttons that
        // change nothing, which is what the rows exist not to be.
        for node in Node::ALL {
            let moved = Focus::default().with(node, 3);
            assert_eq!(moved.at(node), 3, "{node:?} did not move");
            for other in Node::ALL.into_iter().filter(|o| *o != node) {
                assert_eq!(moved.at(other), 0, "{node:?} moved {other:?}");
            }
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
                input: 0,
            },
        );
        params.nudge(
            Knob::Hue,
            0.02,
            Focus {
                camera: 1,
                monitor: 0,
                input: 0,
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
    fn a_seed_is_one_of_two_rigs_and_the_button_swaps_them() {
        assert_eq!(Seed::Dark.toggled(), Seed::BLOB);
        assert_eq!(Seed::BLOB.toggled(), Seed::Dark);
        // A level a config named is not what comes back. There is nowhere to
        // remember it that is not a third state, and a state the instrument
        // holds without showing is the thing this type exists to stop.
        assert_eq!(Seed::WhiteBlob(0.42).toggled(), Seed::Dark);
        assert_eq!(Seed::WhiteBlob(0.42).toggled().toggled(), Seed::BLOB);
        // Only a blob puts light on the glass, and it is the light it says —
        // which is what the surface's lamp reads, rather than the variant.
        assert_eq!(Seed::WhiteBlob(0.42).brightness(), 0.42);
        assert_eq!(Seed::Dark.brightness(), 0.0);
        assert!(Seed::WhiteBlob(0.42).lit() && !Seed::Dark.lit());
        assert!(!Seed::WhiteBlob(0.0).lit(), "a blob of nothing is not lit");
    }

    #[test]
    fn the_readout_names_the_seed_s_rig_and_not_a_level() {
        // The log line is the instrument's whole readout, and a level alone
        // leaves a performer working out from "0.000" which of the two rigs
        // the monitor is on.
        let mut params = p();
        assert!(
            params
                .describe(Focus::default())
                .contains("seed white blob"),
            "{}",
            params.describe(Focus::default())
        );
        params.monitors[0].seed = Seed::Dark;
        assert!(
            params.describe(Focus::default()).contains("seed dark"),
            "{}",
            params.describe(Focus::default())
        );
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
        let at = params.describe(Focus {
            camera: 1,
            monitor: 0,
            input: 0,
        });
        assert!(at.contains("cam 2/2"));
        assert!(at.contains("mon 1/2"));
        assert!(!at.contains("send"));

        let mut three = crate::config::external();
        three.inputs = vec![three.inputs[0].clone(); 3];
        three.routing_inputs = vec![vec![0.014], vec![0.0], vec![0.0]];
        assert!(three.describe(Focus::default()).contains("input 1/3"));
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

    /// Every knob's identity, written out as the number it is rather than as
    /// the constant [`identity_graph`] is built from. Read off the same
    /// constant the code reads and a knob wired to the wrong field would
    /// agree with itself: this table is the independent word.
    const IDENTITIES: [(Knob, f32); 24] = [
        (Knob::Zoom, 1.0),
        (Knob::Rotation, 0.0),
        (Knob::TranslateX, 0.0),
        (Knob::TranslateY, 0.0),
        (Knob::Gain, 1.0),
        (Knob::GainR, 1.0),
        (Knob::GainG, 1.0),
        (Knob::GainB, 1.0),
        (Knob::Bloom, 0.0),
        // Not zero, and neither is the key's softness: both are aim points
        // that nothing reads until the stage they belong to is turned up,
        // and a reset to zero would land that knob hard-edged.
        (Knob::BloomRadius, 0.03),
        (Knob::ChromaBleed, 0.0),
        (Knob::Noise, 0.0),
        (Knob::KeyThreshold, 0.0),
        (Knob::KeySoftness, 0.05),
        (Knob::KeyHue, 0.0),
        // The top of its travel is the off state, so that is where a keyer
        // doing nothing to the light stands.
        (Knob::KeyTolerance, 0.7),
        (Knob::Hue, 0.0),
        (Knob::Saturation, 1.0),
        (Knob::Brightness, 0.0),
        (Knob::Contrast, 1.0),
        (Knob::Gamma, 1.0),
        // The rail at twice display white: an amplifier always has one, and
        // this is the one that touches nothing a monitor can show.
        (Knob::Headroom, 2.0),
        // Unity, like the loop gain — the switcher handing the camera on
        // whole. Zero is the switcher turned off, which blanks the monitor's
        // feed: a choice about the graph, not a stage doing nothing. The
        // send is the same weight and reads the same way.
        (Knob::Route, 0.0),
        (Knob::Send, 0.0),
    ];

    #[test]
    fn a_knobs_identity_is_its_stage_doing_nothing() {
        assert_eq!(
            IDENTITIES.len(),
            Knob::ALL.len(),
            "a knob added since has no identity anyone chose"
        );
        for (knob, want) in IDENTITIES {
            assert_eq!(knob.identity(), want, "{}", knob.name());
            assert!(
                Knob::ALL.contains(&knob),
                "{} is not a knob any more",
                knob.name()
            );
        }
    }

    #[test]
    fn every_identity_is_somewhere_its_own_knob_can_stand() {
        // An identity outside the travel is one the reset can never land on,
        // and `nudge` would quietly clamp it to a rail instead of saying so.
        for knob in Knob::ALL {
            let (low, high) = knob.limit().ends();
            let at = knob.identity();
            assert!(at >= low && at <= high, "{} is {at}", knob.name());
        }
    }

    #[test]
    fn resetting_a_knob_lands_it_on_its_identity_and_leaves_the_rest() {
        // Through `Params::set`, which is what the reset actually calls: a
        // phase wraps, the rigid gain splits into its three channels, and
        // every one of them has to arrive.
        let focus = Focus::default();
        for knob in Knob::ALL {
            let mut params = p();
            let before = params.clone();
            params.set(knob, knob.identity(), focus);
            assert!(
                (params.knob(knob, focus) - knob.identity()).abs() < 1e-6,
                "{} landed on {}",
                knob.name(),
                params.knob(knob, focus)
            );
            // And nothing else moved. The rigid gain is the one knob that is
            // not a field of its own — it turns the three channels and reads
            // as their mean — so it and they move together in both
            // directions, and that pair is the only exemption.
            let channel = |k: Knob| matches!(k, Knob::GainR | Knob::GainG | Knob::GainB);
            for other in Knob::ALL {
                let rigid = (knob == Knob::Gain && channel(other))
                    || (channel(knob) && other == Knob::Gain);
                if other == knob || rigid {
                    continue;
                }
                assert_eq!(
                    params.knob(other, focus),
                    before.knob(other, focus),
                    "resetting {} moved {}",
                    knob.name(),
                    other.name()
                );
            }
        }
        // The colour offsets the rigid gain slides survive it: the mean is
        // 1.0 and the channels are still as far apart as they were.
        let mut params = p();
        let before = params.cameras[0].gain;
        params.set(Knob::Gain, Knob::Gain.identity(), focus);
        let after = params.cameras[0].gain;
        for channel in 0..3 {
            assert!(
                ((after[channel] - after[0]) - (before[channel] - before[0])).abs() < 1e-6,
                "the offsets moved: {before:?} -> {after:?}"
            );
        }
    }

    #[test]
    fn a_reset_of_the_rigid_gain_lands_every_channel_on_unity() {
        // The rigid gain is a reading of three floats, not a field, and a
        // rigid *step* is clamped to the tightest channel's remaining
        // travel — so from a panel with red already on its rail, sliding the
        // mean to 1.0 moves nothing at all and reports that it did.
        let focus = Focus::default();
        let mut params = p();
        params.cameras[0].gain = [1.2, 0.6, 0.6];
        params.reset(Knob::Gain, focus);
        assert_eq!(params.cameras[0].gain, [1.0, 1.0, 1.0]);
        assert_eq!(params.knob(Knob::Gain, focus), Knob::Gain.identity());

        // And from a panel where a slide *could* have reached the mean, the
        // offsets still go: a mean of 1.0 with red a tenth above green is a
        // gain stage that tints the light, which is not doing nothing to it.
        let mut params = p();
        params.cameras[0].gain = [0.9, 0.8, 0.7];
        params.reset(Knob::Gain, focus);
        assert_eq!(params.cameras[0].gain, [1.0, 1.0, 1.0]);

        // One channel reset leaves the other two where they were, which is
        // the other half of what "shares a field" has to get right.
        let mut params = p();
        params.cameras[0].gain = [0.5, 0.6, 0.7];
        params.reset(Knob::GainR, focus);
        assert_eq!(params.cameras[0].gain, [1.0, 0.6, 0.7]);
    }

    #[test]
    fn only_the_gain_and_its_channels_share_a_field() {
        // The one place the crate says which knobs overlap, so both the
        // reset and the surface's release read the same answer.
        for knob in Knob::ALL {
            assert!(knob.shares_a_field_with(knob), "{}", knob.name());
            for other in Knob::ALL {
                let gain = |k| matches!(k, Knob::GainR | Knob::GainG | Knob::GainB);
                let want = knob == other
                    || (knob == Knob::Gain && gain(other))
                    || (gain(knob) && other == Knob::Gain);
                assert_eq!(
                    knob.shares_a_field_with(other),
                    want,
                    "{} and {}",
                    knob.name(),
                    other.name()
                );
            }
        }
        // Two channels are two floats: turning red leaves green's fader
        // standing exactly where green still is.
        assert!(!Knob::GainR.shares_a_field_with(Knob::GainG));
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
            // Through a literal line of TOML, which is how a `midi.toml`
            // names one: the terminal's spelling has to be the file's.
            let read: std::collections::HashMap<String, Knob> =
                toml::from_str(&format!("knob = {:?}", knob.name())).unwrap();
            assert_eq!(read["knob"], knob, "{knob:?} on disk");
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
            // `crossed` with an input plugged in: the send is a field only
            // where there is one, and a knob with no field is a knob this
            // walk cannot tell a mis-wired reader from.
            let mut params = crate::config::crossed();
            params.inputs = vec![Input::Pattern(crate::input::Pattern::Bars)];
            params.routing_inputs = vec![vec![0.0, 0.5]];
            let focus = Focus {
                camera: 1,
                monitor: 1,
                input: 0,
            };
            let before: Vec<f32> = Knob::ALL
                .iter()
                .map(|other| params.knob(*other, focus))
                .collect();
            // Away from whichever rail the default sits on — the key
            // tolerance's off state is its top rail, so it only has room
            // down. Inside the tightest knob's travel, so every knob can
            // take it.
            const STEP: f32 = 0.002;
            let step = match knob.limit() {
                Limit::Clamp(_, high) if params.knob(knob, focus) >= high => -STEP,
                _ => STEP,
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
            input: 0,
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
            input: 0,
        };
        assert!((params.knob(Knob::Route, other) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn a_knob_past_its_rail_is_refused_rather_than_snapped_later() {
        // The bug one range per knob closes: a file loading at a value the
        // first press would clamp away, leaving the instrument showing a
        // state no control can put it back into. Over
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
                input: 0,
            };
            let name = knob.name();
            let node = match knob.side() {
                Side::Camera => format!("camera {}'s {name}", focus.camera),
                Side::Monitor => format!("monitor {}'s {name}", focus.monitor),
                Side::Edge => format!(
                    "camera {}'s {name} to monitor {}",
                    focus.camera, focus.monitor
                ),
                Side::InputEdge => format!(
                    "input {}'s {name} to monitor {}",
                    focus.input, focus.monitor
                ),
            };
            let (low, high) = knob.limit().ends();
            for past in [low - 1.0, high + 1.0] {
                // `insanity` with an input plugged in, so the send has a
                // field on this walk like every other knob; without one it
                // would be the one knob whose refusal nothing here reads.
                let mut params = crate::config::insanity();
                params.inputs = vec![Input::Pattern(crate::input::Pattern::Bars)];
                params.routing_inputs = vec![vec![0.0; params.monitors.len()]];
                *params.knob_mut(knob, focus) = past;
                let why = crate::config::validate(&params)
                    .expect_err(&format!("{name} loaded at {past}"));
                assert_eq!(why, format!("{node} is {past}; it runs {low} to {high}"));
            }
        }
    }
}
