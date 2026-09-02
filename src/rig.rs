//! Dave Blair's 4K Light Herder as one graph: the nodes his August 2026
//! schematic names, the four switchers and the router selects between them,
//! and the flattening of a setting of those into the one switcher this
//! instrument has.
//!
//! Every switcher on the rig is a crossfade between two feeds, and a router
//! select picks one of two, so what any monitor shows is a weighted sum of
//! the three cameras and the seed input. [`Params::routing`] already *is* a
//! weighted sum of cameras per monitor and [`Params::routing_inputs`] the
//! same for inputs, so the rig needs no mixer of its own: [`Rig::params`]
//! multiplies the chain out and writes the products into the matrix. The
//! names live here so that a control on "switcher A" has one place that
//! says which crosspoints it moves.

use crate::affine::Framing;
use crate::input::{Input, Pattern};
use crate::params::{Camera, Character, Key, Monitor, Params};

/// The rig's cameras, in [`Params::cameras`] order. Two Sony a7S III on
/// rotating, sliding shafts, one per structure, and an a7 II that watches the
/// rotating monitor and nothing else.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cam {
    A,
    B,
    Three,
}

/// The rig's monitors, in [`Params::monitors`] order. Each structure is an
/// upper and a lower monitor at a right angle with 50/50 glass at 45°
/// between them, so its camera sees the upper directly and the lower in
/// reflection at equal weight; the fifth is on a shaft of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    UpperA,
    LowerA,
    UpperB,
    LowerB,
    Rotating,
}

impl Screen {
    pub const ALL: [Screen; 5] = [
        Screen::UpperA,
        Screen::LowerA,
        Screen::UpperB,
        Screen::LowerB,
        Screen::Rotating,
    ];
}

/// The four M/Es of the ATEM, each a crossfade from its In1 to its In2.
/// A feeds structure A's monitors, B structure B's; C and D are the chain
/// that brings the rotating monitor and the seed into B: D mixes camera 3
/// with the seed, C mixes camera A with D's program, B mixes camera B with
/// C's program, and A mixes camera A with camera B's feed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Switcher {
    A,
    B,
    C,
    D,
}

/// Which of its two router inputs a structure monitor shows: its structure's
/// camera direct, or its structure's switcher program. A router crosspoint,
/// so one or the other and never a mix — a mix of the two is the switcher's
/// job, one stage upstream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum In {
    One,
    Two,
}

/// A setting of everything on the rig that routes: where each switcher's
/// crossfade stands and which input each structure monitor is on. The
/// rotating monitor has no select — it shows camera B's feed, always.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rig {
    /// How far each switcher stands toward its In2, in [`Switcher`] order:
    /// 0 is In1 whole, 1 is In2 whole.
    pub switchers: [f32; 4],
    /// Upper A, lower A, upper B, lower B.
    pub selects: [In; 4],
}

/// One feed on the rig's cabling, as the share of each camera and of the
/// seed it carries. Every feed's shares sum to one, since nothing on the
/// path amplifies — a switcher mixes and a router picks.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Feed {
    pub cameras: [f32; 3],
    pub seed: f32,
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
        let lerp = |a: f32, b: f32| a * (1.0 - toward_two) + b * toward_two;
        Feed {
            cameras: std::array::from_fn(|c| lerp(one.cameras[c], two.cameras[c])),
            seed: lerp(one.seed, two.seed),
        }
    }
}

impl Rig {
    /// Every structure monitor on its switcher program, both cross-links a
    /// quarter open, the rotating monitor half of switcher C and the seed a
    /// tenth of switcher D. On In1 the switchers would feed nothing, so the
    /// programs are what the preset shows; the cross-links are Blair's
    /// "insanity mode", both structures made of each other, held to a
    /// quarter so each keeps a shape of its own. The seed's share of a B
    /// monitor comes to 0.0125 — the trickle `external` injects, for the
    /// same reason: a loop this close to unity settles at the trickle
    /// divided by its distance from unity.
    pub const PERFORMANCE: Rig = Rig {
        switchers: [0.25, 0.25, 0.5, 0.1],
        selects: [In::Two; 4],
    };

    pub fn program(&self, switcher: Switcher) -> Feed {
        let (one, two) = match switcher {
            Switcher::A => (Feed::camera(Cam::A), Feed::camera(Cam::B)),
            Switcher::B => (Feed::camera(Cam::B), self.program(Switcher::C)),
            Switcher::C => (Feed::camera(Cam::A), self.program(Switcher::D)),
            Switcher::D => (Feed::camera(Cam::Three), Feed::SEED),
        };
        Feed::mix(one, two, self.switchers[switcher as usize])
    }

    /// What the router puts on a monitor.
    pub fn shows(&self, screen: Screen) -> Feed {
        let (camera, switcher) = match screen {
            Screen::UpperA | Screen::LowerA => (Cam::A, Switcher::A),
            Screen::UpperB | Screen::LowerB => (Cam::B, Switcher::B),
            Screen::Rotating => return Feed::camera(Cam::B),
        };
        match self.selects[screen as usize] {
            In::One => Feed::camera(camera),
            In::Two => self.program(switcher),
        }
    }

    /// The rig at this setting, as the graph the instrument runs. The
    /// framings and gains are `crossed`'s idiom — every camera pulls back
    /// a little and turns the same way at its own rate, so the structures
    /// stay distinct and no round trip cancels its own rotation — with the
    /// rotating monitor's camera turning fastest, since on the rig that
    /// monitor turns on its own shaft. The seed is the bars, standing in for
    /// the media player on the schematic; the monitors are dark, because on
    /// this rig the seed input is what sparks the loops.
    pub fn params(&self) -> Params {
        let camera = |cam: Cam, rotation: f32, gain: [f32; 3]| {
            let mut look = [0.0; 5];
            match cam {
                Cam::A => look[..2].fill(0.5),
                Cam::B => look[2..4].fill(0.5),
                Cam::Three => look[4] = 1.0,
            }
            Camera {
                framing: Framing {
                    zoom: 0.994,
                    rotation,
                    translate: [0.0, 0.0],
                },
                gain,
                character: Character::CLEAN,
                key: Key::OFF,
                look: look.to_vec(),
                delay: 0,
            }
        };
        let feeds = Screen::ALL.map(|screen| self.shows(screen));
        Params {
            cameras: vec![
                camera(Cam::A, 0.05, [0.980, 0.986, 0.992]),
                camera(Cam::B, 0.08, [0.992, 0.986, 0.980]),
                camera(Cam::Three, 0.12, [0.985; 3]),
            ],
            monitors: vec![Monitor::default(); Screen::ALL.len()],
            inputs: vec![Input::Pattern(Pattern::Bars)],
            routing: feeds.iter().map(|feed| feed.cameras.to_vec()).collect(),
            routing_inputs: vec![feeds.iter().map(|feed| feed.seed).collect()],
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

    #[test]
    fn a_monitor_on_in1_shows_its_own_camera_whatever_the_switchers_say() {
        let rig = Rig {
            switchers: [1.0; 4],
            selects: [In::One; 4],
        };
        assert_feed(rig.shows(Screen::UpperA), [1.0, 0.0, 0.0], 0.0);
        assert_feed(rig.shows(Screen::LowerA), [1.0, 0.0, 0.0], 0.0);
        assert_feed(rig.shows(Screen::UpperB), [0.0, 1.0, 0.0], 0.0);
        assert_feed(rig.shows(Screen::LowerB), [0.0, 1.0, 0.0], 0.0);
    }

    #[test]
    fn upper_a_on_in2_with_switcher_a_at_in2_reads_camera_b_and_nothing_else() {
        let rig = Rig {
            switchers: [1.0, 0.0, 0.0, 0.0],
            selects: [In::Two, In::One, In::Two, In::Two],
        };
        assert_feed(rig.shows(Screen::UpperA), [0.0, 1.0, 0.0], 0.0);
        // The select is per monitor: the lower one stays on its camera.
        assert_feed(rig.shows(Screen::LowerA), [1.0, 0.0, 0.0], 0.0);
        // Switcher B at In1 is camera B whole, wherever C and D stand.
        assert_feed(rig.shows(Screen::UpperB), [0.0, 1.0, 0.0], 0.0);
    }

    #[test]
    fn the_seed_reaches_a_b_monitor_only_through_the_whole_chain() {
        let all_the_way = Rig {
            switchers: [0.0, 1.0, 1.0, 1.0],
            selects: [In::Two; 4],
        };
        assert_feed(all_the_way.shows(Screen::UpperB), [0.0; 3], 1.0);
        assert_feed(all_the_way.shows(Screen::LowerB), [0.0; 3], 1.0);
        // Structure A never sees the seed but through camera B's loop.
        assert_feed(all_the_way.shows(Screen::UpperA), [1.0, 0.0, 0.0], 0.0);
        // D at In1 puts camera 3 where the seed was.
        let rotating_instead = Rig {
            switchers: [0.0, 1.0, 1.0, 0.0],
            ..all_the_way
        };
        assert_feed(rotating_instead.shows(Screen::UpperB), [0.0, 0.0, 1.0], 0.0);
    }

    #[test]
    fn the_rotating_monitor_shows_camera_b_whatever_the_setting() {
        for rig in [
            Rig::PERFORMANCE,
            Rig {
                switchers: [1.0; 4],
                selects: [In::One; 4],
            },
        ] {
            assert_feed(rig.shows(Screen::Rotating), [0.0, 1.0, 0.0], 0.0);
        }
    }

    #[test]
    fn a_b_monitor_on_in2_is_the_chain_multiplied_out() {
        let [a, b, c, d] = [0.3, 0.4, 0.6, 0.2];
        let rig = Rig {
            switchers: [a, b, c, d],
            selects: [In::Two; 4],
        };
        assert_feed(
            rig.shows(Screen::UpperB),
            [b * (1.0 - c), 1.0 - b, b * c * (1.0 - d)],
            b * c * d,
        );
        assert_feed(rig.shows(Screen::UpperA), [1.0 - a, a, 0.0], 0.0);
    }

    #[test]
    fn every_feed_sums_to_one() {
        // The rig never amplifies: at any setting, on every monitor, the
        // shares of the cameras and the seed are the whole picture.
        let settings = [0.0, 0.25, 0.5, 0.9, 1.0];
        for &a in &settings {
            for &b in &settings {
                for &c in &settings {
                    for &d in &settings {
                        for select in [In::One, In::Two] {
                            let rig = Rig {
                                switchers: [a, b, c, d],
                                selects: [select; 4],
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
    fn the_flattened_graph_is_the_rig_row_for_row() {
        let rig = Rig::PERFORMANCE;
        let params = rig.params();
        for (m, screen) in Screen::ALL.into_iter().enumerate() {
            let feed = rig.shows(screen);
            assert_eq!(params.routing[m], feed.cameras.to_vec(), "{screen:?}");
            assert!(close(params.routing_inputs[0][m], feed.seed), "{screen:?}");
        }
        // The optics: each structure's camera sees its two monitors at half
        // each through the glass, the third sees the rotating one alone.
        assert_eq!(params.cameras[0].look, [0.5, 0.5, 0.0, 0.0, 0.0]);
        assert_eq!(params.cameras[1].look, [0.0, 0.0, 0.5, 0.5, 0.0]);
        assert_eq!(params.cameras[2].look, [0.0, 0.0, 0.0, 0.0, 1.0]);
        // And the seed at the trickle the doc comment promises.
        assert!(close(
            params.routing_inputs[0][Screen::UpperB as usize],
            0.0125
        ));
    }
}
