//! Dave Blair's 4K Light Herder as one graph: the nodes his schematic names,
//! the four switchers and the router selects in front of them, and the
//! flattening of a setting of those into the one switcher this instrument
//! has.
//!
//! Every switcher on the rig is a crossfade between two feeds and a router
//! select picks one of two, so what any monitor shows is a weighted sum of
//! the three cameras and the seed input — which is what [`Params::routing`]
//! and each [`Plug::into`] already are. So the rig gets no mixer of its
//! own: [`Rig::params`] multiplies the chain out and writes the products into
//! the matrix, and from then on only the matrix exists. Switcher A and
//! the four selects land on the matrix directly; switchers B, C and D reach
//! a monitor only as products of one another, so a control on one of those would have to hold a [`Rig`] beside
//! the matrix and re-flatten — a second state to drift, which is why none is
//! held here.

use crate::affine::Framing;
use crate::input::Input;
use crate::params::{Camera, Character, Key, Monitor, Params, Plug};

/// In [`Params::cameras`] order. A and B are on the rotating, sliding shafts,
/// one per structure; the third watches the rotating monitor alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cam {
    A,
    B,
    Three,
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

/// The luma key the seed meets on its way into the switcher: passing from
/// mid-grey up and cutting to nothing a little below it, which is a lit
/// subject against an unlit room — what a camera pointed at a couch faces.
/// A fixed character of the rig, not a control: the board has no key.
const SEED_KEY: Key = Key {
    threshold: 0.35,
    softness: 0.08,
    hue: 0.0,
    tolerance: Key::TOLERANT,
};

/// The four M/Es, each a crossfade from its In1 to its In2. C and D are the
/// chain that brings the rotating monitor and the seed into structure B.
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

/// A setting of everything on the rig that routes. The rotating monitor has
/// no select: it shows camera B's feed, always.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rig {
    /// How far each switcher stands toward its In2, in [`Switcher`] order:
    /// 0 is In1 whole, 1 is In2 whole.
    pub switchers: [f32; 4],
    pub upper_a: Select,
    pub lower_a: Select,
    pub upper_b: Select,
    pub lower_b: Select,
}

/// One feed on the rig's cabling, as the share of each camera and of the
/// seed it carries. The shares sum to one: nothing on the path amplifies.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Feed {
    cameras: [f32; 3],
    seed: f32,
}

impl Feed {
    const SEED: Feed = Feed {
        cameras: [0.0; 3],
        seed: 1.0,
    };

    fn camera(cam: Cam) -> Feed {
        let mut cameras = [0.0; 3];
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
    /// Every structure monitor on its program — on Direct the switchers
    /// would feed nothing — with both cross-links a quarter open, so each
    /// structure is made of the other (Blair's "insanity mode") yet keeps
    /// a shape of its own; switcher C half open and D a tenth, which puts
    /// the seed on a B monitor at 0.0125 — an injection like `external`'s
    /// 0.014, though these coupled loops sit farther from unity, so the seed
    /// settles at about a third of its own brightness rather than nine
    /// tenths.
    pub const PERFORMANCE: Rig = Rig {
        switchers: [0.25, 0.25, 0.5, 0.1],
        upper_a: Select::Program,
        lower_a: Select::Program,
        upper_b: Select::Program,
        lower_b: Select::Program,
    };

    fn program(&self, switcher: Switcher) -> Feed {
        let (one, two) = match switcher {
            Switcher::A => (Feed::camera(Cam::A), Feed::camera(Cam::B)),
            Switcher::B => (Feed::camera(Cam::B), self.program(Switcher::C)),
            Switcher::C => (Feed::camera(Cam::A), self.program(Switcher::D)),
            Switcher::D => (Feed::camera(Cam::Three), Feed::SEED),
        };
        Feed::mix(one, two, self.switchers[switcher as usize])
    }

    fn shows(&self, screen: Screen) -> Feed {
        let (select, camera, switcher) = match screen {
            Screen::UpperA => (self.upper_a, Cam::A, Switcher::A),
            Screen::LowerA => (self.lower_a, Cam::A, Switcher::A),
            Screen::UpperB => (self.upper_b, Cam::B, Switcher::B),
            Screen::LowerB => (self.lower_b, Cam::B, Switcher::B),
            Screen::Rotating => return Feed::camera(Cam::B),
        };
        match select {
            Select::Direct => Feed::camera(camera),
            Select::Program => self.program(switcher),
        }
    }

    /// The rig at this setting, as the graph the instrument runs. Every
    /// camera pulls back a little and turns the same way at its own rate, so
    /// the structures stay distinct and no round trip cancels its own
    /// rotation, with the third turning fastest since its monitor turns on a
    /// shaft. The seed is the one physical camera, and the monitors are dark:
    /// on this rig the seed input is what sparks the loops.
    pub fn params(&self) -> Params {
        let camera = |cam: Cam, rotation: f32, gain: [f32; 3]| Camera {
            framing: Framing {
                zoom: 0.994,
                rotation,
                ..Framing::identity()
            },
            gain,
            character: Character::CLEAN,
            key: Key::OFF,
            look: Screen::ALL.map(|screen| glass(cam, screen)).to_vec(),
            delay: 0,
            divider: 1,
        };
        let feeds = Screen::ALL.map(|screen| self.shows(screen));
        Params {
            cameras: vec![
                camera(Cam::A, 0.05, [0.980, 0.986, 0.992]),
                camera(Cam::B, 0.08, [0.992, 0.986, 0.980]),
                camera(Cam::Three, 0.12, [0.985; 3]),
            ],
            monitors: vec![Monitor::default(); Screen::ALL.len()],
            inputs: vec![Plug {
                source: Input::Capture {
                    format: "v4l2".into(),
                    device: "/dev/video0".into(),
                },
                key: SEED_KEY,
                into: feeds.iter().map(|feed| feed.seed).collect(),
            }],
            routing: feeds.iter().map(|feed| feed.cameras.to_vec()).collect(),
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

    fn all(select: Select, switchers: [f32; 4]) -> Rig {
        Rig {
            switchers,
            upper_a: select,
            lower_a: select,
            upper_b: select,
            lower_b: select,
        }
    }

    const SETTINGS: [[f32; 4]; 4] = [
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
        let rig = Rig {
            switchers: [1.0, 0.0, 0.0, 0.0],
            lower_a: Select::Direct,
            ..all(Select::Program, [0.0; 4])
        };
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
    fn each_select_is_its_own_monitors() {
        let rig = Rig {
            switchers: [1.0; 4],
            upper_a: Select::Direct,
            lower_a: Select::Program,
            upper_b: Select::Program,
            lower_b: Select::Direct,
        };
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
                                upper_a: select(bits & 1 != 0),
                                lower_a: select(bits & 2 != 0),
                                upper_b: select(bits & 4 != 0),
                                lower_b: select(bits & 8 != 0),
                            };
                            for screen in Screen::ALL {
                                let feed = rig.shows(screen);
                                let sum: f32 = feed.cameras.iter().sum::<f32>() + feed.seed;
                                assert!(close(sum, 1.0), "{rig:?} {screen:?}: {feed:?}");
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn the_seed_is_the_one_physical_camera_keyed_on_its_way_in() {
        let params = Rig::PERFORMANCE.params();
        let [plug] = &params.inputs[..] else {
            panic!("the rig has one seed, not {}", params.inputs.len())
        };
        assert_eq!(
            plug.source,
            Input::Capture {
                format: "v4l2".into(),
                device: "/dev/video0".into(),
            }
        );
        assert_eq!(plug.key, SEED_KEY);
        assert!(plug.key.threshold > 0.0 && plug.key.softness > 0.0);
        assert!(params.cameras.iter().all(|c| c.key == Key::OFF));
    }

    #[test]
    fn every_camera_pulls_back_and_turns_the_same_way_at_its_own_rate() {
        let params = Rig::PERFORMANCE.params();
        let [a, b, three] = [&params.cameras[0], &params.cameras[1], &params.cameras[2]];
        for cam in [a, b, three] {
            assert!(cam.framing.zoom < 1.0, "{:?}", cam.framing);
        }
        assert!(0.0 < a.framing.rotation && a.framing.rotation < b.framing.rotation);
        assert!(b.framing.rotation < three.framing.rotation);
        // A and B tinted opposite ways, so the structures stay distinct.
        assert!(a.gain[0] < a.gain[2] && b.gain[0] > b.gain[2]);
    }

    #[test]
    fn the_performance_graph_is_these_rows() {
        // Written out rather than re-derived through `shows`, so a wrong
        // wire in the chain cannot agree with itself here.
        let params = Rig::PERFORMANCE.params();
        let rows = [
            [0.75, 0.25, 0.0],
            [0.75, 0.25, 0.0],
            [0.125, 0.75, 0.1125],
            [0.125, 0.75, 0.1125],
            [0.0, 1.0, 0.0],
        ];
        let seed = [0.0, 0.0, 0.0125, 0.0125, 0.0];
        for (m, (row, seed)) in rows.iter().zip(seed).enumerate() {
            let have = &params.routing[m];
            assert!(
                have.iter().zip(row).all(|(have, want)| close(*have, *want)),
                "monitor {m}: {have:?} is not {row:?}"
            );
            assert!(close(params.inputs[0].into[m], seed), "monitor {m}");
        }
        assert_eq!(params.cameras[0].look, [0.5, 0.5, 0.0, 0.0, 0.0]);
        assert_eq!(params.cameras[1].look, [0.0, 0.0, 0.5, 0.5, 0.0]);
        assert_eq!(params.cameras[2].look, [0.0, 0.0, 0.0, 0.0, 1.0]);
    }
}
