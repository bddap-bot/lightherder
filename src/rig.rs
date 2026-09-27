//! Dave Blair's 4K Light Herder: the nodes his schematic names, the four
//! switchers and the router selects in front of them, and the graph a
//! setting of those makes.
//!
//! Every switcher on the rig is a crossfade between two feeds — D keys its
//! In2, the seed, over its In1 — and a router select picks one of two, so
//! what any monitor shows is a weighted sum of the three cameras and the
//! seed, the weights moving with the key. [`Rig`] is that setting and the
//! whole of the routing state; [`Rig::feed`] multiplies the chain out on
//! demand, and no copy of the products is kept — a stored matrix would be a
//! second state standing beside the levers that set it, free to drift from
//! them.
//!
//! Cameras A and B each hand their picture to a frame delay unit, and at
//! every [`Point`] its camera's feed reaches the router takes the unit's
//! delayed output or its live one.

use std::fmt::{self, Write};

use crate::affine::Framing;
use crate::input::Input;
use crate::params::{Camera, Key, Monitor, Node, Params, Plug};

/// In [`Params::cameras`] order. A and B are on the rotating, sliding shafts,
/// one per structure; the third watches the rotating monitor alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cam {
    A,
    B,
    Three,
}

impl Cam {
    /// Switcher D's In1, which it keys the seed over: where the key cuts,
    /// this camera stands whole.
    const KEYED_OVER: Cam = Cam::Three;
}

/// The frame delay units, one on each rotating camera's cable and indexed
/// as the camera is: camera 3, past the end, has none.
pub const UNITS: usize = 2;

const _: () = assert!(Cam::A as usize == 0 && Cam::B as usize == 1 && Cam::Three as usize == UNITS);

/// The insertion points: the places a delay unit's camera feeds, Loop A's
/// three then Loop B's four, as the schematic's router blocks have them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Point {
    DirectA,
    InA1,
    InC1,
    InA2,
    InB1,
    Rotating,
    DirectB,
}

pub const POINTS: usize = Point::DirectB as usize + 1;

impl Point {
    fn camera(self) -> Cam {
        match self {
            Point::DirectA | Point::InA1 | Point::InC1 => Cam::A,
            Point::InA2 | Point::InB1 | Point::Rotating | Point::DirectB => Cam::B,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Point::DirectA => "A direct",
            Point::InA1 => "switcher A In1",
            Point::InC1 => "switcher C In1",
            Point::InA2 => "switcher A In2",
            Point::InB1 => "switcher B In1",
            Point::Rotating => "the rotating monitor",
            Point::DirectB => "B direct",
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Wire {
    Cable(Point),
    Three,
    Seed,
    Program(Switcher),
}

/// In [`Params::monitors`] order. A structure is an upper and a lower monitor
/// at a right angle with 50/50 glass at 45° between them; the fifth turns on
/// camera A's shaft.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Screen {
    UpperA,
    LowerA,
    UpperB,
    LowerB,
    Rotating,
}

impl Screen {
    const ALL: [Screen; 5] = [
        Screen::UpperA,
        Screen::LowerA,
        Screen::UpperB,
        Screen::LowerB,
        Screen::Rotating,
    ];

    const fn wiring(self) -> (Point, Option<Switcher>) {
        match self {
            Screen::UpperA | Screen::LowerA => (Point::DirectA, Some(Switcher::A)),
            Screen::UpperB | Screen::LowerB => (Point::DirectB, Some(Switcher::B)),
            Screen::Rotating => (Point::Rotating, None),
        }
    }
}

/// The luma key switcher D keys the seed over its In1 with: passing from
/// mid-grey up and cutting to nothing a little below it, which is a lit
/// subject against an unlit room — what a camera pointed at a couch faces.
/// Where it cuts, In1 stands whole. A fixed character of the rig, not a
/// control: the board has no key.
const SEED_KEY: Key = Key {
    threshold: 0.35,
    softness: 0.08,
};

/// The four M/Es, each a crossfade from its In1 to its In2 — D a keyer,
/// since its In2 is the seed. C and D are the chain that brings the rotating
/// monitor and the seed into structure B.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Switcher {
    A,
    B,
    C,
    D,
}

impl Switcher {
    const fn inputs(self) -> [Wire; 2] {
        match self {
            Switcher::A => [Wire::Cable(Point::InA1), Wire::Cable(Point::InA2)],
            Switcher::B => [Wire::Cable(Point::InB1), Wire::Program(Switcher::C)],
            Switcher::C => [Wire::Cable(Point::InC1), Wire::Program(Switcher::D)],
            Switcher::D => [Wire::Three, Wire::Seed],
        }
    }
}

/// A structure monitor's router crosspoint: its own camera direct, or its
/// switcher's program. One or the other, never a mix — mixing is the
/// switcher's job, one stage upstream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Select {
    Direct,
    Program,
}

/// Everything on the rig that routes, which is the whole of the routing
/// state: the matrix is worked out from this and held nowhere. The selects
/// are the structure monitors', in [`Params::monitors`] order — the
/// rotating monitor has none, since it shows camera B's feed always.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rig {
    /// How far each switcher stands toward its In2, in [`Switcher`] order:
    /// 0 is In1 whole, 1 is In2 whole.
    pub switchers: [f32; SWITCHERS],
    pub selects: [Select; SELECTS],
    /// How many passes apart each switcher's period reverses it. Zero is the
    /// mode off, and the only latch it has: the knob at its floor.
    pub periods: [u32; SWITCHERS],
    pub cut_lengths: [u32; SWITCHERS],
    pub patterns: [Pattern; SWITCHERS],
    owed: [bool; SWITCHERS],
    /// Flips are relative, so a knob moved mid-cut must not add or drop the
    /// flip back.
    cutting: [bool; SWITCHERS],
    /// Whether each switcher reversed on the last sixteenth played: a tap
    /// quantized back onto it owes a reversal only if that pass made none.
    reversed_on_sixteenth: [bool; SWITCHERS],
    /// How many frames each delay unit holds its camera's picture back, up
    /// to the graph's reach.
    pub delays: [u32; UNITS],
    /// Which insertion points take their unit's delayed output, indexed by
    /// [`Point`].
    pub inserted: [bool; POINTS],
}

/// The rig's counts, which are the instrument's: nothing chooses them.
pub const CAMERAS: usize = 3;

/// The shafts the cameras stand on. Two, not three: camera A and the
/// rotating monitor are belt-locked to one shaft and turn and slide in
/// unison, and camera 3 is fixed watching that monitor — so what camera 3
/// sees turns and slides with camera A, off the one number they share.
/// Camera B stands on its own post and is turned by its own hand: the
/// artist's two performers give the two cameras different rotations, and
/// the schematics draw two separate rotating-camera nodes.
pub const SHAFTS: usize = 2;

/// Which shaft each camera's view stands on, in [`Params::cameras`] order.
/// The lock is this table and the pair of shafts behind it: there is no
/// second number for the two to disagree on.
pub const SHAFT_OF: [usize; CAMERAS] = [0, 1, 0];

pub const MONITORS: usize = 5;
pub const SWITCHERS: usize = 4;

/// The monitors a router select stands in front of: every monitor but the
/// rotating one, which is wired to camera B and has no select to press. Its
/// own name rather than [`SWITCHERS`], which the rig happens to have as many
/// of.
pub const SELECTS: usize = MONITORS - 1;

/// The rig's count of `node`, by kind: how far the surface's vocabulary of
/// selects runs.
pub const fn count(node: Node) -> usize {
    match node {
        Node::Camera => CAMERAS,
        Node::Monitor => MONITORS,
        Node::Switcher => SWITCHERS,
    }
}

/// The longest period, in passes: a second. The original's rates are
/// unverified; a beat slower than that is a hand on the reversal, not a
/// rhythm.
pub const MAX_PERIOD: u32 = 60;

pub const SIXTEENTH: u64 = 8;
pub const BAR: u64 = 16 * SIXTEENTH;
const _: () = assert!(BAR == u128::BITS as u64);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pattern(u128);

impl Pattern {
    pub fn beats(self, pass: u64) -> bool {
        (self.0 >> (pass % BAR)) & 1 == 1
    }

    fn add(&mut self, pass: u64) {
        self.0 |= 1 << (pass % BAR);
    }
}

impl fmt::Display for Pattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let centred = self.0.rotate_left((SIXTEENTH / 2) as u32);
        (0..BAR / SIXTEENTH).try_for_each(|sixteenth| {
            let heard = (centred >> (sixteenth * SIXTEENTH)) & ((1 << SIXTEENTH) - 1) != 0;
            f.write_char(if heard { 'x' } else { '.' })
        })
    }
}

/// One feed on the rig's cabling, as the share of each camera and of the
/// seed it carries where the seed's key passes. The shares sum to one:
/// nothing on the path amplifies.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Feed {
    pub(crate) cameras: [f32; CAMERAS],
    pub(crate) seed: f32,
}

impl Feed {
    const SEED: Feed = Feed {
        cameras: [0.0; CAMERAS],
        seed: 1.0,
    };

    fn camera(cam: Cam) -> Feed {
        let mut cameras = [0.0; CAMERAS];
        cameras[cam as usize] = 1.0;
        Feed { cameras, seed: 0.0 }
    }

    fn mix(one: Feed, two: Feed, toward_two: f32) -> Feed {
        debug_assert!((0.0..=1.0).contains(&toward_two));
        let lerp = |a: f32, b: f32| a * (1.0 - toward_two) + b * toward_two;
        Feed {
            cameras: std::array::from_fn(|c| lerp(one.cameras[c], two.cameras[c])),
            seed: lerp(one.seed, two.seed),
        }
    }

    /// Camera `c`'s share where the seed's key cuts: the seed's whole share
    /// goes back to the camera D keyed it over, and every other camera's
    /// share is what it was — the key moves light between the seed and that
    /// one camera, so the shares still sum to one.
    pub(crate) fn cut(&self, c: usize) -> f32 {
        if c == Cam::KEYED_OVER as usize {
            self.cameras[c] + self.seed
        } else {
            self.cameras[c]
        }
    }
}

/// How much of `screen` is in front of `cam`'s lens: a structure's camera
/// sees its upper monitor directly and its lower one in the glass, at half
/// each; the third camera sees the rotating monitor whole.
fn glass(cam: Cam, screen: Screen) -> f32 {
    match (cam, screen) {
        (Cam::A, Screen::UpperA | Screen::LowerA) => 0.5,
        (Cam::B, Screen::UpperB | Screen::LowerB) => 0.5,
        (Cam::Three, Screen::Rotating) => 1.0,
        _ => 0.0,
    }
}

impl Rig {
    /// Every switcher at In2 and every monitor on its program: the routing
    /// that hands the seed the length of the chain with no loop closed. Every
    /// insertion point takes the delayed output, so a delay dialled in
    /// reaches everywhere its camera goes until one is taken out.
    pub const IDENTITY: Rig = Rig {
        switchers: [1.0; SWITCHERS],
        selects: [Select::Program; SELECTS],
        periods: [0; SWITCHERS],
        cut_lengths: [0; SWITCHERS],
        patterns: [Pattern(0); SWITCHERS],
        owed: [false; SWITCHERS],
        cutting: [false; SWITCHERS],
        reversed_on_sixteenth: [false; SWITCHERS],
        delays: [0; UNITS],
        inserted: [true; POINTS],
    };

    fn carries(&self, wire: Wire) -> Feed {
        match wire {
            Wire::Cable(point) => Feed::camera(point.camera()),
            Wire::Three => Feed::camera(Cam::Three),
            Wire::Seed => Feed::SEED,
            Wire::Program(switcher) => {
                let [one, two] = switcher.inputs().map(|wire| self.carries(wire));
                Feed::mix(one, two, self.switchers[switcher as usize])
            }
        }
    }

    fn on(&self, screen: Screen) -> Wire {
        let (direct, switcher) = screen.wiring();
        match (self.selects.get(screen as usize), switcher) {
            (Some(Select::Program), Some(switcher)) => Wire::Program(switcher),
            _ => Wire::Cable(direct),
        }
    }

    /// The insertion point on camera `c`'s feed into monitor `m`: none for
    /// camera 3, which has no unit, or where the select does not reach `c`.
    pub fn point(&self, c: usize, m: usize) -> Option<Point> {
        fn find(wire: Wire, c: usize) -> Option<Point> {
            match wire {
                Wire::Cable(point) => (point.camera() as usize == c).then_some(point),
                Wire::Program(switcher) => switcher.inputs().into_iter().find_map(|w| find(w, c)),
                Wire::Three | Wire::Seed => None,
            }
        }
        find(self.on(Screen::ALL[m]), c)
    }

    pub fn delayed(&self, c: usize, m: usize) -> Option<Point> {
        self.point(c, m)
            .filter(|point| self.inserted[*point as usize])
    }

    pub fn late(&self, c: usize, m: usize) -> u32 {
        self.delayed(c, m)
            .map_or(0, |point| self.delays[point.camera() as usize])
    }

    /// Put camera `c`'s unit in or out of its feed into monitor `m`, and say
    /// at which point: none, where that feed has none.
    pub fn insert(&mut self, c: usize, m: usize) -> Option<Point> {
        let point = self.point(c, m)?;
        let inserted = &mut self.inserted[point as usize];
        *inserted = !*inserted;
        Some(point)
    }

    /// What monitor `m` shows, as the share of each camera and of the seed:
    /// the matrix, worked out from the switchers and selects every time it is
    /// asked for rather than flattened into a copy that could stand apart
    /// from them.
    pub(crate) fn feed(&self, m: usize) -> Feed {
        self.shows(Screen::ALL[m])
    }

    /// Whether monitor `m` is on its switcher's program rather than on its
    /// own camera direct. The rotating monitor has no select and is never on
    /// a program: it shows camera B, always.
    pub fn on_program(&self, m: usize) -> bool {
        self.selects.get(m) == Some(&Select::Program)
    }

    /// Turn monitor `m`'s router select over, and whether there was one to
    /// turn: the rotating monitor has none.
    pub fn select(&mut self, m: usize) -> bool {
        let Some(select) = self.selects.get_mut(m) else {
            return false;
        };
        *select = match select {
            Select::Direct => Select::Program,
            Select::Program => Select::Direct,
        };
        true
    }

    /// The switcher's source reversal, and its momentary cut: In1 and In2
    /// trade places, which is the crossfade run to the other end of its
    /// travel. Its own inverse, so a cut held and let go leaves the rig
    /// exactly where it found it.
    pub fn flip(&mut self, switcher: usize) {
        self.switchers[switcher] = 1.0 - self.switchers[switcher];
    }

    pub fn beat(&mut self, pass: u64) {
        for i in 0..SWITCHERS {
            let (period, cut) = (u64::from(self.periods[i]), u64::from(self.cut_lengths[i]));
            let timed = match period {
                0 => self.cutting[i],
                _ if self.cutting[i] && (1..period).contains(&cut) => pass % period >= cut,
                _ => pass.is_multiple_of(period),
            };
            self.cutting[i] ^= timed;
            let reverses =
                std::mem::take(&mut self.owed[i]) || timed || self.patterns[i].beats(pass);
            if reverses {
                self.flip(i);
            }
            if pass.is_multiple_of(SIXTEENTH) {
                self.reversed_on_sixteenth[i] = reverses;
            }
        }
    }

    pub fn tap(&mut self, switcher: usize, pass: u64, quantize: bool) {
        let at = match quantize {
            true => (pass + SIXTEENTH / 2) / SIXTEENTH * SIXTEENTH,
            false => pass,
        };
        self.owed[switcher] |= at < pass
            && !self.reversed_on_sixteenth[switcher]
            && !self.patterns[switcher].beats(at);
        self.patterns[switcher].add(at);
    }

    fn shows(&self, screen: Screen) -> Feed {
        self.carries(self.on(screen))
    }

    /// The rig at this setting, as the graph the instrument runs: both shafts
    /// square on, every knob at its identity. The seed is the one physical camera, and the monitors are dark: on
    /// this rig the seed input is what sparks the loops.
    pub fn params(&self) -> Params {
        let camera = |cam: Cam, gain: [f32; 3]| Camera {
            gain,
            look: Screen::ALL.map(|screen| glass(cam, screen)),
        };
        Params {
            rig: *self,
            shafts: [Framing::identity(); SHAFTS],
            cameras: [
                camera(Cam::A, [0.980, 0.986, 0.992]),
                camera(Cam::B, [0.992, 0.986, 0.980]),
                camera(Cam::Three, [0.985; 3]),
            ],
            monitors: [Monitor::default(); MONITORS],
            input: Plug {
                source: Input::Capture {
                    format: "v4l2".into(),
                    device: "/dev/video0".into(),
                },
                key: SEED_KEY,
            },
            reach: Params::MAX_DELAY,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-6
    }

    fn assert_feed(feed: Feed, cameras: [f32; 3], seed: f32) {
        assert!(
            feed.cameras
                .iter()
                .zip(&cameras)
                .all(|(have, want)| close(*have, *want))
                && close(feed.seed, seed),
            "{feed:?} is not {cameras:?} + seed {seed}"
        );
    }

    fn all(select: Select, switchers: [f32; SWITCHERS]) -> Rig {
        Rig {
            switchers,
            selects: [select; SELECTS],
            ..Rig::IDENTITY
        }
    }

    const SETTINGS: [[f32; SWITCHERS]; 4] = [
        [0.0; 4],
        [1.0; 4],
        [0.3, 0.7, 0.1, 0.9],
        [0.25, 0.25, 0.5, 0.1],
    ];

    #[test]
    fn a_monitor_on_direct_shows_its_own_camera_whatever_the_switchers_say() {
        for switchers in SETTINGS {
            let rig = all(Select::Direct, switchers);
            assert_feed(rig.shows(Screen::UpperA), [1.0, 0.0, 0.0], 0.0);
            assert_feed(rig.shows(Screen::LowerA), [1.0, 0.0, 0.0], 0.0);
            assert_feed(rig.shows(Screen::UpperB), [0.0, 1.0, 0.0], 0.0);
            assert_feed(rig.shows(Screen::LowerB), [0.0, 1.0, 0.0], 0.0);
        }
    }

    #[test]
    fn upper_a_on_program_with_switcher_a_at_in2_reads_camera_b_and_nothing_else() {
        let mut rig = all(Select::Program, [1.0, 0.0, 0.0, 0.0]);
        rig.selects[1] = Select::Direct;
        assert_feed(rig.shows(Screen::UpperA), [0.0, 1.0, 0.0], 0.0);
        assert_feed(rig.shows(Screen::LowerA), [1.0, 0.0, 0.0], 0.0);
        assert_feed(rig.shows(Screen::UpperB), [0.0, 1.0, 0.0], 0.0);
    }

    #[test]
    fn the_seed_reaches_a_b_monitor_only_through_the_whole_chain() {
        let all_the_way = all(Select::Program, [0.5, 1.0, 1.0, 1.0]);
        assert_feed(all_the_way.shows(Screen::UpperB), [0.0; 3], 1.0);
        assert_feed(all_the_way.shows(Screen::LowerB), [0.0; 3], 1.0);
        // Structure A takes camera B's feed, never the seed: on the rig the
        // seed reaches A only as light already round B's loop.
        assert_feed(all_the_way.shows(Screen::UpperA), [0.5, 0.5, 0.0], 0.0);
        let rotating_instead = Rig {
            switchers: [0.5, 1.0, 1.0, 0.0],
            ..all_the_way
        };
        assert_feed(rotating_instead.shows(Screen::UpperB), [0.0, 0.0, 1.0], 0.0);
    }

    #[test]
    fn the_rotating_monitor_shows_camera_b_whatever_the_setting() {
        for switchers in SETTINGS {
            for select in [Select::Direct, Select::Program] {
                let rig = all(select, switchers);
                assert_feed(rig.shows(Screen::Rotating), [0.0, 1.0, 0.0], 0.0);
            }
        }
    }

    #[test]
    fn a_b_monitor_on_program_is_the_chain_multiplied_out() {
        let [a, b, c, d] = [0.3, 0.4, 0.6, 0.2];
        let rig = all(Select::Program, [a, b, c, d]);
        assert_feed(
            rig.shows(Screen::UpperB),
            [b * (1.0 - c), 1.0 - b, b * c * (1.0 - d)],
            b * c * d,
        );
        assert_feed(rig.shows(Screen::UpperA), [1.0 - a, a, 0.0], 0.0);
    }

    #[test]
    fn where_the_key_cuts_the_seed_hands_its_share_to_camera_three() {
        let [b, c, d] = [0.4, 0.6, 0.2];
        let rig = all(Select::Program, [0.3, b, c, d]);
        let feed = rig.shows(Screen::UpperB);
        let cut: [f32; CAMERAS] = std::array::from_fn(|c| feed.cut(c));
        assert!(
            cut.iter()
                .zip([b * (1.0 - c), 1.0 - b, b * c])
                .all(|(have, want)| close(*have, want)),
            "{cut:?}"
        );
        let direct = rig.shows(Screen::Rotating);
        assert_eq!(std::array::from_fn(|c| direct.cut(c)), direct.cameras);
    }

    #[test]
    fn each_select_is_its_own_monitors() {
        let mut rig = all(Select::Program, [1.0; SWITCHERS]);
        rig.selects[0] = Select::Direct;
        rig.selects[3] = Select::Direct;
        assert_feed(rig.shows(Screen::UpperA), [1.0, 0.0, 0.0], 0.0);
        assert_feed(rig.shows(Screen::LowerA), [0.0, 1.0, 0.0], 0.0);
        assert_feed(rig.shows(Screen::UpperB), [0.0; 3], 1.0);
        assert_feed(rig.shows(Screen::LowerB), [0.0, 1.0, 0.0], 0.0);
    }

    #[test]
    fn every_feed_sums_to_one() {
        // The rig never amplifies: at any setting, on every monitor, the
        // shares of the cameras and the seed are the whole picture.
        let positions = [0.0, 0.25, 0.5, 0.9, 1.0];
        let select = |bit: bool| if bit { Select::Program } else { Select::Direct };
        for &a in &positions {
            for &b in &positions {
                for &c in &positions {
                    for &d in &positions {
                        for bits in 0..16u8 {
                            let rig = Rig {
                                switchers: [a, b, c, d],
                                selects: std::array::from_fn(|i| select(bits >> i & 1 != 0)),
                                ..Rig::IDENTITY
                            };
                            for screen in Screen::ALL {
                                let feed = rig.shows(screen);
                                let cameras: f32 = feed.cameras.iter().sum();
                                let cut: f32 = (0..CAMERAS).map(|c| feed.cut(c)).sum();
                                assert!(
                                    close(cameras + feed.seed, 1.0) && close(cut, 1.0),
                                    "{rig:?} {screen:?}: {feed:?}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn the_seed_is_the_one_physical_camera_keyed_on_its_way_in() {
        let params = Rig::IDENTITY.params();
        let plug = &params.input;
        assert_eq!(
            plug.source,
            Input::Capture {
                format: "v4l2".into(),
                device: "/dev/video0".into(),
            }
        );
        assert_eq!(plug.key, SEED_KEY);
        assert!(plug.key.threshold > 0.0 && plug.key.softness > 0.0);
    }

    #[test]
    fn the_shafts_start_square_on_and_the_cables_lose_a_little() {
        let params = Rig::IDENTITY.params();
        assert_eq!(params.shafts, [Framing::identity(); SHAFTS]);
        for camera in &params.cameras {
            assert!(
                camera.gain.iter().all(|g| 0.9 < *g && *g < 1.0),
                "{camera:?}"
            );
        }
        let (a, b) = (&params.cameras[0], &params.cameras[1]);
        assert!(a.gain[0] < a.gain[2] && b.gain[0] > b.gain[2]);
    }

    #[test]
    fn the_identity_graph_is_these_rows() {
        let params = Rig::IDENTITY.params();
        let rows = [
            [0.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0],
            [0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
        ];
        let seed = [0.0, 0.0, 1.0, 1.0, 0.0];
        for (m, (row, seed)) in rows.iter().zip(seed).enumerate() {
            for (c, want) in row.iter().enumerate() {
                let have = params.route(m, c);
                assert!(
                    close(have, *want),
                    "monitor {m} camera {c}: {have} is not {want}"
                );
            }
            assert!(close(params.send(m), seed), "monitor {m}");
        }
        assert_eq!(params.cameras[0].look, [0.5, 0.5, 0.0, 0.0, 0.0]);
        assert_eq!(params.cameras[1].look, [0.0, 0.0, 0.5, 0.5, 0.0]);
        assert_eq!(params.cameras[2].look, [0.0, 0.0, 0.0, 0.0, 1.0]);
    }

    fn heard(
        rig: &mut Rig,
        passes: std::ops::RangeInclusive<u64>,
        taps: &[(u64, usize, bool)],
    ) -> [Vec<u64>; SWITCHERS] {
        let mut heard: [Vec<u64>; SWITCHERS] = Default::default();
        for pass in passes {
            let was = rig.switchers;
            for &(_, switcher, quantize) in taps.iter().filter(|tap| tap.0 == pass) {
                rig.tap(switcher, pass, quantize);
            }
            rig.beat(pass);
            for (i, reversals) in heard.iter_mut().enumerate() {
                if rig.switchers[i] != was[i] {
                    reversals.push(pass);
                }
            }
        }
        heard
    }

    #[test]
    fn a_tapped_beat_reverses_its_switcher_on_its_own_pass_every_bar() {
        let mut rig = Rig::IDENTITY;
        let heard = heard(&mut rig, 1..=3 * BAR, &[(5, 1, false), (43, 1, false)]);
        assert_eq!(
            heard,
            [
                vec![],
                vec![5, 43, BAR + 5, BAR + 43, 2 * BAR + 5, 2 * BAR + 43],
                vec![],
                vec![],
            ]
        );
    }

    #[test]
    fn the_log_shows_each_beat_on_the_sixteenth_it_is_nearest() {
        let mut rig = Rig::IDENTITY;
        for pass in [3, 4, 67, BAR - 4] {
            rig.tap(0, pass, false);
        }
        assert_eq!(rig.patterns[0].to_string(), "xx......x.......");
        assert_eq!(rig.patterns[1].to_string(), "................");
    }

    #[test]
    fn a_quantized_tap_lands_on_the_nearest_sixteenth_of_one_grid_for_every_switcher() {
        let mut rig = Rig::IDENTITY;
        let taps = [(12, 3, true), (19, 0, true), (29, 2, true), (29, 1, false)];
        let heard = heard(&mut rig, 1..=2 * BAR + 40, &taps);
        assert_eq!(
            heard,
            [
                vec![19, BAR + 16, 2 * BAR + 16],
                vec![29, BAR + 29, 2 * BAR + 29],
                vec![32, BAR + 32, 2 * BAR + 32],
                vec![16, BAR + 16, 2 * BAR + 16],
            ]
        );
    }

    #[test]
    fn a_tap_on_a_beat_already_there_adds_nothing() {
        let mut rig = Rig::IDENTITY;
        rig.tap(0, 9, true);
        let once = rig;
        assert_ne!(once, Rig::IDENTITY);
        rig.tap(0, 11, true);
        assert_eq!(rig, once);
        rig.tap(0, 8, false);
        assert_eq!(rig, once);
    }

    #[test]
    fn a_period_and_a_pattern_beating_on_one_pass_reverse_it_once() {
        let mut rig = Rig::IDENTITY;
        rig.periods[0] = 8;
        let heard = heard(&mut rig, 1..=BAR + 16, &[(12, 0, false), (19, 0, true)]);
        let mut want: Vec<u64> = (1..=(BAR + 16) / 8).map(|k| 8 * k).collect();
        want.extend([12, BAR + 12]);
        want.sort_unstable();
        assert_eq!(heard[0], want);
    }

    #[test]
    fn a_late_tap_on_a_pass_that_reverses_anyway_is_that_reversal() {
        let mut rig = Rig::IDENTITY;
        rig.periods[0] = 7;
        let mut want: Vec<u64> = (1..=(BAR + 48) / 7).map(|k| 7 * k).collect();
        want.push(BAR + 48);
        assert_eq!(heard(&mut rig, 1..=BAR + 48, &[(49, 0, true)])[0], want);
        for taps in [
            [(17, 1, true), (17, 1, false)],
            [(17, 1, false), (17, 1, true)],
        ] {
            let mut rig = Rig::IDENTITY;
            assert_eq!(heard(&mut rig, 1..=20, &taps)[1], [17], "{taps:?}");
        }
    }

    #[test]
    fn a_cut_length_holds_each_beat_of_the_period_that_long_and_then_cuts_back() {
        let mut rig = Rig::IDENTITY;
        rig.periods[2] = 8;
        rig.cut_lengths[2] = 3;
        assert_eq!(
            heard(&mut rig, 1..=30, &[]),
            [vec![], vec![], vec![8, 11, 16, 19, 24, 27], vec![]]
        );
        for cut in [0, 8, 9, MAX_PERIOD] {
            let mut rig = Rig::IDENTITY;
            rig.periods[2] = 8;
            rig.cut_lengths[2] = cut;
            assert_eq!(heard(&mut rig, 1..=30, &[])[2], [8, 16, 24], "cut {cut}");
        }
    }

    #[test]
    fn a_knob_turned_under_a_running_period_never_inverts_its_cut() {
        let away = |rig: &Rig| rig.switchers[0] != Rig::IDENTITY.switchers[0];
        for (from, to) in [(8, 5), (8, 12), (12, 8), (5, 5)] {
            for cuts in [[0, 3], [3, 0], [1, 7], [7, 1], [4, 13], [13, 4], [2, 2]] {
                for moved_at in u64::from(from)..3 * u64::from(from) {
                    let mut rig = Rig::IDENTITY;
                    rig.periods[0] = from;
                    rig.cut_lengths[0] = cuts[0];
                    heard(&mut rig, 1..=moved_at - 1, &[]);
                    rig.periods[0] = to;
                    rig.cut_lengths[0] = cuts[1];
                    let (period, cut) = (u64::from(to), u64::from(cuts[1]));
                    for pass in moved_at..moved_at + 4 * period {
                        let was = away(&rig);
                        rig.beat(pass);
                        let case = format!("{from} {cuts:?} -> {to} at {moved_at}: pass {pass}");
                        match cut != 0 && cut < period {
                            true if pass >= moved_at + 2 * period => {
                                assert_eq!(away(&rig), pass % period < cut, "{case}");
                            }
                            true => {}
                            false => {
                                assert_eq!(away(&rig) != was, pass.is_multiple_of(period), "{case}")
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn a_cut_shortened_past_where_it_stands_comes_back_on_the_next_pass() {
        let mut rig = Rig::IDENTITY;
        rig.periods[0] = 16;
        rig.cut_lengths[0] = 8;
        assert_eq!(heard(&mut rig, 1..=20, &[])[0], [16]);
        rig.cut_lengths[0] = 2;
        assert_eq!(heard(&mut rig, 21..=40, &[])[0], [21, 32, 34]);
    }

    #[test]
    fn the_period_at_its_floor_undoes_the_reversal_it_holds() {
        for cut in [0, 6] {
            let mut rig = Rig::IDENTITY;
            rig.periods[0] = 16;
            rig.cut_lengths[0] = cut;
            assert_eq!(heard(&mut rig, 1..=17, &[])[0], [16], "cut {cut}");
            rig.periods[0] = 0;
            assert_eq!(heard(&mut rig, 18..=60, &[])[0], [18], "cut {cut}");
            assert_eq!(rig.switchers, Rig::IDENTITY.switchers, "cut {cut}");
        }
    }

    #[test]
    fn a_late_tap_on_the_pass_a_cut_came_back_on_is_that_reversal() {
        let mut rig = Rig::IDENTITY;
        rig.periods[0] = 16;
        rig.cut_lengths[0] = 8;
        let every_sixteenth: Vec<u64> = (2..=2 * BAR / 8).map(|k| 8 * k).collect();
        assert_eq!(
            heard(&mut rig, 1..=2 * BAR, &[(26, 0, true)])[0],
            every_sixteenth
        );
        assert!(rig.patterns[0].beats(24));
    }

    #[test]
    fn a_late_tap_on_the_pass_an_emptied_pattern_reversed_is_that_reversal() {
        let mut rig = Rig::IDENTITY;
        assert_eq!(
            heard(&mut rig, 1..=BAR + 16, &[(16, 0, false)])[0],
            [16, BAR + 16]
        );
        rig.patterns[0] = Pattern::default();
        assert_eq!(
            heard(&mut rig, BAR + 17..=2 * BAR + 16, &[(BAR + 18, 0, true)])[0],
            [2 * BAR + 16]
        );
    }

    #[test]
    fn a_camera_reaches_a_monitor_through_the_insertion_point_its_wiring_passes() {
        // Loop A's three points for camera A and Loop B's four for camera B,
        // whatever the switchers stand at.
        use Point::*;
        let way = |rig: &Rig, m: usize| -> [Option<Point>; CAMERAS] {
            std::array::from_fn(|c| rig.point(c, m))
        };
        for switchers in SETTINGS {
            let direct = all(Select::Direct, switchers);
            let program = all(Select::Program, switchers);
            for m in [0, 1] {
                assert_eq!(way(&direct, m), [Some(DirectA), None, None]);
                assert_eq!(way(&program, m), [Some(InA1), Some(InA2), None]);
            }
            for m in [2, 3] {
                assert_eq!(way(&direct, m), [None, Some(DirectB), None]);
                assert_eq!(way(&program, m), [Some(InC1), Some(InB1), None]);
            }
            for rig in [&direct, &program] {
                assert_eq!(way(rig, 4), [None, Some(Rotating), None]);
            }
        }
    }

    #[test]
    fn each_camera_enters_each_monitor_once_at_most() {
        // What lets a camera's share of a monitor arrive late or on time and
        // never split: a camera wired in twice would have two insertion points
        // and one share to divide between them.
        fn cameras(wire: Wire, into: &mut Vec<Cam>) {
            match wire {
                Wire::Cable(point) => into.push(point.camera()),
                Wire::Three => into.push(Cam::Three),
                Wire::Program(switcher) => {
                    for wire in switcher.inputs() {
                        cameras(wire, into);
                    }
                }
                Wire::Seed => {}
            }
        }
        let select = |bit: bool| if bit { Select::Program } else { Select::Direct };
        for bits in 0..16u8 {
            let rig = Rig {
                selects: std::array::from_fn(|i| select(bits >> i & 1 != 0)),
                ..Rig::IDENTITY
            };
            for screen in Screen::ALL {
                let mut seen = Vec::new();
                cameras(rig.on(screen), &mut seen);
                for (i, cam) in seen.iter().enumerate() {
                    assert!(
                        !seen[i + 1..].contains(cam),
                        "{screen:?} {bits:04b}: {seen:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_delay_reaches_a_monitor_only_through_an_insertion_point_that_takes_it() {
        let late = |rig: &Rig| -> Vec<[u32; CAMERAS]> {
            (0..MONITORS)
                .map(|m| std::array::from_fn(|c| rig.late(c, m)))
                .collect()
        };
        let mut rig = all(Select::Program, [0.5; SWITCHERS]);
        rig.delays = [3, 5];
        // Every point in, as the rig starts: each rotating camera is
        // late by its own unit wherever it goes, and camera 3 never is.
        assert_eq!(
            late(&rig),
            [[3, 5, 0], [3, 5, 0], [3, 5, 0], [3, 5, 0], [0, 5, 0]]
        );
        // Switcher A's In1 out: camera A is on time on both of structure A's
        // monitors, which that one input feeds, and still late into
        // structure B through switcher C.
        assert_eq!(rig.insert(0, 1), Some(Point::InA1));
        assert_eq!(
            late(&rig),
            [[0, 5, 0], [0, 5, 0], [3, 5, 0], [3, 5, 0], [0, 5, 0]]
        );
        assert_eq!(rig.delayed(0, 0), None);
        assert_eq!(rig.delayed(0, 2), Some(Point::InC1));
        // The select decides which point the feed passes: on direct, upper A
        // takes camera A through A direct, which is still in.
        rig.selects[0] = Select::Direct;
        assert_eq!(late(&rig)[0], [3, 0, 0]);
        assert_eq!(late(&rig)[1], [0, 5, 0]);
        // No feed through a unit, nothing to put in or take out.
        let before = rig;
        assert_eq!(rig.insert(2, 4), None, "camera 3 has no unit");
        assert_eq!(
            rig.insert(1, 0),
            None,
            "camera B does not reach upper A on direct"
        );
        assert_eq!(
            rig.insert(0, 4),
            None,
            "camera A does not reach the rotating monitor"
        );
        assert_eq!(rig, before);
        // Back in, and a unit at zero is on time wherever it is in.
        assert_eq!(rig.insert(0, 1), Some(Point::InA1));
        assert_eq!(late(&rig)[1], [3, 5, 0]);
        rig.delays = [0; UNITS];
        assert!(late(&rig).iter().flatten().all(|late| *late == 0));
    }
}
