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

/// In [`Params::monitors`] order. A structure is an upper and a lower monitor
/// at a right angle with 50/50 glass at 45° between them; the fifth turns on
/// a shaft of its own.
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
    /// Passes between reversals of each switcher. Zero is the mode off, and
    /// the only latch it has: the knob at its floor.
    pub periods: [u32; SWITCHERS],
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
    /// that hands the seed the length of the chain with no loop closed.
    pub const IDENTITY: Rig = Rig {
        switchers: [1.0; SWITCHERS],
        selects: [Select::Program; SELECTS],
        periods: [0; SWITCHERS],
    };

    fn program(&self, switcher: Switcher) -> Feed {
        let (one, two) = match switcher {
            Switcher::A => (Feed::camera(Cam::A), Feed::camera(Cam::B)),
            Switcher::B => (Feed::camera(Cam::B), self.program(Switcher::C)),
            Switcher::C => (Feed::camera(Cam::A), self.program(Switcher::D)),
            Switcher::D => (Feed::camera(Cam::KEYED_OVER), Feed::SEED),
        };
        Feed::mix(one, two, self.switchers[switcher as usize])
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

    /// Every switcher whose period divides `pass` reverses. Counted from the
    /// start of the run rather than from when a period was dialled in, so
    /// every switcher in the mode beats on one grid — which is what the
    /// original's quantizing is for. The grid is in passes, so nothing here
    /// reads a clock.
    pub fn beat(&mut self, pass: u64) {
        for i in 0..SWITCHERS {
            let period = u64::from(self.periods[i]);
            if period != 0 && pass.is_multiple_of(period) {
                self.flip(i);
            }
        }
    }

    fn shows(&self, screen: Screen) -> Feed {
        let (select, camera, switcher) = match screen {
            Screen::UpperA => (self.selects[0], Cam::A, Switcher::A),
            Screen::LowerA => (self.selects[1], Cam::A, Switcher::A),
            Screen::UpperB => (self.selects[2], Cam::B, Switcher::B),
            Screen::LowerB => (self.selects[3], Cam::B, Switcher::B),
            Screen::Rotating => return Feed::camera(Cam::B),
        };
        match select {
            Select::Direct => Feed::camera(camera),
            Select::Program => self.program(switcher),
        }
    }

    /// The rig at this setting, as the graph the instrument runs: both shafts
    /// square on, every knob at its identity. The seed is the one physical camera, and the monitors are dark: on
    /// this rig the seed input is what sparks the loops.
    pub fn params(&self) -> Params {
        let camera = |cam: Cam, gain: [f32; 3]| Camera {
            gain,
            look: Screen::ALL.map(|screen| glass(cam, screen)),
            delay: 0,
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
            // Two frames, not the original's thirty: a frame of reach is a copy
            // of all five monitors, and the bank cap at 4K holds about four.
            delay: 2,
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
            periods: [0; SWITCHERS],
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
                                periods: [0; SWITCHERS],
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
}
