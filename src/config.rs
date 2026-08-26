//! The graphs the instrument ships with, and loading arbitrary ones from
//! disk. A preset is nothing but a [`Params`] value; a config file is the
//! same struct in TOML, so anything a preset can express a file can too.

use crate::affine::Framing;
use crate::feedback::MAX_TAPS;
use crate::input::{Input, Pattern};
use crate::motion::{Lfo, Shape};
use crate::params::{Camera, Character, Focus, Knob, Limit, Monitor, Params};

/// More monitors than this and the uniform buffer, the present grid and the
/// texture array all need a second look; fewer keeps every one of them dumb.
/// Cameras have no cap of their own: they only reach the GPU as taps, and
/// [`MAX_TAPS`] already bounds those.
pub const MAX_MONITORS: usize = 8;

/// Inputs get their own cap because nothing else bounds them: a camera aimed
/// at one is one tap however many there are, so [`MAX_TAPS`] does not, and
/// each costs a bank layer plus — for a file or a device — a process and a
/// thread of its own. Four is what a switcher has spare inputs for.
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
            look: vec![1.0],
        }],
        monitors: vec![Monitor {
            seed_brightness: 0.10,
            ..Default::default()
        }],
        inputs: Vec::new(),
        routing: vec![vec![1.0]],
        motion: Vec::new(),
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

/// The camera on a motor: [`single`]'s loop with its rotation swept right
/// round, once every twenty seconds. That is a ramp at full depth on the
/// rotation knob and nothing else — the continuous camera rotation of the
/// real instrument, and the demonstration that automation needs no separate
/// kinetics beside it. The spiral it draws winds one way, unwinds through
/// square-on, and winds the other, over and over.
///
/// The base rotation is [`single`]'s rather than zero, so switching the motor
/// off with `p` leaves exactly `single` running. At zero it would leave a
/// camera pointed square on, and a loop whose trail never winds away from the
/// seed piles up on it until the monitor clips — the same reason [`crossed`]
/// and [`insanity`] spin all their cameras the same way. Sweeping *through*
/// square-on is fine; stopping there is not.
///
/// It sweeps slowly enough that even passing through costs something — the
/// trail piles up faster than it winds away, and on hardware that flare
/// reached flat white. So the amplifier's rail sits below white, the way
/// [`analog`]'s does: the flare compresses into a bloom instead of clipping,
/// which is the rail doing the job stage four built it for.
pub fn kinetic() -> Params {
    let mut params = single();
    params.monitors[0].headroom = 0.9;
    params.motion = vec![Lfo {
        depth: std::f32::consts::PI,
        rate: 0.05,
        ..Lfo::new(Knob::Rotation, Focus::default(), Shape::Ramp)
    }];
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
                seed_brightness: 0.10,
                ..Default::default()
            },
            Monitor {
                seed_brightness: 0.10,
                ..Default::default()
            },
        ],
        inputs: Vec::new(),
        routing: vec![vec![0.0, 1.0], vec![1.0, 0.0]],
        motion: Vec::new(),
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
                look,
            }
        })
        .collect();
    Params {
        cameras,
        monitors: (0..N)
            .map(|_| Monitor {
                seed_brightness: 0.10,
                ..Default::default()
            })
            .collect(),
        inputs: Vec::new(),
        routing: vec![vec![1.0 / N as f32; N]; N],
        motion: Vec::new(),
    }
}

/// A test pattern driving the loop instead of the seed spot. One camera is
/// the classic rig, turning and pulling back on its own monitor; the other is
/// pointed at the bars and hands over almost nothing — a hundredth of what it
/// sees. That is the whole point of a loop this close to unity: what settles
/// is the trickle divided by how far the gain is from 1, so a hundredth of
/// the bars becomes most of a monitor. The seed is off, so every photon here
/// came in from outside, and the gain is flat across the channels because an
/// input supplies its own colour — there is nothing for a per-channel decay
/// to add.
pub fn external() -> Params {
    let looking_at_the_loop = Camera {
        framing: Framing {
            zoom: 0.994,
            rotation: 0.05,
            translate: [0.0, 0.0],
        },
        gain: [0.985; 3],
        character: Character::CLEAN,
        look: vec![1.0, 0.0],
    };
    let looking_at_the_bars = Camera {
        // Square on, so the pattern arrives as itself and everything that
        // happens to it afterwards is the loop's doing.
        framing: Framing::identity(),
        gain: [0.014; 3],
        character: Character::CLEAN,
        look: vec![0.0, 1.0],
    };
    Params {
        cameras: vec![looking_at_the_loop, looking_at_the_bars],
        monitors: vec![Monitor::default()],
        inputs: vec![Input::Pattern(Pattern::Bars)],
        routing: vec![vec![1.0, 1.0]],
        motion: Vec::new(),
    }
}

/// A preset: the name the command line knows it by, and the graph it builds.
pub type Preset = (&'static str, fn() -> Params);

/// The presets, by the names the command line and the error messages use.
pub const PRESETS: [Preset; 6] = [
    ("single", single as fn() -> Params),
    ("analog", analog),
    ("kinetic", kinetic),
    ("crossed", crossed),
    ("insanity", insanity),
    ("external", external),
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

/// One TOML file of [`Params`], validated. The one way a graph is read off
/// disk — the preset slots go through here too, so a saved performance and a
/// hand-written config get the same door and the same checks.
pub fn read(path: &std::path::Path) -> Result<Params, String> {
    let shown = path.display();
    let text = std::fs::read_to_string(path).map_err(|e| format!("{shown}: {e}"))?;
    let params: Params = toml::from_str(&text).map_err(|e| format!("{shown}: {e}"))?;
    validate(&params).map_err(|e| format!("{shown}: {e}"))?;
    Ok(params)
}

/// Everything the GPU side assumes about a graph, checked at load — and
/// re-asserted by `Feedback::step`, so the success path must stay
/// allocation-free. Beyond the shape, every number a file can supply is
/// required finite: a NaN written into a loop that feeds itself never leaves,
/// and the knobs cannot repair it — `clamp` passes NaN through, and Reset
/// restores the same poisoned initial.
pub fn validate(params: &Params) -> Result<(), String> {
    let (m, c) = (params.monitors.len(), params.cameras.len());
    if !(1..=MAX_MONITORS).contains(&m) {
        return Err(format!("{m} monitors; needs between 1 and {MAX_MONITORS}"));
    }
    if c == 0 {
        return Err("no cameras; nothing would ever reach a monitor".into());
    }
    if params.inputs.len() > MAX_INPUTS {
        return Err(format!(
            "{} inputs; at most {MAX_INPUTS}",
            params.inputs.len()
        ));
    }
    let weights = |row: &[f32], len: usize| -> Result<(), String> {
        if row.len() != len {
            return Err(format!("has {} entries; needs {len}", row.len()));
        }
        match row.iter().find(|w| !w.is_finite() || **w < 0.0) {
            Some(w) => Err(format!("contains {w}; weights are finite and >= 0")),
            None => Ok(()),
        }
    };
    let finite = |value: f32, what: &str| -> Result<(), String> {
        if value.is_finite() {
            Ok(())
        } else {
            Err(format!("{what} is {value}; every number is finite"))
        }
    };
    for (i, camera) in params.cameras.iter().enumerate() {
        weights(&camera.look, params.sources()).map_err(|e| format!("camera {i}'s look {e}"))?;
        weights(&camera.gain, 3).map_err(|e| format!("camera {i}'s gain {e}"))?;
        let f = &camera.framing;
        for (value, what) in [
            (f.zoom, "zoom"),
            (f.rotation, "rotation"),
            (f.translate[0], "pan x"),
            (f.translate[1], "pan y"),
        ] {
            finite(value, what).map_err(|e| format!("camera {i}'s {e}"))?;
        }
        // The sampling transform divides by the zoom, and a camera that sees
        // nothing but one texel smeared to infinity helps nobody either.
        if f.zoom == 0.0 {
            return Err(format!("camera {i}'s zoom is 0; it would divide by it"));
        }
        let ch = &camera.character;
        for (value, what) in [
            (ch.bloom, "bloom"),
            (ch.bloom_radius, "bloom radius"),
            (ch.chroma_bleed, "chroma bleed"),
            (ch.noise, "noise"),
        ] {
            finite(value, what).map_err(|e| format!("camera {i}'s {e}"))?;
            if value < 0.0 {
                return Err(format!("camera {i}'s {what} is {value}; it is not signed"));
            }
        }
        // Above its rail `mix` extrapolates away from the halo instead of
        // towards it, which is a lens returning more light than it was
        // handed — and inside a loop that is a multiply, not an artefact.
        // The rail is the knob's, read from it rather than repeated: two
        // numbers meaning one thing is how a config reaches a state its own
        // knob cannot return it from.
        let Limit::Clamp(_, most) = Knob::Bloom.limit() else {
            unreachable!("bloom clamps")
        };
        if ch.bloom > most {
            return Err(format!(
                "camera {i}'s bloom is {}; a lens scatters at most {most} of it",
                ch.bloom
            ));
        }
    }
    if params.routing.len() != m {
        return Err(format!(
            "routing has {} rows; needs one per monitor, {m}",
            params.routing.len()
        ));
    }
    for (i, row) in params.routing.iter().enumerate() {
        weights(row, c).map_err(|e| format!("routing row {i} {e}"))?;
    }
    for (i, monitor) in params.monitors.iter().enumerate() {
        let colour = &monitor.colour;
        for (value, what) in [
            (monitor.seed_brightness, "seed brightness"),
            (monitor.headroom, "headroom"),
            (colour.hue, "hue"),
            (colour.saturation, "saturation"),
            (colour.brightness, "brightness"),
            (colour.contrast, "contrast"),
            (colour.gamma, "gamma"),
        ] {
            finite(value, what).map_err(|e| format!("monitor {i}'s {e}"))?;
        }
        if monitor.seed_brightness < 0.0 {
            return Err(format!("monitor {i}'s seed brightness is negative"));
        }
        // The rail's curve divides by the headroom, and a rail at or below
        // zero is an amplifier with no output at all.
        if monitor.headroom <= 0.0 {
            return Err(format!(
                "monitor {i}'s headroom is {}; the amplifier needs some",
                monitor.headroom
            ));
        }
        // pow(0, g) for g <= 0 is an infinity, and the monitor's corners are
        // exactly 0 whenever the seed does not reach them. One pass later the
        // chroma matrix turns that infinity into a NaN, which per the note
        // above never leaves the loop. The knob's own floor is 0.25.
        if colour.gamma <= 0.0 {
            return Err(format!(
                "monitor {i}'s gamma is {}; black to the power of it is not a number",
                colour.gamma
            ));
        }
    }
    for (i, lfo) in params.motion.iter().enumerate() {
        let at = |e: String| format!("motion {i} ({}) {e}", lfo.knob.name());
        for (value, what) in [
            (lfo.rate, "rate"),
            (lfo.depth, "depth"),
            (lfo.phase, "phase"),
        ] {
            if !value.is_finite() {
                return Err(at(format!(
                    "has a {what} of {value}; every number is finite"
                )));
            }
        }
        // Both rails read off the knob itself, so an automation range and the
        // knob's range cannot come apart.
        let Limit::Clamp(_, fastest) = Lfo::RATE else {
            unreachable!("the rate clamps")
        };
        if !(0.0..=fastest).contains(&lfo.rate) {
            return Err(at(format!(
                "runs at {} Hz; between 0 and {fastest} — half the frame rate",
                lfo.rate
            )));
        }
        let widest = Lfo::depth_limit(lfo.knob);
        if !(0.0..=widest).contains(&lfo.depth) {
            return Err(at(format!(
                "swings {}; at most {widest}, which is all the travel that knob has",
                lfo.depth
            )));
        }
        if lfo.focus.camera >= c || lfo.focus.monitor >= m {
            return Err(at(format!(
                "drives camera {} of {c}, monitor {} of {m}",
                lfo.focus.camera + 1,
                lfo.focus.monitor + 1
            )));
        }
        // A monitor knob does not read a camera index, so a file that sets
        // one is saying something the instrument cannot do — and it would
        // hide the automation from the keys, which look it up narrowed.
        if lfo.focus != lfo.focus.narrowed(lfo.knob) {
            let side = if lfo.knob.is_camera() {
                ("monitor", "a camera knob")
            } else {
                ("camera", "a monitor knob")
            };
            return Err(at(format!("names a {}; {} has none", side.0, side.1)));
        }
    }
    // The flattened routing-times-look products are what the shader
    // iterates, and its uniform array is a fixed size. Counted by the same
    // function that builds them.
    for i in 0..m {
        let taps = crate::feedback::taps_of(params, i).count();
        if taps > MAX_TAPS {
            return Err(format!(
                "monitor {i} is fed by {taps} taps; at most {MAX_TAPS}"
            ));
        }
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
        // Over the monitors a camera looks at, and not its inputs: an input
        // is light entering the graph, so it belongs to what the loop is
        // driven *by*, not to what it multiplies — the seed is left out of
        // this sum for exactly the same reason. This is the first place the
        // one index space over two kinds of source charges rent, and it will
        // not be the last: anywhere the loop's own gain is reasoned about,
        // `look` has to be sliced.
        for (name, params) in presets() {
            let monitors = params.monitors.len();
            for (i, row) in params.routing.iter().enumerate() {
                let sum: f32 = (0..3)
                    .map(|ch| {
                        row.iter()
                            .zip(&params.cameras)
                            .map(|(route, cam)| {
                                route * cam.gain[ch] * cam.look[..monitors].iter().sum::<f32>()
                            })
                            .sum()
                    })
                    .fold(0.0, f32::max);
                assert!(sum < 1.0, "{name} monitor {i}: gain sum {sum} blooms");
                assert!(sum > 0.9, "{name} monitor {i}: gain sum {sum} dies fast");
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
    fn a_config_file_round_trips() {
        let dir = std::env::temp_dir().join(format!("lightherder-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Between them these three carry a non-default value in every field
        // the format has: the routing and splitter weights, the character and
        // the rail, the inputs, and the automation.
        for params in [crossed(), analog(), external(), kinetic()] {
            let path = dir.join("preset.toml");
            std::fs::write(&path, toml::to_string(&params).unwrap()).unwrap();
            assert_eq!(load(path.to_str().unwrap()).unwrap(), params);
        }
        std::fs::remove_dir_all(&dir).unwrap();
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
        assert_eq!(params.monitors[0].seed_brightness, 0.0);
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
            // The amplifier's rail is the other half of that stage, and
            // `kinetic` runs it below white too — on purpose, and for a
            // different reason: it sweeps through square-on, where the trail
            // piles up on the seed instead of winding away from it, and the
            // rail is what bounds that pile-up. Verified on hardware by
            // `the_turning_camera_keeps_an_image_all_the_way_round`.
            let open_rail = params
                .monitors
                .iter()
                .all(|m| m.headroom == Monitor::KNEE_AT_WHITE);
            assert_eq!(
                open_rail,
                !["analog", "kinetic"].contains(&name),
                "{name}'s rail"
            );
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

        let mut negative = crossed();
        negative.routing[0][1] = -1.0;
        assert!(validate(&negative).is_err());

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
        let poison: &[fn(&mut Params)] = &[
            |p| p.cameras[0].gain[0] = f32::NAN,
            |p| p.cameras[0].gain[1] = -1.0,
            |p| p.cameras[0].framing.zoom = 0.0,
            |p| p.cameras[0].framing.rotation = f32::INFINITY,
            |p| p.monitors[0].colour.gamma = f32::NAN,
            |p| p.monitors[0].seed_brightness = -0.5,
            |p| p.routing[0][1] = f32::INFINITY,
            |p| p.cameras[0].character.bloom = f32::NAN,
            |p| p.cameras[0].character.bloom = 1.5,
            |p| p.cameras[0].character.bloom_radius = -0.01,
            |p| p.cameras[0].character.chroma_bleed = f32::INFINITY,
            |p| p.cameras[0].character.noise = -1.0,
            |p| p.monitors[0].headroom = 0.0,
            |p| p.monitors[0].headroom = f32::NAN,
            |p| p.monitors[0].colour.gamma = 0.0,
            |p| p.monitors[0].colour.gamma = -1.0,
        ];
        for (i, poison) in poison.iter().enumerate() {
            let mut params = crossed();
            poison(&mut params);
            assert!(validate(&params).is_err(), "poison {i} passed");
        }
    }

    /// A graph carrying one of every kind of input, with looks long enough
    /// for them: `external` plus a grid, a file and a device.
    fn one_of_every_input() -> Params {
        let mut params = external();
        params.inputs = vec![
            Input::Pattern(Pattern::Grid),
            Input::File("clip.mp4".into()),
            Input::Capture {
                format: "v4l2".into(),
                device: "/dev/video0".into(),
            },
        ];
        for camera in &mut params.cameras {
            camera.look.resize(params.monitors.len() + 3, 0.0);
        }
        params
    }

    #[test]
    fn every_kind_of_input_spells_itself_the_way_it_is_documented() {
        // The literal keys, not a round trip: a round trip agrees with itself
        // whatever serde is told to call these, and every config file and
        // every line of the README that mentions an input depends on the
        // names rather than on the agreement.
        let params: Params = toml::from_str(
            "cameras = [{ look = [1.0, 0.0, 0.0, 0.0] }]\n\
             monitors = [{}]\n\
             routing = [[1.0]]\n\
             inputs = [\n\
             \x20 { pattern = \"grid\" },\n\
             \x20 { file = \"clip.mp4\" },\n\
             \x20 { capture = { format = \"v4l2\", device = \"/dev/video0\" } },\n\
             ]\n",
        )
        .unwrap();
        validate(&params).unwrap();
        assert_eq!(params.inputs, one_of_every_input().inputs);
        // And out again, so a file written by the instrument reads back in.
        let written = toml::to_string(&params).unwrap();
        assert_eq!(toml::from_str::<Params>(&written).unwrap(), params);
    }

    #[test]
    fn a_look_has_to_cover_the_inputs_too() {
        // The failure this stops is silent: a look one entry short used to be
        // exactly right, and every camera would still be aimed at something.
        let mut params = external();
        params.cameras[0].look.pop();
        params.cameras[1].look.pop();
        let why = validate(&params).unwrap_err();
        assert!(why.contains("look"), "refused for the wrong reason: {why}");
    }

    #[test]
    fn more_inputs_than_the_switcher_has_are_refused() {
        let mut params = one_of_every_input();
        params.inputs = vec![Input::Pattern(Pattern::Bars); MAX_INPUTS + 1];
        for camera in &mut params.cameras {
            camera
                .look
                .resize(params.monitors.len() + MAX_INPUTS + 1, 0.0);
        }
        let why = validate(&params).unwrap_err();
        assert!(
            why.contains("at most"),
            "refused for the wrong reason: {why}"
        );
    }

    #[test]
    fn the_external_preset_is_lit_by_its_input_and_nothing_else() {
        let p = external();
        assert_eq!(p.inputs.len(), 1);
        assert_eq!(p.monitors[0].seed_brightness, 0.0, "the seed is still on");
        // One camera in the loop, one on the input, and no camera doing both:
        // the injection level is that camera's gain, which is only true while
        // it sees nothing else.
        let input = p.monitors.len();
        let on_input: Vec<usize> = (0..p.cameras.len())
            .filter(|c| p.cameras[*c].look[input] > 0.0)
            .collect();
        assert_eq!(on_input, vec![1]);
        assert_eq!(p.cameras[1].look[..p.monitors.len()], [0.0]);
        assert_eq!(p.cameras[0].look[input], 0.0);
    }

    #[test]
    fn automation_spells_itself_the_way_it_is_documented() {
        // The README's own example, literally — knob names with a space in
        // them included. Not a round trip: a round trip agrees with itself
        // whatever serde is told to call these, while every line of the
        // README depends on the names.
        let params: Params = toml::from_str(
            "cameras = [{ look = [1.0] }, { look = [1.0] }]\n\
             monitors = [{}]\n\
             routing = [[1.0, 0.0]]\n\
             motion = [\n\
             \x20 { knob = \"rotation\", shape = \"ramp\", rate = 0.05, depth = 3.14159 },\n\
             \x20 { knob = \"hue\", shape = \"sine\", rate = 0.2, depth = 0.4, phase = 0.25 },\n\
             \x20 { knob = \"pan x\", rate = 0.1, depth = 0.3, focus = { camera = 1 } },\n\
             ]\n",
        )
        .unwrap();
        validate(&params).unwrap();
        assert_eq!(params.motion[0].knob, Knob::Rotation);
        assert_eq!(params.motion[0].shape, Shape::Ramp);
        assert_eq!(params.motion[2].knob, Knob::TranslateX);
        assert_eq!(params.motion[2].focus.camera, 1);
        // The keys the third line omits fall to their documented defaults.
        assert_eq!(params.motion[2].shape, Shape::Sine);
        assert_eq!(params.motion[2].phase, 0.0);
        assert_eq!(params.motion[1].focus, Focus::default());
    }

    #[test]
    fn the_kinetic_preset_is_a_camera_that_keeps_turning() {
        let p = kinetic();
        let [lfo] = p.motion[..] else {
            panic!("kinetic is one turning camera")
        };
        assert_eq!(lfo.knob, Knob::Rotation);
        assert_eq!(lfo.shape, Shape::Ramp);
        assert_eq!(
            lfo.depth,
            Lfo::depth_limit(Knob::Rotation),
            "not a full turn"
        );
        assert!(lfo.rate > 0.0, "the motor is off");
        // And it is the only preset that moves on its own, so the others are
        // exactly as they were before this stage.
        for (name, params) in presets() {
            assert_eq!(params.motion.is_empty(), name != "kinetic", "{name}");
        }
    }

    #[test]
    fn a_graph_whose_automation_is_out_of_shape_is_refused() {
        fn lfo(knob: Knob) -> Lfo {
            Lfo::new(knob, Focus::default(), Shape::Sine)
        }
        /// What the poison does, and a name for the failure it stands for.
        type Poison = (&'static str, fn(&mut Params));
        let poison: &[Poison] = &[
            ("a knob on a camera that is not there", |p| {
                p.motion = vec![Lfo {
                    focus: Focus {
                        camera: 9,
                        monitor: 0,
                    },
                    ..Lfo::new(Knob::Zoom, Focus::default(), Shape::Sine)
                }]
            }),
            ("a knob on a monitor that is not there", |p| {
                p.motion = vec![Lfo {
                    focus: Focus {
                        camera: 0,
                        monitor: 9,
                    },
                    ..Lfo::new(Knob::Hue, Focus::default(), Shape::Sine)
                }]
            }),
            ("a monitor knob naming a camera", |p| {
                p.motion = vec![Lfo {
                    focus: Focus {
                        camera: 1,
                        monitor: 0,
                    },
                    ..Lfo::new(Knob::Hue, Focus::default(), Shape::Sine)
                }]
            }),
            ("a camera knob naming a monitor", |p| {
                p.motion = vec![Lfo {
                    focus: Focus {
                        camera: 0,
                        monitor: 1,
                    },
                    ..Lfo::new(Knob::Zoom, Focus::default(), Shape::Sine)
                }]
            }),
            ("a rate past Nyquist", |p| {
                p.motion = vec![Lfo {
                    rate: 60.0,
                    ..lfo(Knob::Hue)
                }]
            }),
            ("a negative rate", |p| {
                p.motion = vec![Lfo {
                    rate: -1.0,
                    ..lfo(Knob::Hue)
                }]
            }),
            ("a swing wider than the knob", |p| {
                p.motion = vec![Lfo {
                    depth: 99.0,
                    ..lfo(Knob::Zoom)
                }]
            }),
            ("a negative swing", |p| {
                p.motion = vec![Lfo {
                    depth: -0.1,
                    ..lfo(Knob::Zoom)
                }]
            }),
            ("a rate that is not a number", |p| {
                p.motion = vec![Lfo {
                    rate: f32::NAN,
                    ..lfo(Knob::Hue)
                }]
            }),
            ("a phase that is not a number", |p| {
                p.motion = vec![Lfo {
                    phase: f32::INFINITY,
                    ..lfo(Knob::Hue)
                }]
            }),
        ];
        for (what, poison) in poison {
            // Two cameras and two monitors, so "9" is out of range but "1" is
            // a real node — the focus rules have to be about the knob's own
            // side of the graph, not about the index existing.
            let mut params = crossed();
            poison(&mut params);
            assert!(validate(&params).is_err(), "{what} passed");
        }
        // And the ones the rules are drawn around still load.
        let mut params = crossed();
        params.motion = vec![
            Lfo::new(
                Knob::Zoom,
                Focus {
                    camera: 1,
                    monitor: 0,
                },
                Shape::Ramp,
            ),
            Lfo::new(
                Knob::Hue,
                Focus {
                    camera: 0,
                    monitor: 1,
                },
                Shape::Sine,
            ),
            // Two on one knob: a beat, not a conflict.
            Lfo::new(
                Knob::Hue,
                Focus {
                    camera: 0,
                    monitor: 1,
                },
                Shape::Sine,
            ),
            Lfo {
                rate: 0.0,
                ..lfo(Knob::Gamma)
            },
        ];
        validate(&params).unwrap();
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
}
