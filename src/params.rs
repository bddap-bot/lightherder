//! The knobs on the instrument. No windowing, no GPU — a MIDI surface drives
//! the same values a fader does.

use crate::affine::{Axis, Framing};
use crate::input::Input;
use crate::rig::Rig;

/// The colour controls on one monitor's front panel, in the order an analog
/// signal meets them: chroma decode, video amplifier, phosphor.
///
/// None of these is the loop gain wearing a hat. The gain is a per-channel
/// multiply of the light coming *back*, and is what puts any chroma into a
/// white seed's trail at all; these turn the chroma that is already there.
#[derive(Clone, Copy, Debug, PartialEq)]
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
    /// Where the monitor's white sits on the Planckian locus, as a distance
    /// from D65 in mired — reciprocal megakelvin, the unit the locus is close
    /// to even in, where kelvin bunches every warm white into a corner of
    /// the fader. Positive warms, negative cools. A grey takes on that
    /// white's tint and keeps its luma, which is what tells this knob from
    /// the three gains.
    pub temperature: f32,
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

/// D65 in mired: the white a grey has with the temperature knob at rest.
const D65_MIRED: f64 = 1e6 / 6504.0;

/// The chroma a grey of unit luma takes on with the white moved `mired`
/// along the Planckian locus from D65, as the two subcarrier axes.
///
/// Kim et al.'s cubic fit of a black body's CIE 1931 chromaticity, to XYZ
/// at unit Y, to sRGB as a display shows it — the loop's texels are what
/// the glass shows, not linear light — then decoded. The locus does not
/// pass through D65 itself — a daylight white sits a little above it — so
/// this is the shift from the locus's own point at D65's temperature: at
/// zero it is exactly zero, which keeps the neutral matrix exactly the
/// identity.
fn white_shift(mired: f64) -> [f64; 2] {
    fn chroma(kelvin: f64) -> [f64; 2] {
        let t = kelvin;
        let x = if t < 4000.0 {
            -0.2661239e9 / (t * t * t) - 0.2343589e6 / (t * t) + 0.8776956e3 / t + 0.179910
        } else {
            -3.0258469e9 / (t * t * t) + 2.1070379e6 / (t * t) + 0.2226347e3 / t + 0.240390
        };
        let y = if t < 2222.0 {
            -1.1063814 * x * x * x - 1.34811020 * x * x + 2.18555832 * x - 0.20219683
        } else if t < 4000.0 {
            -0.9549476 * x * x * x - 1.37418593 * x * x + 2.09137015 * x - 0.16748867
        } else {
            3.0817580 * x * x * x - 5.87338670 * x * x + 3.75112997 * x - 0.37001483
        };
        let (big_x, big_z) = (x / y, (1.0 - x - y) / y);
        let rgb = [
            3.2406 * big_x - 1.5372 - 0.4986 * big_z,
            -0.9689 * big_x + 1.8758 + 0.0415 * big_z,
            0.0557 * big_x - 0.2040 + 1.0570 * big_z,
        ]
        .map(|linear: f64| {
            let linear = linear.max(0.0);
            if linear <= 0.0031308 {
                12.92 * linear
            } else {
                1.055 * linear.powf(1.0 / 2.4) - 0.055
            }
        });
        let weigh = |row: [f64; 3]| row.iter().zip(rgb).map(|(w, c)| w * c).sum::<f64>();
        let luma = weigh(DECODE[0]);
        [weigh(DECODE[1]) / luma, weigh(DECODE[2]) / luma]
    }
    let kelvin = |mired: f64| 1e6 / (D65_MIRED + mired);
    let [i, q] = chroma(kelvin(mired));
    let [i0, q0] = chroma(kelvin(0.0));
    [i - i0, q - q0]
}

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
        temperature: 0.0,
    };

    /// The 3x3 the shader multiplies RGB by: decode, turn the chroma by hue
    /// and scale it by saturation, add the white point's chroma in
    /// proportion to the luma, encode back. Indexed `m[row][col]`.
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
        let [warm_i, warm_q] = white_shift(self.temperature as f64);
        std::array::from_fn(|row| {
            // Turning and scaling the subcarrier is one complex multiply, so
            // it folds into the pair of chroma weights this row encodes with.
            let (i, q) = (ENCODE[row][1], ENCODE[row][2]);
            // The white point is the phosphor's, past the decode, so the hue
            // does not turn it: it rides the luma into each channel unturned.
            let white = 1.0 + i * warm_i + q * warm_q;
            let (i, q) = (i * turn + q * lift, q * turn - i * lift);
            std::array::from_fn(|col| {
                (DECODE[0][col] * white + i * DECODE[1][col] + q * DECODE[2][col]) as f32
            })
        })
    }
}

/// The keyer on the switcher: what it refuses of the light coming in. A luma
/// key, which is what lifts a lit subject off an unlit room — the rig keys
/// on the switcher and nowhere else, so this is the whole of it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Key {
    /// The luma the key passes in full. Cutting is complete one `softness`
    /// below it, so at 0 the key passes everything exactly — which is what
    /// lets 0 be the off state without a switch beside the knob.
    pub threshold: f32,
    /// Width of the key's soft edge, in luma units. Zero is a hard edge; the
    /// shader keeps its smoothstep legal on its own.
    pub softness: f32,
}

impl Key {
    /// The key off: the switcher hands on everything, exactly. A threshold of
    /// zero passes every luma there is, which is what lets zero be the off
    /// state without a switch beside it.
    pub const OFF: Key = Key {
        threshold: 0.0,
        softness: 0.05,
    };
}

/// One camera in the graph: what it sees, how it frames it, and how much of
/// the light it hands on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    /// Per-channel gain applied to everything this camera sees: the loss down
    /// the cable and through the lens, which is what makes a loop settle
    /// rather than run. The channels differ to colour the trails. Nothing on
    /// the rig turns one, so nothing here does either.
    pub gain: [f32; 3],
    /// The beam splitter in front of the lens: how much of each monitor this
    /// camera sees. `[1.0]`-style one-hots are a camera aimed straight at one
    /// monitor; two non-zero entries are a camera looking through
    /// beam-splitter glass at a pair.
    ///
    /// Monitors, and nothing else. A camera here is a camera on a stand in a
    /// room of monitors, so the only things in front of its lens are glass
    /// and light already going round; light from outside arrives where a
    /// switcher takes it, on [`Params::send`]. That is what makes every
    /// camera recursive by construction rather than by convention.
    pub look: [f32; crate::rig::MONITORS],
    /// The frame delay unit on this camera's cable: how many passes old the
    /// frames it hands on are, past the one pass every camera is behind by,
    /// at most the graph's reach. Zero is the cable alone. What it does to the picture is the
    /// original's: a sudden movement comes back as an echoing pulse, a
    /// smooth one as a frozen smear.
    pub delay: u32,
}

impl Params {
    /// The most reach a graph's delay units may have: the thirty frames the
    /// original's dial up to. A bound because the reach is bought in bank:
    /// every frame of it is another copy of every monitor.
    pub const MAX_DELAY: u32 = 30;
}

/// The frame rate of a router output, as the cadence of passes it takes a
/// fresh frame on. A cadence rather than a rate because the rig's clock is
/// a pass, not a second.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cadence {
    Full,
    Half,
    Film,
}

impl Cadence {
    /// A second of the rig's clock, in passes: what [`Cadence::fps`] is
    /// counted against, and the period every cadence repeats in.
    pub const SECOND: u32 = 60;

    /// Fastest to slowest: the order the knob turns through them.
    pub const ALL: [Cadence; 3] = [Cadence::Full, Cadence::Half, Cadence::Film];

    pub const fn fps(self) -> u32 {
        match self {
            Cadence::Full => Cadence::SECOND,
            Cadence::Half => 30,
            Cadence::Film => 24,
        }
    }

    /// Whether an output at this cadence takes a fresh frame on pass
    /// `frame` rather than holding the one it has: where a count of `fps`
    /// a second crosses a whole number.
    pub fn refreshes(self, frame: u64) -> bool {
        let frame = (frame % Cadence::SECOND as u64) as u32;
        frame == 0
            || frame * self.fps() / Cadence::SECOND != (frame - 1) * self.fps() / Cadence::SECOND
    }
}

/// One monitor in the graph: its front panel, and what the router output
/// feeding it does to what it is fed — the mirror and the frame rate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Monitor {
    pub colour: Colour,
    /// Whether this monitor's router output is mirrored left for right and
    /// top for bottom, in [`Axis`] order.
    pub flip: [bool; 2],
    pub cadence: Cadence,
    /// The unsharp mask on the front panel: how much of the difference
    /// between a texel and the mean of its four neighbours is added back.
    /// Zero is the stage skipped outright, so a rested knob is exactly
    /// inert inside a loop that would compound a residual.
    pub sharpness: f32,
}

impl Default for Monitor {
    fn default() -> Self {
        Monitor {
            colour: Colour::NEUTRAL,
            flip: [false; 2],
            cadence: Cadence::Full,
            sharpness: 0.0,
        }
    }
}

impl Monitor {
    pub fn flip(&mut self, axis: Axis) {
        self.flip[axis as usize] = !self.flip[axis as usize];
    }
}

/// The whole instrument for one frame: the shafts the cameras stand on,
/// every camera, every monitor, and the switchers routing the first onto the
/// second. The live state the knobs mutate, and the only one there is.
#[derive(Clone, Debug, PartialEq)]
pub struct Params {
    /// The shafts the cameras stand on — see [`crate::rig::SHAFT_OF`]. Camera
    /// A and the rotating monitor share the first, so turning it turns both
    /// and there is nothing to keep in step.
    pub shafts: [Framing; crate::rig::SHAFTS],
    pub cameras: [Camera; crate::rig::CAMERAS],
    pub monitors: [Monitor; crate::rig::MONITORS],
    /// The switchers and the router selects: the whole of the routing, and
    /// the one place it lives. What each monitor shows is worked out from
    /// this every time it is asked for, so there is no matrix to keep in
    /// step with the levers that set it.
    pub rig: Rig,
    /// The one light the switcher has that the graph did not make. Plugged
    /// into the switcher and nothing else — nothing draws to it and no
    /// camera may watch it — so it is light entering the graph rather than
    /// light going round it. How much of it each monitor shows is the
    /// switchers' business, [`Params::send`].
    pub input: Plug,
    /// The frame delay units' reach: how many frames a camera's `delay` may
    /// be dialled up to, and so how deep a ring of the monitors the bank
    /// keeps. Bought at load, since a frame of it is another copy of every
    /// monitor, so the knob runs to here and no further. Zero is a rig with
    /// no delay unit, and no delay knob.
    pub delay: u32,
}

/// The light plugged into the switcher: what it is, and what the switcher
/// refuses of it on the way in. How much of it each monitor shows is the
/// switchers' business — see [`Params::send`] — and not written here.
#[derive(Clone, Debug, PartialEq)]
pub struct Plug {
    pub source: Input,
    pub key: Key,
}

impl Default for Params {
    fn default() -> Self {
        crate::config::instrument()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End {
    Camera(usize),
    Monitor(usize),
    Seed,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Flow {
    pub from: End,
    pub to: End,
    pub share: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Node {
    Camera,
    Monitor,
    Switcher,
}

impl Node {
    /// Every `for node in ALL` walk is silently vacuous for a kind missing
    /// from this list, including the ones that exist to catch omissions.
    pub const ALL: [Node; 3] = [Node::Camera, Node::Monitor, Node::Switcher];

    /// The kind in the words the on-screen overlay's captions have room for.
    pub const fn short(self) -> &'static str {
        match self {
            Node::Camera => "cam",
            Node::Monitor => "mon",
            Node::Switcher => "sw",
        }
    }
}

/// Which camera, which monitor and which switcher the knobs act on. Named
/// fields on purpose: bare `usize`s in a row would let a swapped pair compile
/// and silently edit the wrong node.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Focus {
    pub camera: usize,
    pub monitor: usize,
    pub switcher: usize,
}

impl Focus {
    pub fn at(self, node: Node) -> usize {
        match node {
            Node::Camera => self.camera,
            Node::Monitor => self.monitor,
            Node::Switcher => self.switcher,
        }
    }

    pub fn with(mut self, node: Node, index: usize) -> Focus {
        match node {
            Node::Camera => self.camera = index,
            Node::Monitor => self.monitor = index,
            Node::Switcher => self.switcher = index,
        }
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Knob {
    /// The camera's slide along its shaft, which is what a zoom of the image
    /// is at this end. The lens's own zoom is a setting of its own and not
    /// this one — see #48.
    Zoom,
    /// The camera's turn about its shaft.
    Rotation,
    /// The frame delay unit on the camera's cable, in whole frames.
    Delay,
    Hue,
    Saturation,
    Brightness,
    Contrast,
    Temperature,
    Sharpness,
    /// The frame rate of the router output feeding the focused monitor, as
    /// a step along [`Cadence::ALL`]: full rate at rest, slower up the travel.
    FrameRate,
    /// How far the focused switcher stands toward its In2: 0 is In1 whole, 1
    /// is In2 whole. The routing is these four and the four selects, and
    /// nothing else.
    Switcher,
    /// Passes between reversals of the focused switcher, the original's
    /// period mode. Zero is the mode off.
    Period,
}

impl Limit {
    /// The two values a knob runs between. A phase runs -PI to PI, which is
    /// one full turn and the only place that says so — [`wrap_pi`] brings a
    /// value back into it and the control surface spans a fader across it,
    /// and a second spelling of half a turn is a number the two could differ
    /// on.
    pub const fn ends(self) -> (f32, f32) {
        match self {
            Limit::Clamp(low, high) | Limit::Ratio(low, high) => (low, high),
            Limit::Whole(high) => (0.0, high as f32),
            Limit::Wrap => (-std::f32::consts::PI, std::f32::consts::PI),
        }
    }

    /// What a surface divides among its codes.
    pub fn travel(self) -> f32 {
        let (low, high) = self.ends();
        self.stepped(high) - self.stepped(low)
    }

    pub fn fraction(self, value: f32) -> f32 {
        let (low, _) = self.ends();
        match self.travel() {
            0.0 => 0.0,
            travel => ((self.stepped(value) - self.stepped(low)) / travel).clamp(0.0, 1.0),
        }
    }

    /// Nepers on a ratio, so that one step is one factor wherever the knob
    /// stands.
    fn stepped(self, value: f32) -> f32 {
        match self {
            Limit::Ratio(..) => value.ln(),
            Limit::Clamp(..) | Limit::Whole(_) | Limit::Wrap => value,
        }
    }

    fn valued(self, stepped: f32) -> f32 {
        match self {
            Limit::Ratio(..) => stepped.exp(),
            Limit::Clamp(..) | Limit::Whole(_) | Limit::Wrap => stepped,
        }
    }
}

/// What a knob does when it runs out of room.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Limit {
    Clamp(f32, f32),
    /// A step multiplies rather than adds.
    Ratio(f32, f32),
    /// A count of frames or passes, from none up to this many: a knob that
    /// moves a whole one at a time, which a surface turning it by deltas
    /// has to bank up to.
    Whole(u32),
    Wrap,
}

impl Knob {
    /// Every `for knob in ALL` test is silently vacuous for a knob missing
    /// from this list, including the ones that exist to catch omissions.
    pub const ALL: [Knob; 12] = [
        Knob::Zoom,
        Knob::Rotation,
        Knob::Delay,
        Knob::Hue,
        Knob::Saturation,
        Knob::Brightness,
        Knob::Contrast,
        Knob::Temperature,
        Knob::Sharpness,
        Knob::FrameRate,
        Knob::Switcher,
        Knob::Period,
    ];

    /// The one name a knob has: on the overlay, in the log and in an error.
    pub const fn name(self) -> &'static str {
        match self {
            Knob::Zoom => "zoom",
            Knob::Rotation => "rotation",
            Knob::Delay => "delay",
            Knob::Hue => "hue",
            Knob::Saturation => "saturation",
            Knob::Brightness => "brightness",
            Knob::Contrast => "contrast",
            Knob::Temperature => "temperature",
            Knob::Sharpness => "sharpness",
            Knob::FrameRate => "frame rate",
            Knob::Switcher => "switcher",
            Knob::Period => "period",
        }
    }

    pub fn reads(self, value: f32) -> String {
        match self {
            Knob::Zoom | Knob::Saturation | Knob::Contrast | Knob::Sharpness | Knob::Switcher => {
                format!("{value:.3}")
            }
            Knob::Rotation | Knob::Hue | Knob::Brightness => format!("{value:+.3}"),
            Knob::Temperature => format!("{value:+.1}"),
            Knob::Delay | Knob::Period => format!("{}", value as u32),
            Knob::FrameRate => format!("{}", Cadence::ALL[value as usize].fps()),
        }
    }

    pub const fn node(self) -> Node {
        match self {
            Knob::Zoom | Knob::Rotation | Knob::Delay => Node::Camera,
            Knob::Hue
            | Knob::Saturation
            | Knob::Brightness
            | Knob::Contrast
            | Knob::Temperature
            | Knob::Sharpness
            | Knob::FrameRate => Node::Monitor,
            Knob::Switcher | Knob::Period => Node::Switcher,
        }
    }

    pub fn identity(self) -> f32 {
        crate::config::instrument().knob(self, Focus::default())
    }

    pub fn limit(self, params: &Params) -> Limit {
        match self {
            // Whole frames, as far as the ring the graph bought goes.
            Knob::Delay => Limit::Whole(params.delay),
            // Zero would divide by zero in the sampling transform.
            Knob::Zoom => Limit::Ratio(0.25, 4.0),
            // Spinning one way for long enough must not run the number away.
            Knob::Rotation => Limit::Wrap,
            // A phase: it comes back round instead of running away.
            Knob::Hue => Limit::Wrap,
            Knob::Saturation | Knob::Contrast => Limit::Clamp(0.0, 4.0),
            // Potent inside a loop, so the rails are close: a tenth of a unit
            // added every pass floods the monitor to white in under a second.
            Knob::Brightness => Limit::Clamp(-0.5, 0.5),
            // Candlelight to open shade, in mired from D65; both ends well
            // inside the 1667 K to 25 000 K the locus fit is good for.
            Knob::Temperature => Limit::Clamp(-100.0, 340.0),
            // The cross's gain on the finest grain is 1 + 2s, so the top of
            // the travel is fivefold on it every pass; past that the loop
            // shows its grain and nothing else.
            Knob::Sharpness => Limit::Clamp(0.0, 2.0),
            Knob::FrameRate => Limit::Whole(Cadence::ALL.len() as u32 - 1),
            Knob::Period => Limit::Whole(crate::rig::MAX_PERIOD),
            // A crossfade stands between its two inputs and nowhere else.
            Knob::Switcher => Limit::Clamp(0.0, 1.0),
        }
    }
}

impl Params {
    /// How many frames of every monitor the bank keeps as a ring: the one a
    /// pass is drawing, the one every camera reads, and one more per frame
    /// of the graph's reach.
    pub fn history(&self) -> usize {
        2 + self.delay as usize
    }

    /// How camera `c`'s view is magnified and turned, which is where the
    /// shaft it stands on stands.
    pub fn framing(&self, c: usize) -> Framing {
        self.shafts[crate::rig::SHAFT_OF[c]]
    }

    /// How much of camera `c` monitor `m` shows, and how much of the seed:
    /// the matrix, off the switchers and the selects. Not stored — see
    /// [`Params::rig`].
    pub fn route(&self, m: usize, c: usize) -> f32 {
        self.rig.feed(m).cameras[c]
    }

    pub fn send(&self, m: usize) -> f32 {
        self.rig.feed(m).seed
    }

    pub fn flows(&self) -> impl Iterator<Item = Flow> + '_ {
        let flow = |from, to, share| Flow { from, to, share };
        let looks = self.cameras.iter().enumerate().flat_map(move |(c, cam)| {
            (0..cam.look.len()).map(move |m| flow(End::Monitor(m), End::Camera(c), cam.look[m]))
        });
        let feeds = (0..self.monitors.len()).flat_map(move |m| {
            let feed = self.rig.feed(m);
            (0..self.cameras.len())
                .map(move |c| flow(End::Camera(c), End::Monitor(m), feed.cameras[c]))
                .chain(std::iter::once(flow(End::Seed, End::Monitor(m), feed.seed)))
        });
        looks.chain(feeds).filter(|f| f.share > 0.0)
    }

    /// Through [`Params::place`] rather than by writing the field, so the
    /// rails and the wrap live in one place.
    fn set(&mut self, knob: Knob, value: f32, focus: Focus) {
        self.place(knob, value, focus);
    }

    /// Put `knob` back where its stage does nothing to the light. Through
    /// [`Params::set`], so the rails and the wrap are unchanged.
    pub fn reset(&mut self, knob: Knob, focus: Focus) {
        self.set(knob, knob.identity(), focus);
    }

    /// Where `knob` is standing. Every index is one the caller has already
    /// landed inside this graph.
    pub fn knob(&self, knob: Knob, focus: Focus) -> f32 {
        let cam = &self.cameras[focus.camera];
        let mon = &self.monitors[focus.monitor];
        match knob {
            Knob::Zoom => self.framing(focus.camera).zoom,
            Knob::Rotation => self.framing(focus.camera).rotation,
            Knob::Delay => cam.delay as f32,
            Knob::Hue => mon.colour.hue,
            Knob::Saturation => mon.colour.saturation,
            Knob::Brightness => mon.colour.brightness,
            Knob::Contrast => mon.colour.contrast,
            Knob::Temperature => mon.colour.temperature,
            Knob::Sharpness => mon.sharpness,
            Knob::FrameRate => Cadence::ALL
                .iter()
                .position(|c| *c == mon.cadence)
                .expect("every cadence is on the ladder") as f32,
            Knob::Period => self.rig.periods[focus.switcher] as f32,
            Knob::Switcher => self.rig.switchers[focus.switcher],
        }
    }

    /// Turn `knob` by `delta` of its own step. Past a rail the rest of the
    /// step is dropped.
    pub fn nudge(&mut self, knob: Knob, delta: f32, focus: Focus) {
        let limit = knob.limit(self);
        let to = limit.valued(limit.stepped(self.knob(knob, focus)) + delta);
        self.place(knob, to, focus);
    }

    fn place(&mut self, knob: Knob, value: f32, focus: Focus) {
        match knob.limit(self) {
            Limit::Whole(most) => {
                let count = value.round().clamp(0.0, most as f32) as u32;
                match knob {
                    Knob::Delay => self.cameras[focus.camera].delay = count,
                    Knob::Period => self.rig.periods[focus.switcher] = count,
                    Knob::FrameRate => {
                        self.monitors[focus.monitor].cadence = Cadence::ALL[count as usize]
                    }
                    Knob::Zoom
                    | Knob::Rotation
                    | Knob::Hue
                    | Knob::Saturation
                    | Knob::Brightness
                    | Knob::Contrast
                    | Knob::Temperature
                    | Knob::Sharpness
                    | Knob::Switcher => unreachable!("only a count has a whole limit"),
                }
            }
            Limit::Clamp(low, high) | Limit::Ratio(low, high) => {
                *self.knob_mut(knob, focus) = value.clamp(low, high);
            }
            Limit::Wrap => {
                *self.knob_mut(knob, focus) = wrap_pi(value);
            }
        }
    }

    /// Every index is one the caller has already landed inside this graph.
    fn knob_mut(&mut self, knob: Knob, focus: Focus) -> &mut f32 {
        match knob {
            Knob::Zoom => &mut self.shafts[crate::rig::SHAFT_OF[focus.camera]].zoom,
            Knob::Rotation => &mut self.shafts[crate::rig::SHAFT_OF[focus.camera]].rotation,
            Knob::Hue => &mut self.monitors[focus.monitor].colour.hue,
            Knob::Saturation => &mut self.monitors[focus.monitor].colour.saturation,
            Knob::Brightness => &mut self.monitors[focus.monitor].colour.brightness,
            Knob::Contrast => &mut self.monitors[focus.monitor].colour.contrast,
            Knob::Temperature => &mut self.monitors[focus.monitor].colour.temperature,
            Knob::Sharpness => &mut self.monitors[focus.monitor].sharpness,
            Knob::Switcher => &mut self.rig.switchers[focus.switcher],
            Knob::Delay | Knob::Period | Knob::FrameRate => {
                unreachable!("nudge() rounds a count to whole steps")
            }
        }
    }

    /// The focused nodes and every knob's value: the only readout the
    /// instrument has.
    pub fn describe(&self, focus: Focus) -> String {
        let reads = |knob: Knob| knob.reads(self.knob(knob, focus));
        format!(
            "cam {}/{}: zoom {}  rot {}  delay {}/{}\n\
             mon {}/{}: hue {}  sat {}  bright {}  contrast {}  \
             temp {}  sharp {}  flip {:?}  rate {}/{}  {}  shows {:.3} of cam {}\n\
             sw {}/{}: switcher {}  period {}",
            focus.camera + 1,
            self.cameras.len(),
            reads(Knob::Zoom),
            reads(Knob::Rotation),
            reads(Knob::Delay),
            self.delay,
            focus.monitor + 1,
            self.monitors.len(),
            reads(Knob::Hue),
            reads(Knob::Saturation),
            reads(Knob::Brightness),
            reads(Knob::Contrast),
            reads(Knob::Temperature),
            reads(Knob::Sharpness),
            self.monitors[focus.monitor].flip,
            reads(Knob::FrameRate),
            Cadence::SECOND,
            match self.rig.on_program(focus.monitor) {
                true => "program",
                false => "direct",
            },
            self.route(focus.monitor, focus.camera),
            focus.camera + 1,
            focus.switcher + 1,
            self.rig.switchers.len(),
            reads(Knob::Switcher),
            reads(Knob::Period),
        )
    }
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

    /// The instrument with a delay unit that has reach, which is where one
    /// of every knob lives: the delay is the one knob a graph can be
    /// without, so a walk over `Knob::ALL` on a rig without it is a walk
    /// with holes in it.
    fn p() -> Params {
        let params = Params {
            delay: 4,
            ..Params::default()
        };
        crate::config::validate(&params).unwrap();
        params
    }

    fn nudge(p: &mut Params, knob: Knob, delta: f32) {
        p.nudge(knob, delta, Focus::default());
    }

    /// A step every knob can take: the delay moves by whole frames and
    /// nothing else, so a fraction of one is a turn it rounds away.
    fn step_for(knob: Knob, step: f32) -> f32 {
        match knob {
            Knob::Delay | Knob::Period | Knob::FrameRate => step.signum(),
            _ => step,
        }
    }

    #[test]
    fn a_rate_refreshes_its_fps_times_a_second_in_the_film_cadence() {
        assert_eq!(Cadence::ALL.map(Cadence::fps), [60, 30, 24]);
        let refreshes = |rate: Cadence, second: u64| {
            (0..Cadence::SECOND as u64)
                .map(|f| rate.refreshes(second * Cadence::SECOND as u64 + f))
                .collect::<Vec<_>>()
        };
        let film = refreshes(Cadence::Film, 0);
        assert_eq!(
            film[..14],
            [
                true, false, false, true, false, true, false, false, true, false, true, false,
                false, true
            ]
        );
        assert_eq!(refreshes(Cadence::Half, 0)[..4], [true, false, true, false]);
        assert!(refreshes(Cadence::Full, 0).iter().all(|r| *r));
        for rate in Cadence::ALL {
            let first = refreshes(rate, 0);
            assert_eq!(first.iter().filter(|r| **r).count() as u32, rate.fps());
            let last = u64::MAX / Cadence::SECOND as u64 - 1;
            assert_eq!(first, refreshes(rate, last), "{rate:?} in the last second");
        }
    }

    #[test]
    fn the_frame_rate_knob_steps_through_the_rates_and_stops_at_the_slowest() {
        let mut params = p();
        let focus = Focus::default();
        assert_eq!(params.monitors[0].cadence, Cadence::Full);
        nudge(&mut params, Knob::FrameRate, 1.0);
        assert_eq!(params.monitors[0].cadence, Cadence::Half);
        nudge(&mut params, Knob::FrameRate, 1.0);
        assert_eq!(params.monitors[0].cadence, Cadence::Film);
        nudge(&mut params, Knob::FrameRate, 1.0);
        assert_eq!(params.monitors[0].cadence, Cadence::Film);
        assert_eq!(params.knob(Knob::FrameRate, focus), 2.0);
        assert!(params.monitors[1..]
            .iter()
            .all(|m| m.cadence == Cadence::Full));
        nudge(&mut params, Knob::FrameRate, -3.0);
        assert_eq!(params.monitors[0].cadence, Cadence::Full);
    }

    #[test]
    fn every_knob_moves_something() {
        for knob in Knob::ALL {
            // Both ways: a knob whose default sits on a rail — the switcher's
            // crosspoint does, at a full send — has room in one direction
            // only, and a knob that moves in neither is the broken one.
            let moved = [0.01f32, -0.01].map(|delta| {
                let mut params = p();
                nudge(&mut params, knob, step_for(knob, delta));
                params != p()
            });
            assert!(moved.iter().any(|m| *m), "{knob:?} did nothing");
        }
    }

    #[test]
    fn a_ratio_knob_steps_by_the_same_factor_wherever_it_stands() {
        // Named, so a knob demoted to a plain clamp cannot slip out of the
        // walk unnoticed.
        let mut ratios = Vec::new();
        for knob in Knob::ALL {
            let Limit::Ratio(low, high) = knob.limit(&p()) else {
                continue;
            };
            ratios.push(knob);
            // A rail at or below zero has no log, and would turn the first
            // step into a NaN the loop never sheds.
            assert!(low > 0.0, "{knob:?}");
            let focus = Focus::default();
            let middle = (low * high).sqrt();
            let half = knob.limit(&p()).travel() / 2.0;
            let mut params = p();
            params.set(knob, low, focus);
            params.nudge(knob, half, focus);
            assert!((params.knob(knob, focus) - middle).abs() < 1e-5, "{knob:?}");
            params.set(knob, high, focus);
            params.nudge(knob, -half, focus);
            assert!((params.knob(knob, focus) - middle).abs() < 1e-5, "{knob:?}");
            params.nudge(knob, 0.3, focus);
            params.nudge(knob, -0.3, focus);
            assert!((params.knob(knob, focus) - middle).abs() < 1e-5, "{knob:?}");
            params.nudge(knob, 2.0 * half + 1.0, focus);
            assert_eq!(params.knob(knob, focus), high, "{knob:?}");
            // Exactly, which a step through the log would not manage.
            params.set(knob, knob.identity(), focus);
            assert_eq!(params.knob(knob, focus), knob.identity(), "{knob:?}");
        }
        assert_eq!(ratios, [Knob::Zoom]);
    }

    #[test]
    fn a_knob_reads_exactly_the_index_it_is_stored_under() {
        for knob in Knob::ALL {
            let mut params = crate::config::instrument();
            params.delay = 1;
            let at = Focus::default();
            // Every neighbouring focus as it stood, since the rig's nodes
            // are not alike: what a side does not read must be exactly what
            // it was, and what it reads must be what the nudge left.
            let stood: Vec<f32> = Node::ALL
                .iter()
                .map(|node| params.knob(knob, at.with(*node, 1)))
                .collect();
            let before = params.knob(knob, at);
            params.nudge(knob, 1.0, at);
            if params.knob(knob, at) == before {
                params.nudge(knob, -1.0, at);
            }
            let after = params.knob(knob, at);
            assert_ne!(before, after, "{knob:?} did not move");
            for (i, node) in Node::ALL.into_iter().enumerate() {
                let elsewhere = params.knob(knob, at.with(node, 1));
                let want = match knob.node() == node {
                    true => stood[i],
                    false => after,
                };
                assert_eq!(elsewhere, want, "{knob:?} under {node:?}");
            }
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
        assert_eq!(params.shafts[0].zoom, 4.0);
        assert_eq!(cam.delay, params.delay);
        assert_eq!(params.rig.periods[0], crate::rig::MAX_PERIOD);
        assert_eq!(params.rig.switchers[0], 1.0);
        assert_eq!(mon.colour.saturation, 4.0);
        assert_eq!(mon.colour.brightness, 0.5);
        assert_eq!(mon.colour.contrast, 4.0);
        assert_eq!(mon.colour.temperature, 340.0);
        assert_eq!(mon.sharpness, 2.0);

        for _ in 0..10_000 {
            for knob in Knob::ALL {
                nudge(&mut params, knob, -1.0);
            }
        }
        let (cam, mon) = (&params.cameras[0], &params.monitors[0]);
        assert_eq!(params.shafts[0].zoom, 0.25);
        assert_eq!(cam.delay, 0);
        assert_eq!(params.rig.periods[0], 0);
        assert_eq!(params.rig.switchers[0], 0.0);
        assert_eq!(mon.colour.saturation, 0.0);
        assert_eq!(mon.colour.brightness, -0.5);
        assert_eq!(mon.colour.contrast, 0.0);
        assert_eq!(mon.colour.temperature, -100.0);
        assert_eq!(mon.sharpness, 0.0);
    }

    #[test]
    fn a_knob_follows_its_own_side_of_the_graph() {
        // Two cameras and two monitors: a camera knob nudged at focus (1, 0)
        // lands on camera 1 and nowhere else, and a monitor knob on monitor 0.
        let mut params = crate::config::instrument();
        let before = params.clone();
        params.nudge(
            Knob::Zoom,
            0.01,
            Focus {
                camera: 1,
                monitor: 0,
                switcher: 0,
            },
        );
        params.nudge(
            Knob::Hue,
            0.02,
            Focus {
                camera: 1,
                monitor: 0,
                switcher: 0,
            },
        );
        assert_eq!(params.shafts[0], before.shafts[0]);
        assert_ne!(params.shafts[1], before.shafts[1]);
        assert_ne!(params.monitors[0].colour, before.monitors[0].colour);
        assert_eq!(params.monitors[1], before.monitors[1]);
    }

    #[test]
    fn rotation_wraps_instead_of_running_away() {
        let mut params = p();
        params.shafts[0].rotation = 0.0;
        for _ in 0..10_000 {
            nudge(&mut params, Knob::Rotation, 0.5);
            let rotation = params.shafts[0].rotation;
            assert!(rotation > -PI && rotation <= PI);
        }
        assert!((params.shafts[0].rotation - wrap_pi(5000.0)).abs() < 1e-2);
    }

    #[test]
    fn wrap_pi_keeps_the_boundaries_it_promises() {
        assert!((wrap_pi(PI) - PI).abs() < 1e-6);
        assert!((wrap_pi(-PI) - PI).abs() < 1e-6);
        assert!(wrap_pi(0.0).abs() < 1e-6);
        assert!((wrap_pi(PI + 0.1) - (-PI + 0.1)).abs() < 1e-5);
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
            let (_, high) = knob.limit(&params).ends();
            let delta = if params.knob(knob, Focus::default()) >= high {
                -0.05
            } else {
                0.05
            };
            nudge(&mut params, knob, step_for(knob, delta));
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
        let params = crate::config::instrument();
        let at = params.describe(Focus {
            camera: 1,
            monitor: 0,
            switcher: 2,
        });
        assert!(at.contains("cam 2/3"), "{at}");
        assert!(at.contains("mon 1/5"), "{at}");
        assert!(at.contains("sw 3/4"), "{at}");
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

    /// The independent word on where identity is, beside the instrument
    /// [`Knob::identity`] reads it from: the two must agree or one lies.
    const IDENTITIES: [(Knob, f32); 12] = [
        (Knob::Zoom, 1.0),
        (Knob::Rotation, 0.0),
        (Knob::Delay, 0.0),
        (Knob::Hue, 0.0),
        (Knob::Saturation, 1.0),
        (Knob::Brightness, 0.0),
        (Knob::Contrast, 1.0),
        (Knob::Temperature, 0.0),
        (Knob::Sharpness, 0.0),
        (Knob::FrameRate, 0.0),
        (Knob::Switcher, 1.0),
        (Knob::Period, 0.0),
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
    fn the_period_reverses_on_the_exact_pass_boundary_and_stops_at_zero() {
        let mut params = crate::config::instrument();
        let stood = params.rig.switchers[0];
        params.rig.periods[0] = 3;
        for pass in 1..=12u64 {
            params.rig.beat(pass);
            let expect = if (pass / 3) % 2 == 1 {
                1.0 - stood
            } else {
                stood
            };
            assert_eq!(params.rig.switchers[0], expect, "pass {pass}");
            assert_eq!(
                params.rig.switchers[1],
                crate::rig::Rig::IDENTITY.switchers[1],
                "pass {pass}: the other switcher"
            );
        }
        params.rig.periods[0] = 0;
        for pass in 13..=30u64 {
            params.rig.beat(pass);
            assert_eq!(params.rig.switchers[0], stood, "pass {pass}");
        }
        // One grid for every switcher in the mode, and each keeps its own
        // beat.
        let other = params.rig.switchers[1];
        params.rig.periods[0] = 4;
        params.rig.periods[1] = 2;
        params.rig.beat(2);
        assert_eq!(params.rig.switchers[0], stood, "not this switcher's beat");
        assert_eq!(params.rig.switchers[1], 1.0 - other);
        params.rig.beat(4);
        assert_eq!(params.rig.switchers[0], 1.0 - stood);
        assert_eq!(params.rig.switchers[1], other);
        params.rig.beat(6);
        assert_eq!(params.rig.switchers[1], 1.0 - other);
    }

    #[test]
    fn the_period_knob_lands_on_whole_passes() {
        let mut params = p();
        let focus = Focus::default();
        assert_eq!(
            Knob::Period.limit(&params),
            Limit::Whole(crate::rig::MAX_PERIOD)
        );
        params.set(Knob::Period, 2.4, focus);
        assert_eq!(params.rig.periods[0], 2);
        params.set(Knob::Period, 2.6, focus);
        assert_eq!(params.knob(Knob::Period, focus), 3.0);
        params.set(Knob::Period, 900.0, focus);
        assert_eq!(params.rig.periods[0], crate::rig::MAX_PERIOD);
        assert!(params.describe(focus).contains("period 60"));
        params.reset(Knob::Period, focus);
        assert_eq!(params.rig.periods[0], 0);
    }

    #[test]
    fn the_delay_knob_lands_on_whole_frames_inside_the_reach() {
        let mut params = crate::config::instrument();
        params.delay = 4;
        let focus = Focus::default();
        assert_eq!(Knob::Delay.limit(&params), Limit::Whole(4));
        params.set(Knob::Delay, 2.4, focus);
        assert_eq!(params.cameras[0].delay, 2);
        assert_eq!(params.knob(Knob::Delay, focus), 2.0);
        params.set(Knob::Delay, 2.6, focus);
        assert_eq!(params.cameras[0].delay, 3);
        params.set(Knob::Delay, 9.0, focus);
        assert_eq!(params.cameras[0].delay, 4, "the reach is the rail");
        assert_eq!(params.cameras[1].delay, 0, "the other cable is its own");
        assert!(params.describe(focus).contains("delay 4/4"));
        params.set(Knob::Delay, 3.0, focus);
        assert!(
            params.describe(focus).contains("delay 3/4"),
            "cable, then reach"
        );
        params.reset(Knob::Delay, focus);
        assert_eq!(params.cameras[0].delay, 0);
        // With no reach there is nothing to dial: the knob's rail is the
        // reach, so it holds still at zero.
        params.delay = 0;
        params.nudge(Knob::Delay, 4.0, focus);
        assert_eq!(params.cameras[0].delay, 0);
    }

    #[test]
    fn every_identity_is_somewhere_its_own_knob_can_stand() {
        // An identity outside the travel is one the reset can never land on,
        // and `nudge` would quietly clamp it to a rail instead of saying so.
        for knob in Knob::ALL {
            let (low, high) = knob.limit(&p()).ends();
            let at = knob.identity();
            assert!(at >= low && at <= high, "{} is {at}", knob.name());
        }
    }

    #[test]
    fn resetting_a_knob_lands_it_on_its_identity_and_leaves_the_rest() {
        // Channels apart, so a reset of one of them moves the mean and the
        // rigid pair's exemption below is exercised rather than vacuous.
        let focus = Focus::default();
        for knob in Knob::ALL {
            let mut params = p();
            params.cameras[0].gain = [1.2, 0.6, 0.6];
            let before = params.clone();
            params.reset(knob, focus);
            assert!(
                (params.knob(knob, focus) - knob.identity()).abs() < 1e-6,
                "{} landed on {}",
                knob.name(),
                params.knob(knob, focus)
            );
            // And nothing else moved.
            for other in Knob::ALL {
                if other == knob {
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
    fn the_white_point_tints_grey_at_constant_luma_and_only_off_its_rest() {
        let grey = |m: [[f32; 3]; 3]| -> [f32; 3] { m.map(|row| row.iter().sum()) };
        let warm = grey(
            Colour {
                temperature: 340.0,
                ..Colour::NEUTRAL
            }
            .chroma_matrix(),
        );
        let cool = grey(
            Colour {
                temperature: -100.0,
                ..Colour::NEUTRAL
            }
            .chroma_matrix(),
        );
        assert!(
            warm[0] > warm[1] && warm[1] > warm[2],
            "warm white {warm:?}"
        );
        assert!(
            cool[2] > cool[1] && cool[1] > cool[0],
            "cool white {cool:?}"
        );
        for white in [warm, cool] {
            let luma: f32 = white.iter().zip(DECODE[0]).map(|(c, w)| c * w as f32).sum();
            assert!((luma - 1.0).abs() < 1e-5, "grey's luma moved: {white:?}");
        }
        // Rest is exactly rest, not the locus's own point near D65: the
        // subtraction that makes it so is what this pins.
        assert_eq!(white_shift(0.0), [0.0; 2]);
        // And how far the rails go, pinned: every other assertion here is an
        // ordering or an invariant, which a shift of the wrong size passes.
        // The candle grey is the displayed sRGB white of a 2025 K black body
        // at unit NTSC luma, nine parts red to one of blue, not the ninety
        // of linear light.
        for (got, want) in warm.iter().zip([1.5491, 0.8814, 0.1704]) {
            assert!((got - want).abs() < 2e-3, "warm white {warm:?}");
        }
        for (got, want) in cool.iter().zip([0.8715, 1.0093, 1.2892]) {
            assert!((got - want).abs() < 2e-3, "cool white {cool:?}");
        }
        // And hue leaves the white where it is: a turned chroma is not a
        // turned phosphor.
        let turned = grey(
            Colour {
                temperature: 340.0,
                hue: 2.0,
                ..Colour::NEUTRAL
            }
            .chroma_matrix(),
        );
        for (t, w) in turned.iter().zip(warm) {
            assert!(
                (t - w).abs() < 1e-6,
                "hue turned the white: {turned:?} vs {warm:?}"
            );
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
    fn no_two_knobs_share_a_name() {
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
            let mut params = crate::config::instrument();
            params.delay = 4;
            let focus = Focus {
                camera: 1,
                monitor: 1,
                switcher: 1,
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
            let (_, high) = knob.limit(&params).ends();
            let step = if params.knob(knob, focus) >= high {
                -STEP
            } else {
                STEP
            };
            let step = step_for(knob, step);
            params.nudge(knob, step, focus);
            for (other, was) in Knob::ALL.into_iter().zip(before) {
                let now = params.knob(other, focus);
                let expected = if other == knob {
                    match knob.limit(&params) {
                        Limit::Ratio(..) => was * step.exp(),
                        Limit::Clamp(..) | Limit::Whole(_) | Limit::Wrap => was + step,
                    }
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
        // What `set` is for: the value lands, and it lands inside the rails.
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
    }

    #[test]
    fn the_matrix_is_the_switchers_and_the_selects_and_nothing_else() {
        // The one direction: a crossfade moved changes what the monitors
        // show, and there is no cell anywhere to move instead.
        let mut params = crate::config::instrument();
        let focus = Focus {
            camera: 0,
            monitor: 0,
            switcher: 0,
        };
        // Switcher A stands between camera A and camera B, and structure A's
        // monitors are on its program.
        params.set(Knob::Switcher, 0.25, focus);
        assert!((params.route(0, 0) - 0.75).abs() < 1e-6);
        assert!((params.route(0, 1) - 0.25).abs() < 1e-6);
        params.set(Knob::Switcher, 1.0, focus);
        assert!((params.route(0, 0)).abs() < 1e-6);
        assert!((params.route(0, 1) - 1.0).abs() < 1e-6);
        // A select is the other half of it, and takes the monitor off the
        // program outright.
        params.rig.selects[0] = crate::rig::Select::Direct;
        assert!((params.route(0, 0) - 1.0).abs() < 1e-6);
        assert!((params.route(0, 1)).abs() < 1e-6);
        // The rotating monitor has no select and shows camera B whatever
        // either says.
        assert!((params.route(4, 1) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn a_knob_past_its_rail_is_refused_rather_than_snapped_later() {
        // The bug one range per knob closes: a file loading at a value the
        // first press would clamp away, leaving the instrument showing a
        // state no control can put it back into. Over
        // `Knob::ALL` rather than a list of the knobs that had the bug, so a
        // knob added later is covered the day it joins it.
        for knob in Knob::ALL {
            // Camera 1 and monitor 2, and the whole line compared rather than
            // hunted for the knob's name: this walk is the only thing left
            // standing behind that message, and against a node named 1 and a
            // node named 2 a knob reported against the wrong half of the
            // focus is caught by the number as well as by the word.
            let focus = Focus {
                camera: 1,
                monitor: 2,
                switcher: 0,
            };
            let name = knob.name();
            let node = match knob.node() {
                Node::Camera => format!("camera {}'s {name}", focus.camera),
                Node::Monitor => format!("monitor {}'s {name}", focus.monitor),
                Node::Switcher => format!("switcher {}'s {name}", focus.switcher),
            };
            let (low, high) = knob.limit(&crate::config::instrument()).ends();
            // A count has no field below zero to poison.
            let pasts = match knob {
                Knob::Delay | Knob::Period => vec![high + 1.0],
                Knob::FrameRate => continue,
                _ => vec![low - 1.0, high + 1.0],
            };
            for past in pasts {
                let mut params = crate::config::instrument();
                match knob {
                    Knob::Delay => params.cameras[focus.camera].delay = past as u32,
                    Knob::Period => params.rig.periods[focus.switcher] = past as u32,
                    _ => *params.knob_mut(knob, focus) = past,
                }
                let why = crate::config::validate(&params)
                    .expect_err(&format!("{name} loaded at {past}"));
                assert_eq!(why, format!("{node} is {past}; it runs {low} to {high}"));
            }
        }
    }
}
