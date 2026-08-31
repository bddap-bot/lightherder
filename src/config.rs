//! The graphs the instrument ships with, and loading arbitrary ones from
//! disk. A preset is nothing but a [`Params`] value; a config file is the
//! same struct in TOML, so anything a preset can express a file can too.

use crate::affine::Framing;
use crate::feedback::MAX_TAPS;
use crate::input::{Input, Pattern};
use crate::params::{Camera, Character, Focus, Key, Knob, Monitor, Params, Seed, Side};

/// More monitors than this and the uniform buffer, the present grid and the
/// texture array all need a second look; fewer keeps every one of them dumb.
pub const MAX_MONITORS: usize = 8;

/// One camera per key, by definition rather than by assertion so the tie
/// cannot drift: a ninth camera has no key to bring the focus to it, so it
/// would play forever at whatever the file left its knobs on. Monitors make
/// the same promise, so their independently-motivated cap must stay inside
/// the keys too.
pub const MAX_CAMERAS: usize = crate::keys::KEYED_NODES;
const _: () = assert!(MAX_MONITORS <= crate::keys::KEYED_NODES);

/// Inputs get their own cap because what [`MAX_TAPS`] bounds is not what
/// they cost: one tap each, against a bank layer each plus — for a file or a
/// device — a process and a thread of its own. Four is what a switcher has
/// spare inputs for.
pub const MAX_INPUTS: usize = 4;

/// The classic rig: one camera aimed straight at the one monitor it draws to.
/// The values are the bootstrap stage's defaults — the camera pulls back a
/// little and turns a little each pass, at a gain just under unity spread
/// across the channels, so the seed leaves a spiral that cools from white to
/// blue as it winds in.
pub fn single() -> Params {
    Params {
        cameras: vec![Camera {
            framing: Framing {
                zoom: 0.994,
                rotation: 0.05,
                translate: [0.0, 0.0],
            },
            gain: [0.980, 0.986, 0.992],
            character: Character::CLEAN,
            key: Key::OFF,
            look: vec![1.0],
        }],
        monitors: vec![Monitor {
            seed: Seed::BLOB,
            ..Default::default()
        }],
        inputs: Vec::new(),
        routing: vec![vec![1.0]],
        routing_inputs: Vec::new(),
    }
}

/// The same single loop with the signal path turned on: a lens that scatters
/// a third of the light into a halo, chroma smeared along the scanline, a
/// little grain, and an amplifier whose rail sits below white so the trail
/// compresses into the spiral instead of burning out the middle of it.
/// Everything else is [`single`]'s, so the difference between the two presets
/// is the analog character and nothing else — the chroma the bleed smears is
/// the same age-to-hue gradient the per-channel loop gain already makes.
pub fn analog() -> Params {
    let mut params = single();
    params.cameras[0].character = Character {
        bloom: 0.35,
        bloom_radius: 0.05,
        chroma_bleed: 0.02,
        noise: 0.02,
    };
    params.monitors[0].headroom = 0.9;
    params
}

/// The Light Herder's crossed two-structure setup: each camera looks at its
/// own monitor through beam-splitter glass that lets a quarter of the other
/// one bleed in, and the switcher routes each camera to the *opposite*
/// monitor, so every image is made of its twin's past. Both framings turn
/// the same way on purpose: opposite spins would cancel over a round trip,
/// and a trail that never winds away from the seed piles up on it until the
/// display clips. The unequal rates keep the two structures distinct.
pub fn crossed() -> Params {
    let camera = |looks_at: usize, rotation: f32, gain: [f32; 3]| {
        let mut look = vec![0.25; 2];
        look[looks_at] = 0.75;
        Camera {
            framing: Framing {
                zoom: 0.994,
                rotation,
                translate: [0.0, 0.0],
            },
            gain,
            character: Character::CLEAN,
            key: Key::OFF,
            look,
        }
    };
    Params {
        cameras: vec![
            camera(0, 0.05, [0.980, 0.986, 0.992]),
            camera(1, 0.08, [0.992, 0.986, 0.980]),
        ],
        monitors: vec![
            Monitor {
                seed: Seed::BLOB,
                ..Default::default()
            },
            Monitor {
                seed: Seed::BLOB,
                ..Default::default()
            },
        ],
        inputs: Vec::new(),
        routing: vec![vec![0.0, 1.0], vec![1.0, 0.0]],
        routing_inputs: Vec::new(),
    }
}

/// Insanity mode: every monitor is composed of every other. Four cameras,
/// one per monitor, and an all-to-all routing matrix whose rows sum to one.
/// The framings differ per camera — staggered spin and zoom — so the four
/// contributions to any one monitor never line up. All the spins go the same
/// way for the same reason as [`crossed`]: mixed-sign rotations hand the mix
/// closed paths that never wind away from the seed, and those clip.
pub fn insanity() -> Params {
    const N: usize = 4;
    let cameras = (0..N)
        .map(|c| {
            let mut look = vec![0.0; N];
            look[c] = 1.0;
            Camera {
                framing: Framing {
                    zoom: 0.990 + 0.004 * c as f32,
                    rotation: 0.04 + 0.02 * c as f32,
                    translate: [0.0, 0.0],
                },
                // Each camera favours a different channel, so the mix on any
                // monitor carries chroma for the hue knobs to turn.
                gain: std::array::from_fn(|ch| if ch == c % 3 { 0.992 } else { 0.976 }),
                character: Character::CLEAN,
                key: Key::OFF,
                look,
            }
        })
        .collect();
    Params {
        cameras,
        monitors: (0..N)
            .map(|_| Monitor {
                seed: Seed::BLOB,
                ..Default::default()
            })
            .collect(),
        inputs: Vec::new(),
        routing: vec![vec![1.0 / N as f32; N]; N],
        routing_inputs: Vec::new(),
    }
}

/// A test pattern driving the loop instead of the seed spot. One camera, the
/// classic rig turning and pulling back on its own monitor, and the bars
/// plugged into the switcher beside it on a crosspoint that hands over
/// almost nothing — a seventieth of the picture, 0.014. That is the whole
/// point of a loop this close to unity: what settles is the trickle divided
/// by how far the gain is from 1, so 0.014 of the bars over a loop 0.015
/// short of unity settles at almost the bars' own brightness. The glass is
/// dark, so every photon here came in from outside, and the gain is flat
/// across the channels because an input supplies its own colour — there is
/// nothing for a per-channel decay to add.
pub fn external() -> Params {
    Params {
        cameras: vec![Camera {
            framing: Framing {
                zoom: 0.994,
                rotation: 0.05,
                translate: [0.0, 0.0],
            },
            gain: [0.985; 3],
            character: Character::CLEAN,
            key: Key::OFF,
            look: vec![1.0],
        }],
        monitors: vec![Monitor::default()],
        inputs: vec![Input::Pattern(Pattern::Bars)],
        // The loop camera at a full send.
        routing: vec![vec![1.0]],
        // The bars at a trickle. The pattern arrives square on and whole,
        // since nothing frames what the switcher hands over — everything
        // that happens to it afterwards is the loop's doing.
        routing_inputs: vec![vec![0.014]],
    }
}

///
/// Two monitors, because a key belongs to a camera and a camera watches
/// monitors. The switcher puts the device whole on the first — a monitor
/// used as the room's window, with nothing routed to it and no loop of its
/// own — and a camera watches *that* through its luma key, handing on the
/// subject and refusing the dark room behind it. What it hands on drives the
/// second monitor's loop, which is [`external`]'s: the injection divided by
/// the loop's distance from unity, and at 0.015 into a loop 0.015 short of
/// unity a subject settles at its own brightness — full scale in, white
/// trail out, with the amplifier's rail above it for the moments a light
/// swings past.
///
/// The key is where the two-monitor shape buys something a one-monitor rig
/// cannot have: a key on the loop's own camera would gate the trail it is
/// building, so it would never build one.
///
/// The window costs the room one frame on its way in, since a camera reads
/// the frame a monitor held rather than the one it is being handed. A
/// sixtieth of a second behind a live subject is under the hand's own
/// latency, and the loop's own delay is a whole pass anyway.
pub fn webcam() -> Params {
    let window = 0;
    let loop_monitor = 1;
    let looking_at_the_room = Camera {
        // Square on, so what the key passes is the picture the device sent.
        framing: Framing::identity(),
        gain: [0.015; 3],
        character: Character::CLEAN,
        // Passing from mid-grey up and cutting to nothing a little below it:
        // a subject lit against an unlit room, which is what a bare webcam
        // faces. The luma half only — a dark room, not a coloured sheet.
        key: Key {
            threshold: 0.35,
            softness: 0.08,
            ..Key::OFF
        },
        look: one_hot(2, window),
    };
    let mut looking_at_the_loop = external().cameras.remove(0);
    looking_at_the_loop.look = one_hot(2, loop_monitor);
    Params {
        cameras: vec![looking_at_the_room, looking_at_the_loop],
        monitors: vec![Monitor::default(); 2],
        inputs: vec![Input::Capture {
            format: "v4l2".into(),
            device: "/dev/video0".into(),
        }],
        // The keyed camera and the loop camera both onto the loop's monitor,
        // and nothing onto the window: a camera routed there would put the
        // loop back in front of the lens that is watching the room.
        routing: vec![vec![0.0, 0.0], vec![1.0, 1.0]],
        // The device onto the window, whole. Its level is the keyed camera's
        // gain, one stage further on, so this end of it stays at full.
        routing_inputs: vec![vec![1.0, 0.0]],
    }
}

/// A weight per monitor with one of them full — a camera aimed straight at
/// the one it watches.
fn one_hot(monitors: usize, at: usize) -> Vec<f32> {
    let mut look = vec![0.0; monitors];
    look[at] = 1.0;
    look
}

/// A preset: the name the command line knows it by, and the graph it builds.
pub type Preset = (&'static str, fn() -> Params);

/// The presets, by the names the command line and the error messages use.
pub const PRESETS: [Preset; 6] = [
    ("single", single as fn() -> Params),
    ("analog", analog),
    ("crossed", crossed),
    ("insanity", insanity),
    ("external", external),
    ("webcam", webcam),
];

/// `arg` is a preset name or a path to a TOML file of [`Params`]. Either way
/// the result is validated, so the GPU side can trust its shape.
pub fn load(arg: &str) -> Result<Params, String> {
    match PRESETS.iter().find(|(name, _)| *name == arg) {
        Some((_, build)) => {
            let params = build();
            validate(&params)?;
            Ok(params)
        }
        None => read(std::path::Path::new(arg)).map_err(|e| {
            let names: Vec<&str> = PRESETS.iter().map(|(name, _)| *name).collect();
            format!("{e} (presets: {})", names.join(", "))
        }),
    }
}

fn read(path: &std::path::Path) -> Result<Params, String> {
    let shown = path.display();
    let text = std::fs::read_to_string(path).map_err(|e| format!("{shown}: {e}"))?;
    let params: Params = toml::from_str(&text).map_err(|e| format!("{shown}: {e}"))?;
    validate(&params).map_err(|e| format!("{shown}: {e}"))?;
    Ok(params)
}

/// Every focus at which a knob on `side` names a value of its own.
///
/// Only the indices the side reads: a camera knob is one value per camera
/// however many monitors there are, so walking whole focuses and dropping
/// the ones a knob does not distinguish would be the same checks and eight
/// times the loop, once a frame, on a camera count nothing caps. The rest
/// stay at zero — including the input index on a graph with no inputs, since
/// a knob on any other side never reads it.
///
/// This is what lets `validate` check the switcher's two halves in one walk
/// over one rail each: they are different pairs of indices, not a matrix and
/// an exception to it.
fn focuses(side: Side, params: &Params) -> impl Iterator<Item = Focus> {
    let (cameras, monitors, inputs) = match side {
        Side::Camera => (params.cameras.len(), 1, 1),
        Side::Monitor => (1, params.monitors.len(), 1),
        Side::Edge => (params.cameras.len(), params.monitors.len(), 1),
        Side::InputEdge => (1, params.monitors.len(), params.inputs.len()),
    };
    (0..cameras).flat_map(move |camera| {
        (0..monitors).flat_map(move |monitor| {
            (0..inputs).map(move |input| Focus {
                camera,
                monitor,
                input,
            })
        })
    })
}

/// Everything the GPU side assumes about a graph, checked at load — and
/// re-asserted by `Feedback::step`, so the success path must stay
/// allocation-free.
///
/// Every value a knob turns is checked against that knob's own [`Knob::limit`]
/// and nowhere else. A file outside one is *refused* rather than loaded and
/// silently snapped by the first key press: a `headroom = 1e6` that validates
/// is a state the instrument shows, cannot return to, and hands a fader a
/// travel it is standing off the end of.
///
/// A phase is refused outside its turn for a weaker reason, since `rotation =
/// 6.5` is an angle the instrument can perfectly well hold — it is one turn
/// and a third, and a file that says so is far likelier to be counting in
/// degrees than to mean it. Refusing says which; wrapping it silently would
/// not, and would put the value somewhere the file did not name.
///
/// A range check is also the finiteness check, since neither a NaN nor an
/// infinity is inside any range — and finiteness is not optional here: a NaN
/// written into a loop that feeds itself never leaves, because `clamp` passes
/// it through and Reset restores the same poisoned initial.
pub fn validate(params: &Params) -> Result<(), String> {
    let (m, c, n) = (
        params.monitors.len(),
        params.cameras.len(),
        params.inputs.len(),
    );
    if !(1..=MAX_MONITORS).contains(&m) {
        return Err(format!("{m} monitors; needs between 1 and {MAX_MONITORS}"));
    }
    if c == 0 {
        return Err(
            "no cameras; a rig with none has no loop to be a feedback rig, and the \
             panel's camera knobs would have nothing to point at"
                .into(),
        );
    }
    if c > MAX_CAMERAS {
        return Err(format!(
            "{c} cameras; at most {MAX_CAMERAS} — one per focus key, and a camera \
             past the keys could never be turned live"
        ));
    }
    if n > MAX_INPUTS {
        return Err(format!("{n} inputs; at most {MAX_INPUTS}"));
    }
    // The switcher's shape, before anything reads a crosspoint out of it:
    // `Params::knob` indexes `routing[monitor][camera]` directly, so a short
    // row would panic rather than fail. Its two halves are counted against
    // their own kinds and transposed from each other — a row per monitor
    // over the cameras, a row per input over the monitors — which is why
    // this is two shape checks and not one.
    if params.routing.len() != m {
        return Err(format!(
            "routing has {} rows; needs one per monitor, {m}",
            params.routing.len()
        ));
    }
    for (i, row) in params.routing.iter().enumerate() {
        if row.len() != c {
            return Err(format!(
                "routing row {i} has {} entries; needs one per camera, {c}",
                row.len()
            ));
        }
    }
    if params.routing_inputs.len() != n {
        return Err(format!(
            "routing_inputs has {} rows; needs one per input, {n}",
            params.routing_inputs.len()
        ));
    }
    for (i, row) in params.routing_inputs.iter().enumerate() {
        if row.len() != m {
            return Err(format!(
                "routing_inputs row {i} has {} entries; needs one per monitor, {m}",
                row.len()
            ));
        }
    }
    // A splitter is not a knob — nothing on the panel turns one — so this is
    // the only place its weights are decided, and they are checked as
    // written rather than against a rail no key could hit. Monitors only:
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
    // A blob's brightness is not a knob either — nothing on the panel turns
    // one — so this is the only place its level is decided. Zero is refused
    // rather than loaded: a blob putting no light on the glass is what
    // `dark` says, and two spellings of one rig is the ambiguity the union
    // exists to delete — one the surface's lamp would then get wrong.
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
    // differ on, which is how a config used to load a bloom the bloom knob
    // could not reach.
    for knob in Knob::ALL.into_iter().filter(|knob| knob.owns_a_field()) {
        let (low, high) = knob.limit().ends();
        for focus in focuses(knob.side(), params) {
            let value = params.knob(knob, focus);
            if !(low..=high).contains(&value) {
                // Built only on the way out, so the frame-by-frame
                // re-assertion above stays allocation-free.
                let (name, node) = (knob.name(), focus);
                let what = match knob.side() {
                    Side::Camera => format!("camera {}'s {name}", node.camera),
                    Side::Monitor => format!("monitor {}'s {name}", node.monitor),
                    Side::Edge => {
                        format!(
                            "camera {}'s {name} to monitor {}",
                            node.camera, node.monitor
                        )
                    }
                    Side::InputEdge => {
                        format!("input {}'s {name} to monitor {}", node.input, node.monitor)
                    }
                };
                return Err(format!("{what} is {value}; it runs {low} to {high}"));
            }
        }
    }
    // The flattened routing-times-look products are what the shader iterates,
    // and its uniform array is a fixed size. Bounded against every crosspoint
    // turned up, not against what the file happens to say: `Knob::Route`
    // sweeps one mid-performance, so a row of zeroes at load is no promise
    // about the tap count a second later. The look weights are not a
    // crosspoint, so they still count as written.
    let reachable = crate::feedback::reachable_taps(params);
    if reachable > MAX_TAPS {
        return Err(format!(
            "a monitor here could be fed by {reachable} taps once the switcher \
             is turned up; at most {MAX_TAPS}"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::Colour;

    /// Off [`PRESETS`] rather than listed again, so a preset added without a
    /// line here cannot slip past every test in this file.
    fn presets() -> Vec<(&'static str, Params)> {
        PRESETS
            .iter()
            .map(|(name, build)| (*name, build()))
            .collect()
    }

    #[test]
    fn every_preset_validates_and_loads_by_name() {
        for (name, params) in presets() {
            validate(&params).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(load(name).unwrap(), params);
        }
    }

    #[test]
    fn every_preset_is_contracting() {
        // The light monitor `i` shows next frame is at most `sum` times the
        // brightest thing on any monitor this frame, so `sum < 1` means every
        // preset settles instead of blooming to white. Near 1, or the trail
        // is not worth seeing.
        //
        // `routing` and not `routing_inputs`: an input is light entering the
        // graph, so it belongs to what the loop is driven *by*, not to what
        // it multiplies — the seed is left out of this sum for exactly the
        // same reason. Reading the loop's own gain is naming one of the two
        // halves of the switcher, which is the whole of the argument for
        // their being two.
        for (name, params) in presets() {
            // A monitor no camera is routed to is a window and not a loop:
            // its light arrives from outside every frame at the size it
            // arrived last frame. So a camera watching one is carrying light
            // *in*, and its weight on that monitor is injection rather than
            // gain — `webcam` keys a room off such a window into a loop, and
            // counting the two together would read its loop as exactly unity
            // when the part that goes round is 0.985.
            let is_loop: Vec<bool> = params
                .routing
                .iter()
                .map(|row| row.iter().any(|route| *route > 0.0))
                .collect();
            for (i, row) in params.routing.iter().enumerate() {
                let sum: f32 = (0..3)
                    .map(|ch| {
                        row.iter()
                            .zip(&params.cameras)
                            .map(|(route, cam)| {
                                let round: f32 = cam
                                    .look
                                    .iter()
                                    .zip(&is_loop)
                                    .filter(|(_, feeds_back)| **feeds_back)
                                    .map(|(look, _)| look)
                                    .sum();
                                route * cam.gain[ch] * round
                            })
                            .sum()
                    })
                    .fold(0.0, f32::max);
                assert!(sum < 1.0, "{name} monitor {i}: gain sum {sum} blooms");
                // The lower rail is asked only where there is a loop to ask
                // it of: a window has no gain to be near unity.
                if is_loop[i] {
                    assert!(sum > 0.9, "{name} monitor {i}: gain sum {sum} dies fast");
                }
            }
        }
    }

    #[test]
    fn the_crossed_preset_really_crosses() {
        let p = crossed();
        // Each monitor shows only the other's camera…
        assert_eq!(p.routing[0][0], 0.0);
        assert_eq!(p.routing[1][1], 0.0);
        assert!(p.routing[0][1] > 0.0 && p.routing[1][0] > 0.0);
        // …and each camera sees both monitors through the splitter.
        for camera in &p.cameras {
            assert!(camera.look.iter().all(|w| *w > 0.0));
        }
    }

    #[test]
    fn insanity_composes_every_monitor_of_every_camera() {
        let p = insanity();
        for row in &p.routing {
            assert!(
                row.iter().all(|w| *w > 0.0),
                "a camera is left out: {row:?}"
            );
        }
    }

    #[test]
    fn a_seed_is_written_in_a_file_the_way_the_readme_spells_it() {
        // The union's two shapes as a hand writes them, which is the one
        // thing a serde round trip of the crate's own output cannot check.
        let params: Params = toml::from_str(
            "cameras = [{ look = [1.0] }]\n\
             monitors = [{ seed = { white_blob = 0.1 } }, { seed = \"dark\" }]\n\
             routing = [[1.0], [1.0]]\n",
        )
        .unwrap();
        assert_eq!(params.monitors[0].seed, Seed::WhiteBlob(0.1));
        assert_eq!(params.monitors[1].seed, Seed::Dark);
    }

    #[test]
    fn a_terse_config_file_gets_the_documented_defaults() {
        // The smallest useful file: one static camera, one silent monitor.
        // Framing, gain and colour all fall to their identity defaults.
        let params: Params = toml::from_str(
            "cameras = [{ look = [1.0] }]\n\
             monitors = [{}]\n\
             routing = [[1.0]]\n",
        )
        .unwrap();
        validate(&params).unwrap();
        assert_eq!(params.cameras[0].framing, Framing::identity());
        assert_eq!(params.cameras[0].gain, [1.0; 3]);
        assert_eq!(params.cameras[0].character, Character::CLEAN);
        assert_eq!(params.monitors[0].colour, Colour::NEUTRAL);
        assert_eq!(params.monitors[0].seed, Seed::Dark);
        assert_eq!(params.monitors[0].headroom, Monitor::KNEE_AT_WHITE);
    }

    #[test]
    fn only_the_analog_preset_has_any_character() {
        // The stage is additive: it must not have quietly changed the look of
        // the presets that were here before it.
        for (name, params) in presets() {
            let clean = params
                .cameras
                .iter()
                .all(|c| c.character == Character::CLEAN);
            assert_eq!(clean, name != "analog", "{name}");
            let open_rail = params
                .monitors
                .iter()
                .all(|m| m.headroom == Monitor::KNEE_AT_WHITE);
            assert_eq!(open_rail, name != "analog", "{name}'s rail");
        }
        // And the one that does have it turns on all four of the things this
        // stage is named for.
        let a = analog();
        let ch = a.cameras[0].character;
        assert!(ch.bloom > 0.0 && ch.bloom_radius > 0.0);
        assert!(ch.chroma_bleed > 0.0 && ch.noise > 0.0);
        assert!(a.monitors[0].headroom < 1.0, "the rail never bites");
    }

    #[test]
    fn a_misshapen_graph_is_refused() {
        let mut wrong_look = crossed();
        wrong_look.cameras[0].look.pop();
        assert!(validate(&wrong_look).is_err());

        let mut wrong_row = crossed();
        wrong_row.routing[1].push(0.5);
        assert!(validate(&wrong_row).is_err());

        let mut empty = crossed();
        empty.monitors.clear();
        assert!(validate(&empty).is_err());

        assert!(load("no-such-preset").is_err());
    }

    #[test]
    fn a_graph_with_a_poisoned_number_is_refused() {
        // TOML accepts `nan` and `inf` literals, and a NaN inside a loop
        // that feeds itself never leaves: the knobs clamp with `clamp`,
        // which passes NaN through, and Reset restores the same initial.
        // Refusing at load is the only door.
        //
        // One of each kind of number a file carries — a knob on a camera, on
        // a monitor and on a crosspoint, and a splitter weight, which is the
        // one that is not a knob and so is the one case the rail walk in
        // `params::a_knob_past_its_rail_is_refused_rather_than_snapped_later`
        // does not reach. The out-of-range poisons that used to be listed
        // here are that walk's, over every knob rather than the few that
        // happened to be written down.
        let poison: &[fn(&mut Params)] = &[
            |p| p.cameras[0].gain[0] = f32::NAN,
            |p| p.cameras[0].framing.rotation = f32::INFINITY,
            |p| p.cameras[0].character.bloom = f32::NAN,
            |p| p.cameras[0].key.hue = f32::INFINITY,
            |p| p.monitors[0].colour.gamma = f32::NAN,
            |p| p.monitors[0].headroom = f32::NAN,
            |p| p.routing[0][1] = f32::INFINITY,
            |p| p.cameras[0].look[0] = f32::NAN,
        ];
        for (i, poison) in poison.iter().enumerate() {
            let mut params = crossed();
            poison(&mut params);
            assert!(validate(&params).is_err(), "poison {i} passed");
        }
    }

    /// A graph carrying one of every kind of input, the switcher's input
    /// half widened to match: `external` plus a file and a device.
    fn one_of_every_input() -> Params {
        let mut params = external();
        params.inputs = vec![
            Input::Pattern(Pattern::Bars),
            Input::File("clip.mp4".into()),
            Input::Capture {
                format: "v4l2".into(),
                device: "/dev/video0".into(),
            },
        ];
        params.routing_inputs.resize(3, vec![0.0]);
        params
    }

    #[test]
    fn every_kind_of_input_spells_itself_the_way_it_is_documented() {
        // The literal keys, not a round trip: a round trip agrees with itself
        // whatever serde is told to call these, and every config file and
        // every line of the README that mentions an input depends on the
        // names rather than on the agreement.
        let params: Params = toml::from_str(
            "cameras = [{ look = [1.0] }]\n\
             monitors = [{}]\n\
             routing = [[1.0]]\n\
             routing_inputs = [[0.0], [0.0], [0.0]]\n\
             inputs = [\n\
             \x20 { pattern = \"bars\" },\n\
             \x20 { file = \"clip.mp4\" },\n\
             \x20 { capture = { format = \"v4l2\", device = \"/dev/video0\" } },\n\
             ]\n",
        )
        .unwrap();
        validate(&params).unwrap();
        assert_eq!(params.inputs, one_of_every_input().inputs);
    }

    #[test]
    fn a_weight_is_counted_against_its_own_kind_of_thing() {
        // A camera added to a working graph, and nothing else touched. Were
        // the inputs columns of `routing` past the cameras', a hand that
        // then made the row the length it is asked for would have the new
        // camera holding the bars' 0.014 while the bars went dark — the
        // silent shift two index spaces exist to make impossible. Counted
        // against their own kinds there is no length that spells it: this
        // is a routing row short of a camera, which is a refusal.
        let mut params = external();
        params.cameras.push(params.cameras[0].clone());
        let why = validate(&params).unwrap_err();
        assert!(
            why.contains("routing row 0") && why.contains("per camera"),
            "refused for the wrong reason: {why}"
        );

        // And a monitor added, which is the same story on the other axis: the
        // input patch is a row per input over the monitors, so it comes out
        // one monitor short rather than sending the new monitor whatever the
        // old one had.
        let mut params = external();
        params.monitors.push(params.monitors[0].clone());
        params.routing = vec![vec![1.0]; 2];
        params.cameras[0].look = vec![1.0, 0.0];
        let why = validate(&params).unwrap_err();
        assert!(
            why.contains("routing_inputs row 0") && why.contains("per monitor"),
            "refused for the wrong reason: {why}"
        );

        // And a splitter short of a monitor, the check the two above are the
        // switcher's half of.
        let mut params = external();
        params.cameras[0].look.clear();
        let why = validate(&params).unwrap_err();
        assert!(
            why.contains("look has") && why.contains("monitor"),
            "refused for the wrong reason: {why}"
        );
    }

    #[test]
    fn a_switcher_with_the_wrong_number_of_sends_is_refused() {
        // The row count, which is the check that keeps a send addressing a
        // bank layer the graph has not got. An input plugged in and the
        // patch left alone is a source nothing can reach; a row left behind
        // by an input taken away is a tap on a layer past the end of the
        // bank, which the sampler would quietly clamp onto a neighbour
        // rather than fail.
        for wrong in [
            (|p: &mut Params| p.inputs.push(Input::Pattern(Pattern::Bars))) as fn(&mut Params),
            |p: &mut Params| p.routing_inputs.push(vec![0.0]),
        ] {
            let mut params = external();
            wrong(&mut params);
            let why = validate(&params).unwrap_err();
            assert!(
                why.contains("routing_inputs has") && why.contains("per input"),
                "refused for the wrong reason: {why}"
            );
        }
    }

    #[test]
    fn more_inputs_than_the_switcher_has_are_refused() {
        let mut params = one_of_every_input();
        params.inputs = vec![Input::Pattern(Pattern::Bars); MAX_INPUTS + 1];
        params.routing_inputs = vec![vec![0.0]; MAX_INPUTS + 1];
        let why = validate(&params).unwrap_err();
        assert!(
            why.contains("at most"),
            "refused for the wrong reason: {why}"
        );
    }

    #[test]
    fn an_input_patched_past_the_crosspoint_rail_is_refused() {
        // The send's own rail, which the walk over the knobs reads for it
        // like every other: a graph the panel could not put in this state is
        // a graph the loader refuses.
        let (_, high) = Knob::Send.limit().ends();
        for level in [high + 0.1, -0.1, f32::NAN] {
            let mut params = external();
            params.routing_inputs[0][0] = level;
            let why = validate(&params).unwrap_err();
            assert!(
                why.contains("input 0's send to monitor 0"),
                "refused for the wrong reason: {why}"
            );
        }
    }

    #[test]
    fn the_external_preset_is_lit_by_its_input_and_nothing_else() {
        let p = external();
        assert_eq!(p.inputs.len(), 1);
        assert_eq!(p.monitors[0].seed, Seed::Dark, "a blob is still lit");
        // One camera, and it is in the loop: the bars reach the monitor over
        // the switcher, not down a lens, which is the whole shape of the
        // rig. The injection level is the crosspoint the bars are patched on.
        assert_eq!(p.cameras.len(), 1);
        assert_eq!(p.cameras[0].look, [1.0]);
        // The send is the injection level, and what it is *for* is where the
        // loop settles: the trickle divided by the loop's distance from
        // unity, which the doc claims lands just under the bars' own
        // brightness. A number asserted as a number would pass at ten times
        // this and paint a monitor the bars could not.
        let settled = p.routing_inputs[0][0] / (1.0 - p.cameras[0].gain[0]);
        assert!((0.9..1.0).contains(&settled), "settles at {settled}");
    }

    #[test]
    fn the_webcam_preset_keys_a_room_off_a_monitor_and_into_a_loop() {
        let p = webcam();
        assert_eq!(
            p.inputs,
            vec![Input::Capture {
                format: "v4l2".into(),
                device: "/dev/video0".into(),
            }]
        );
        // The device lands whole on the window monitor, which is a window
        // and not a loop: nothing is routed to it, so what the keyed camera
        // watches is one frame of the room and not a frame of the room plus
        // whatever came back.
        assert_eq!(p.routing_inputs, vec![vec![1.0, 0.0]]);
        assert_eq!(p.routing[0], vec![0.0, 0.0]);

        // The key is on the camera watching the window — a camera watching a
        // monitor, which is the only kind there is — and the loop's own
        // camera hands on everything, or the trail would gate itself.
        let (room, loop_camera) = (&p.cameras[0], &p.cameras[1]);
        assert_eq!(room.look, [1.0, 0.0], "the keyed camera is off the window");
        assert_eq!(loop_camera.look, [0.0, 1.0], "the loop camera is its own");
        assert!(room.key.threshold > 0.0 && room.key.softness > 0.0);
        assert_eq!(room.key.tolerance, Key::TOLERANT, "the chroma half is off");
        assert_eq!(loop_camera.key, Key::OFF);

        // A subject settles at its own brightness: what the keyed camera
        // hands over, divided by the loop's distance from unity.
        let settled = room.gain[0] / (1.0 - loop_camera.gain[0]);
        assert!((settled - 1.0).abs() < 0.02, "settles at {settled}");
        // No blob anywhere: every photon on either monitor came in from the
        // room.
        assert!(p.monitors.iter().all(|m| m.seed == Seed::Dark));
        // And it is the only preset that keys anything, so the stage is
        // additive — every other graph hands its light on whole.
        for (name, params) in presets() {
            let keyed = params.cameras.iter().any(|c| c.key != Key::OFF);
            assert_eq!(keyed, name == "webcam", "{name}");
        }
    }

    #[test]
    fn a_white_blob_is_refused_outside_the_light_it_can_be() {
        // No knob turns this, so the load is the only place it is checked at
        // all. On the *second* monitor, because a walk that stops at the
        // first is a check every one-monitor preset passes for free.
        let mut params = crossed();
        assert_eq!(params.monitors.len(), 2);
        let blob = |params: &mut Params, brightness| {
            params.monitors[1].seed = Seed::WhiteBlob(brightness);
        };
        blob(&mut params, 2.0);
        let why = validate(&params).unwrap_err();
        assert!(
            why.contains("monitor 1") && why.contains("white blob"),
            "{why}"
        );
        // A range check is the finiteness check, here as everywhere else: a
        // NaN written into a loop that feeds itself never leaves.
        blob(&mut params, f32::NAN);
        assert!(validate(&params).is_err());
        // The other end, and the whole point of the union: a blob of no
        // light is dark glass, and a file gets one spelling of it — the
        // magic zero a level could not do without.
        blob(&mut params, 0.0);
        let why = validate(&params).unwrap_err();
        assert!(why.contains("dark"), "{why}");
        // And both rigs, said properly, load.
        blob(&mut params, Seed::BRIGHTEST);
        validate(&params).unwrap();
        params.monitors[1].seed = Seed::Dark;
        validate(&params).unwrap();
    }

    /// A file of this test's own: the suite runs in one process, so the pid
    /// alone would have every test here sharing a path.
    fn scratch(what: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("lightherder-cfg-{}-{what}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("graph.toml")
    }

    #[test]
    fn every_field_of_the_format_arrives_off_a_file_that_names_it() {
        // Every value off its default, because a field left at one proves
        // nothing about whether its name was read. Literal, since the
        // instrument writes no file to round-trip against.
        let path = scratch("every-field");
        std::fs::write(
            &path,
            "cameras = [{ look = [0.5], gain = [0.9, 0.85, 0.8],\n\
             \x20 framing = { zoom = 0.994, rotation = 0.05, translate = [0.01, -0.02] },\n\
             \x20 character = { bloom = 0.1, bloom_radius = 0.04, chroma_bleed = 0.02, noise = 0.01 },\n\
             \x20 key = { threshold = 0.2, softness = 0.06, hue = 1.2, tolerance = 0.3 } }]\n\
             monitors = [{ seed = { white_blob = 0.2 }, headroom = 1.5,\n\
             \x20 colour = { hue = 0.1, saturation = 1.1, brightness = 0.02, contrast = 1.05, gamma = 1.2 } }]\n\
             routing = [[0.7]]\n\
             routing_inputs = [[0.3]]\n\
             inputs = [{ pattern = \"bars\" }]\n",
        )
        .unwrap();
        let params = load(path.to_str().unwrap()).unwrap();

        let camera = &params.cameras[0];
        assert_eq!(camera.look, [0.5]);
        assert_eq!(camera.gain, [0.9, 0.85, 0.8]);
        assert_eq!(camera.framing.zoom, 0.994);
        assert_eq!(camera.framing.rotation, 0.05);
        assert_eq!(camera.framing.translate, [0.01, -0.02]);
        assert_eq!(camera.character.bloom, 0.1);
        assert_eq!(camera.character.bloom_radius, 0.04);
        assert_eq!(camera.character.chroma_bleed, 0.02);
        assert_eq!(camera.character.noise, 0.01);
        assert_eq!(camera.key.threshold, 0.2);
        assert_eq!(camera.key.softness, 0.06);
        assert_eq!(camera.key.hue, 1.2);
        assert_eq!(camera.key.tolerance, 0.3);

        let monitor = &params.monitors[0];
        assert_eq!(monitor.seed, Seed::WhiteBlob(0.2));
        assert_eq!(monitor.headroom, 1.5);
        assert_eq!(monitor.colour.hue, 0.1);
        assert_eq!(monitor.colour.saturation, 1.1);
        assert_eq!(monitor.colour.brightness, 0.02);
        assert_eq!(monitor.colour.contrast, 1.05);
        assert_eq!(monitor.colour.gamma, 1.2);

        assert_eq!(params.routing, [[0.7]]);
        assert_eq!(params.routing_inputs, [[0.3]]);
        assert_eq!(params.inputs, [Input::Pattern(Pattern::Bars)]);

        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn a_graph_file_the_instrument_would_refuse_is_refused_at_the_door() {
        // A file is as editable as the hand that wrote it, so the loader
        // validates rather than trusting it — the GPU side is built on that
        // promise.
        let path = scratch("poisoned");
        std::fs::write(
            &path,
            "cameras = [{ look = [1.0], gain = [nan, 1.0, 1.0] }]\n\
             monitors = [{}]\n\
             routing = [[1.0]]\n",
        )
        .unwrap();
        let why = load(path.to_str().unwrap()).unwrap_err();
        assert!(why.contains("graph.toml") && why.contains("NaN"), "{why}");

        std::fs::write(&path, "not toml [").unwrap();
        assert!(load(path.to_str().unwrap()).is_err());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn a_config_with_a_misspelled_key_is_refused() {
        // deny_unknown_fields, so a typo cannot silently leave a knob at its
        // default.
        let err = toml::from_str::<Params>(
            "cameras = [{ look = [1.0], gian = [0.9, 0.9, 0.9] }]\n\
             monitors = [{}]\n\
             routing = [[1.0]]\n",
        );
        assert!(err.is_err());
    }

    #[test]
    fn the_tap_bound_holds_for_every_setting_of_the_switcher_and_not_just_the_one_on_disk() {
        // `taps_of` drops a crosspoint at zero and `Knob::Route` can raise
        // one, so the count validate holds against has to be the reachable
        // one. Every preset: what the file loads with is at or under it, and
        // turning every crosspoint up reaches exactly it.
        for (name, params) in presets() {
            let reachable = crate::feedback::reachable_taps(&params);
            for m in 0..params.monitors.len() {
                let now = crate::feedback::taps_of(&params, m).count();
                assert!(
                    now <= reachable,
                    "{name}: monitor {m} has {now} of {reachable}"
                );
            }
            let mut all_on = params.clone();
            for weight in all_on
                .routing
                .iter_mut()
                .chain(&mut all_on.routing_inputs)
                .flatten()
            {
                *weight = 1.0;
            }
            for m in 0..all_on.monitors.len() {
                assert_eq!(
                    crate::feedback::taps_of(&all_on, m).count(),
                    reachable,
                    "{name}: monitor {m} with the whole switcher up"
                );
            }
        }
        // And `crossed` is a graph where the two differ, so none of the above
        // is comparing a number with itself.
        let crossed = crossed();
        assert_eq!(crate::feedback::taps_of(&crossed, 0).count(), 2);
        assert_eq!(crate::feedback::reachable_taps(&crossed), 4);

        // The zero-weight rule on the switcher's input half, which the sweep
        // above cannot show: an input sent nowhere is no tap, while the bound
        // counts it all the same — the send is a knob, so a zero on disk is
        // no promise about a second later.
        let mut sent_nowhere = external();
        sent_nowhere.routing_inputs[0][0] = 0.0;
        assert_eq!(crate::feedback::taps_of(&sent_nowhere, 0).count(), 1);
        assert_eq!(crate::feedback::reachable_taps(&sent_nowhere), 2);
    }

    #[test]
    fn a_camera_past_the_keys_is_refused_at_load() {
        let mut params = external();
        let spare = params.cameras[0].clone();
        while params.cameras.len() <= MAX_CAMERAS {
            params.cameras.push(spare.clone());
        }
        let why = validate(&params).unwrap_err();
        assert!(
            why.contains("focus key"),
            "refused for the wrong reason: {why}"
        );

        // And exactly at the cap, well-shaped, it loads: the bound is the
        // ninth camera, not the eighth.
        params.cameras.pop();
        for row in &mut params.routing {
            row.resize(params.cameras.len(), 0.0);
        }
        validate(&params).unwrap();
    }

    #[test]
    fn a_graph_the_switcher_could_overrun_is_refused_at_load() {
        // Sparse on disk and legal under a count of what is switched on, but
        // one fader from handing the shader more taps than its array holds.
        let cameras = MAX_TAPS / MAX_MONITORS + 1;
        let mut params = Params {
            cameras: (0..cameras)
                .map(|_| Camera {
                    framing: Framing::identity(),
                    gain: [0.9; 3],
                    character: Character::CLEAN,
                    key: Key::OFF,
                    look: vec![1.0; MAX_MONITORS],
                })
                .collect(),
            monitors: (0..MAX_MONITORS).map(|_| Monitor::default()).collect(),
            inputs: Vec::new(),
            routing_inputs: Vec::new(),
            // One camera per monitor switched on: well under the bound as it
            // stands, and over it the moment a crosspoint is swept.
            routing: (0..MAX_MONITORS)
                .map(|m| (0..cameras).map(|c| f32::from(c == m)).collect())
                .collect(),
        };
        for m in 0..MAX_MONITORS {
            assert!(crate::feedback::taps_of(&params, m).count() <= MAX_TAPS);
        }
        let why = validate(&params).unwrap_err();
        assert!(why.contains("switcher is turned up"), "{why}");
        // One camera fewer and it fits however the switcher is set.
        params.cameras.pop();
        params.routing.iter_mut().for_each(|row| {
            row.pop();
        });
        validate(&params).unwrap();
    }
}
