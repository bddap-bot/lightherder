//! The instrument's graph, and everything the GPU side assumes about it.

use crate::feedback::MAX_TAPS;
use crate::midi::ROW_BUTTONS;
use crate::params::{Camera, Focus, Knob, Node, Params, Seed, Side};
use crate::rig::{self, Rig};

/// The most of `node` there is — the rig's own counts, which is how far the
/// surface's vocabulary of selects runs.
pub const fn cap(node: Node) -> usize {
    match node {
        Node::Camera => rig::CAMERAS,
        Node::Monitor => rig::MONITORS,
        Node::Switcher => rig::SWITCHERS,
    }
}

const _: () = assert!(
    rig::CAMERAS <= ROW_BUTTONS && rig::MONITORS <= ROW_BUTTONS && rig::SWITCHERS <= ROW_BUTTONS,
    "a count past the select row would name selects no button can carry"
);

/// The instrument: Blair's rig at its performance setting — see [`Rig`].
/// There is one, and nothing chooses it.
pub fn instrument() -> Params {
    Rig::PERFORMANCE.params()
}

/// Every focus at which a knob on `side` names a value of its own.
///
/// Only the indices the side reads: a camera knob is one value per camera
/// however many monitors there are, so walking whole focuses and dropping
/// the ones a knob does not distinguish would be the same checks and five
/// times the loop, once a frame. The rest stay at zero, since a knob on any
/// other side never reads them.
fn focuses(side: Side, params: &Params) -> impl Iterator<Item = Focus> {
    let count = |node| match side.reads(node) {
        true => params.count(node),
        false => 1,
    };
    let (cameras, monitors, switchers) = (
        count(Node::Camera),
        count(Node::Monitor),
        count(Node::Switcher),
    );
    (0..cameras).flat_map(move |camera| {
        (0..monitors).flat_map(move |monitor| {
            (0..switchers).map(move |switcher| Focus {
                camera,
                monitor,
                switcher,
            })
        })
    })
}

/// Everything the GPU side assumes about the graph, re-asserted by
/// `Feedback::step` every frame — so the success path must stay
/// allocation-free.
///
/// Every value a knob turns is checked against that knob's own [`Knob::limit`]
/// and nowhere else. A range check is also the finiteness check, since
/// neither a NaN nor an infinity is inside any range — and finiteness is not
/// optional here: a NaN written into a loop that feeds itself never leaves,
/// because `clamp` passes it through and Reset restores the same poisoned
/// initial.
pub fn validate(params: &Params) -> Result<(), String> {
    let (m, c) = (params.monitors.len(), params.cameras.len());
    if (m, c) != (rig::MONITORS, rig::CAMERAS) {
        return Err(format!(
            "{c} cameras and {m} monitors; the rig is {} on {}",
            rig::CAMERAS,
            rig::MONITORS
        ));
    }
    // A splitter is not a knob — nothing on the panel turns one — so this is
    // the only place its weights are decided, and they are checked as
    // written rather than against a rail no control could hit. Monitors only:
    // a camera watches the light going round and never the light coming in.
    for (i, camera) in params.cameras.iter().enumerate() {
        if camera.look.len() != m {
            return Err(format!(
                "camera {i}'s look has {} entries; needs one per monitor, {m}",
                camera.look.len(),
            ));
        }
        if let Some(w) = camera.look.iter().find(|w| !w.is_finite() || **w < 0.0) {
            return Err(format!(
                "camera {i}'s look contains {w}; weights are finite and >= 0"
            ));
        }
    }
    // A blob's brightness is not a knob either, so this is the only place its
    // level is decided. Zero is refused rather than loaded: a blob putting no
    // light on the glass is what `dark` says, and two spellings of one rig is
    // the ambiguity the union exists to delete.
    for (i, monitor) in params.monitors.iter().enumerate() {
        if let Seed::WhiteBlob(brightness) = monitor.seed {
            if !(brightness > 0.0 && brightness <= Seed::BRIGHTEST) {
                return Err(format!(
                    "monitor {i}'s white blob is {brightness}; it runs above 0 \
                     to {}, and a blob of no light is \"dark\"",
                    Seed::BRIGHTEST
                ));
            }
        }
    }
    // Every knob, at every focus that names a value of its own, against the
    // one definition of its travel. This is the whole of the per-value
    // checking: a rail spelled a second time here is a rail the two could
    // differ on.
    for knob in Knob::ALL.into_iter().filter(|knob| knob.owns_a_field()) {
        let (low, high) = knob.limit(params).ends();
        for focus in focuses(knob.side(), params) {
            let value = params.knob(knob, focus);
            if !(low..=high).contains(&value) {
                // Built only on the way out, so the frame-by-frame
                // re-assertion above stays allocation-free.
                let (name, node) = (knob.name(), focus);
                let what = match knob.side() {
                    Side::Camera => format!("camera {}'s {name}", node.camera),
                    Side::Monitor => format!("monitor {}'s {name}", node.monitor),
                    Side::Switcher => format!("switcher {}'s {name}", node.switcher),
                };
                return Err(format!("{what} is {value}; it runs {low} to {high}"));
            }
        }
    }
    // Bought in bank rather than in taps: a frame of reach is a copy of
    // every monitor, and the ring is sized from it at load. A camera past
    // the reach is caught by the walk over the knobs above, since the reach
    // is the delay knob's rail.
    if params.delay > Params::MAX_DELAY {
        return Err(format!(
            "the delay units reach {} frames; at most {}",
            params.delay,
            Params::MAX_DELAY
        ));
    }
    // Not a knob either, so the walk above does not see it: a divider of
    // zero is a path that never hands on a frame, and one past the slowest
    // is bank bought for a rate the original never ran at.
    for (i, camera) in params.cameras.iter().enumerate() {
        if !(1..=Camera::MAX_DIVIDER).contains(&camera.divider) {
            return Err(format!(
                "camera {i}'s divider is {}; it runs 1 to {}",
                camera.divider,
                Camera::MAX_DIVIDER
            ));
        }
    }
    // The flattened routing-times-look products are what the shader iterates,
    // and its uniform array is a fixed size. Bounded against every crossfade
    // wherever it can stand, not against where it happens to be: a switcher
    // sweeps mid-performance, so a feed of zeroes now is no promise about the
    // tap count a second later.
    let reachable = crate::feedback::reachable_taps(params);
    if reachable > MAX_TAPS {
        return Err(format!(
            "a monitor here could be fed by {reachable} taps once the switchers \
             are turned up; at most {MAX_TAPS}"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::Knob;

    #[test]
    fn the_instrument_validates() {
        validate(&instrument()).unwrap();
    }

    #[test]
    fn the_instrument_is_contracting() {
        // At the setting it plays, every loop is near unity: a trail that
        // decays in a few passes is not worth seeing.
        let played = instrument();
        for m in 0..played.monitors.len() {
            let sum: f32 = (0..played.cameras.len())
                .map(|c| {
                    let cam = &played.cameras[c];
                    played.route(m, c) * cam.gain[1] * cam.look.iter().sum::<f32>()
                })
                .sum();
            assert!(sum > 0.9, "monitor {m}: gain sum {sum} dies fast");
        }
        // The light monitor `i` shows next frame is at most `sum` times the
        // brightest thing on any monitor this frame, so `sum < 1` means it
        // settles instead of blooming to white. Near 1, or the trail is not
        // worth seeing. The seed is left out: it is light entering the graph,
        // so it belongs to what the loop is driven *by*, not to what it
        // multiplies.
        //
        // At every setting of the switchers, not only the one it starts on:
        // a crossfade is a fader, and a rig that blooms at the top of one is
        // a rig that blooms.
        let positions = [0.0, 0.25, 0.5, 0.9, 1.0];
        for &a in &positions {
            for &b in &positions {
                for bits in 0..16u8 {
                    let mut params = instrument();
                    params.rig.switchers = [a, b, a, b];
                    params.rig.selects = std::array::from_fn(|i| match bits >> i & 1 {
                        0 => crate::rig::Select::Direct,
                        _ => crate::rig::Select::Program,
                    });
                    for m in 0..params.monitors.len() {
                        let sum = (0..3)
                            .map(|ch| {
                                (0..params.cameras.len())
                                    .map(|c| {
                                        let cam = &params.cameras[c];
                                        let round: f32 = cam.look.iter().sum();
                                        params.route(m, c) * cam.gain[ch] * round
                                    })
                                    .sum::<f32>()
                            })
                            .fold(0.0, f32::max);
                        assert!(sum < 1.0, "monitor {m}: gain sum {sum} blooms");
                    }
                }
            }
        }
    }

    #[test]
    fn a_misshapen_graph_is_refused() {
        let mut wrong_look = instrument();
        wrong_look.cameras[0].look.pop();
        assert!(validate(&wrong_look).is_err());

        let mut short = instrument();
        short.cameras.pop();
        assert!(validate(&short).is_err());

        let mut empty = instrument();
        empty.monitors.clear();
        assert!(validate(&empty).is_err());
    }

    #[test]
    fn a_graph_with_a_poisoned_number_is_refused() {
        // A NaN inside a loop that feeds itself never leaves: the knobs clamp
        // with `clamp`, which passes NaN through, and Reset restores the same
        // initial. One of each kind of number the graph holds — a knob on a
        // camera, on a monitor, on a switcher, and a splitter weight, which
        // is the one that is not a knob and so the one case the rail walk in
        // `params::a_knob_past_its_rail_is_refused_rather_than_snapped_later`
        // does not reach.
        let poison: &[fn(&mut Params)] = &[
            |p| p.cameras[0].gain[0] = f32::NAN,
            |p| p.cameras[0].framing.rotation = f32::INFINITY,
            |p| p.cameras[0].character.bloom = f32::NAN,
            |p| p.cameras[0].key.hue = f32::INFINITY,
            |p| p.monitors[0].colour.gamma = f32::NAN,
            |p| p.monitors[0].headroom = f32::NAN,
            |p| p.rig.switchers[2] = f32::INFINITY,
            |p| p.cameras[0].look[0] = f32::NAN,
        ];
        for (i, poison) in poison.iter().enumerate() {
            let mut params = instrument();
            poison(&mut params);
            assert!(validate(&params).is_err(), "poison {i} passed");
        }
    }

    #[test]
    fn a_white_blob_is_refused_outside_the_light_it_can_be() {
        let with = |seed| {
            let mut p = instrument();
            p.monitors[0].seed = seed;
            p
        };
        validate(&with(Seed::BLOB)).unwrap();
        validate(&with(Seed::Dark)).unwrap();
        for bad in [0.0, -0.1, Seed::BRIGHTEST + 0.1, f32::NAN] {
            let why = validate(&with(Seed::WhiteBlob(bad))).unwrap_err();
            assert!(why.contains("white blob"), "{bad}: {why}");
        }
    }

    #[test]
    fn a_delay_past_the_units_reach_is_refused() {
        let with = |reach: u32, delay: u32| {
            let mut p = instrument();
            p.delay = reach;
            p.cameras[0].delay = delay;
            p
        };
        assert!(validate(&with(Params::MAX_DELAY, Params::MAX_DELAY)).is_ok());
        let why = validate(&with(Params::MAX_DELAY + 1, 0)).unwrap_err();
        assert!(why.contains("reach 31"), "{why}");
        assert_eq!(
            validate(&with(3, 4)).unwrap_err(),
            "camera 0's delay is 4; it runs 0 to 3"
        );
        // The ring the reach and a divided path's hold buy between them.
        let mut p = with(4, 4);
        p.cameras[1].divider = 2;
        assert_eq!(p.history(), 7);
    }

    #[test]
    fn a_divider_outside_one_to_the_slowest_is_refused() {
        let with = |divider: u32| {
            let mut p = instrument();
            p.cameras[0].divider = divider;
            p
        };
        assert!(validate(&with(Camera::MAX_DIVIDER)).is_ok());
        assert_eq!(
            validate(&with(0)).unwrap_err(),
            "camera 0's divider is 0; it runs 1 to 3"
        );
        assert_eq!(
            validate(&with(Camera::MAX_DIVIDER + 1)).unwrap_err(),
            "camera 0's divider is 4; it runs 1 to 3"
        );
        assert_eq!(instrument().history(), 4);
    }

    #[test]
    fn the_tap_bound_holds_at_every_setting_of_the_switchers() {
        // `taps_of` drops a feed at zero and a crossfade can be swept, so the
        // count validate holds against has to be the reachable one: what the
        // rig runs is at or under it, and every switcher turned up reaches
        // exactly it.
        let params = instrument();
        let reachable = crate::feedback::reachable_taps(&params);
        for m in 0..params.monitors.len() {
            let now = crate::feedback::taps_of(&params, m, 0, 0).count();
            assert!(now <= reachable, "monitor {m} has {now} of {reachable}");
            assert!(reachable <= MAX_TAPS, "{reachable} past {MAX_TAPS}");
        }
    }

    #[test]
    fn a_knob_the_rig_has_no_field_for_is_refused_a_reading() {
        // The delay is the one knob the graph can be without, and the map
        // refuses a binding of one that is off.
        let mut none = instrument();
        none.delay = 0;
        assert!(!Knob::Delay.is_on(&none));
        assert!(Knob::Delay.is_on(&instrument()));
        for knob in Knob::ALL.into_iter().filter(|k| *k != Knob::Delay) {
            assert!(knob.is_on(&none), "{knob:?}");
        }
    }
}
