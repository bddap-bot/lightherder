//! The instrument's graph, and everything the GPU side assumes about it.

use crate::params::{Focus, Knob, Node, Params};
use crate::rig::{self, Rig};

/// The instrument: Blair's rig at identity — see [`Rig`].
/// There is one, and nothing chooses it.
pub fn instrument() -> Params {
    Rig::IDENTITY.params()
}

/// Every focus at which a knob on `side`, or the rig's for none, names a
/// value of its own.
///
/// Only the indices the side reads: a camera knob is one value per camera
/// however many monitors there are, so walking whole focuses and dropping
/// the ones a knob does not distinguish would be the same checks and five
/// times the loop, once a frame. The rest stay at zero, since a knob on any
/// other side never reads them.
fn focuses(side: Option<Node>) -> impl Iterator<Item = Focus> {
    let count = |node| match side == Some(node) {
        true => rig::count(node),
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
    // A splitter is not a knob — nothing on the panel turns one — so this is
    // the only place its weights are decided, and they are checked as
    // written rather than against a rail no control could hit. Monitors only:
    // a camera watches the light going round and never the light coming in.
    for (i, camera) in params.cameras.iter().enumerate() {
        if let Some(g) = camera.gain.iter().find(|g| !g.is_finite() || **g < 0.0) {
            return Err(format!(
                "camera {i}'s gain contains {g}; gains are finite and >= 0"
            ));
        }
        if let Some(w) = camera.look.iter().find(|w| !w.is_finite() || **w < 0.0) {
            return Err(format!(
                "camera {i}'s look contains {w}; weights are finite and >= 0"
            ));
        }
    }
    // The switcher's key is fixed character rather than a knob, so this is
    // the only place its numbers are decided.
    let key = params.input.key;
    if !(key.threshold.is_finite() && key.softness.is_finite())
        || key.threshold < 0.0
        || key.softness < 0.0
    {
        return Err(format!(
            "the seed's key is {} over {}; both are finite and >= 0",
            key.threshold, key.softness
        ));
    }
    // Every knob, at every focus that names a value of its own, against the
    // one definition of its travel. This is the whole of the per-value
    // checking: a rail spelled a second time here is a rail the two could
    // differ on.
    for knob in Knob::ALL {
        let (low, high) = knob.limit(params).ends();
        for focus in focuses(knob.node()) {
            let value = params.knob(knob, focus);
            if !(low..=high).contains(&value) {
                // Built only on the way out, so the frame-by-frame
                // re-assertion above stays allocation-free.
                let (name, node) = (knob.name(), focus);
                let what = match knob.node() {
                    None => format!("the rig's {name}"),
                    Some(Node::Camera) => format!("camera {}'s {name}", node.camera),
                    Some(Node::Monitor) => format!("monitor {}'s {name}", node.monitor),
                    Some(Node::Switcher) => format!("switcher {}'s {name}", node.switcher),
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_instrument_validates() {
        validate(&instrument()).unwrap();
    }

    #[test]
    fn the_instrument_is_contracting() {
        // The light monitor `i` shows next frame is at most `sum` times the
        // brightest thing on any monitor this frame, so `sum < 1` means it
        // settles instead of blooming to white. The seed is left out: it is
        // light entering the graph, so it belongs to what the loop is driven
        // *by*, not to what it multiplies. Where its key cuts, the cameras it
        // was keyed over stand at their larger share, so that is the share
        // measured.
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
                        let feed = params.rig.feed(m);
                        let sum = (0..3)
                            .map(|ch| {
                                (0..params.cameras.len())
                                    .map(|c| {
                                        let cam = &params.cameras[c];
                                        let round: f32 = cam.look.iter().sum();
                                        feed.cut(c) * cam.gain[ch] * round
                                    })
                                    .sum::<f32>()
                            })
                            .fold(0.0, f32::max);
                        assert!(sum < 1.0, "monitor {m}: gain sum {sum} blooms");
                        if a == 0.0 && b == 0.0 {
                            assert!(sum > 0.9, "monitor {m}: gain sum {sum} dies fast");
                        }
                    }
                }
            }
        }
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
            |p| p.framing.rotation = f32::INFINITY,
            |p| p.monitors[0].colour.saturation = f32::NAN,
            |p| p.input.key.threshold = f32::NAN,
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
        // The ring the reach buys: the frame being drawn, the one every
        // camera reads, and one more per frame of reach.
        assert_eq!(with(4, 4).history(), 6);
    }
}
