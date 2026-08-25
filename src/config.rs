//! The graphs the instrument ships with, and loading arbitrary ones from
//! disk. A preset is nothing but a [`Params`] value; a config file is the
//! same struct in TOML, so anything a preset can express a file can too.

use crate::affine::Framing;
use crate::feedback::MAX_TAPS;
use crate::params::{Camera, Colour, Monitor, Params};

/// More monitors than this and the uniform buffer, the present grid and the
/// texture array all need a second look; fewer keeps every one of them dumb.
/// Cameras have no cap of their own: they only reach the GPU as taps, and
/// [`MAX_TAPS`] already bounds those.
pub const MAX_MONITORS: usize = 8;

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
            look: vec![1.0],
        }],
        monitors: vec![Monitor {
            colour: Colour::NEUTRAL,
            seed_brightness: 0.10,
        }],
        routing: vec![vec![1.0]],
    }
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
                colour: Colour::NEUTRAL,
                seed_brightness: 0.10,
            },
            Monitor {
                colour: Colour::NEUTRAL,
                seed_brightness: 0.10,
            },
        ],
        routing: vec![vec![0.0, 1.0], vec![1.0, 0.0]],
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
                look,
            }
        })
        .collect();
    Params {
        cameras,
        monitors: (0..N)
            .map(|_| Monitor {
                colour: Colour::NEUTRAL,
                seed_brightness: 0.10,
            })
            .collect(),
        routing: vec![vec![1.0 / N as f32; N]; N],
    }
}

/// `arg` is a preset name or a path to a TOML file of [`Params`]. Either way
/// the result is validated, so the GPU side can trust its shape.
pub fn load(arg: &str) -> Result<Params, String> {
    let params = match arg {
        "single" => single(),
        "crossed" => crossed(),
        "insanity" => insanity(),
        path => {
            let text = std::fs::read_to_string(path)
                .map_err(|e| format!("{path}: {e} (presets: single, crossed, insanity)"))?;
            toml::from_str(&text).map_err(|e| format!("{path}: {e}"))?
        }
    };
    validate(&params)?;
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
        weights(&camera.look, m).map_err(|e| format!("camera {i}'s look {e}"))?;
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

    fn presets() -> [(&'static str, Params); 3] {
        [
            ("single", single()),
            ("crossed", crossed()),
            ("insanity", insanity()),
        ]
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
        for (name, params) in presets() {
            for (i, row) in params.routing.iter().enumerate() {
                let sum: f32 = (0..3)
                    .map(|ch| {
                        row.iter()
                            .zip(&params.cameras)
                            .map(|(route, cam)| route * cam.gain[ch] * cam.look.iter().sum::<f32>())
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
        let path = dir.join("crossed.toml");
        std::fs::write(&path, toml::to_string(&crossed()).unwrap()).unwrap();
        assert_eq!(load(path.to_str().unwrap()).unwrap(), crossed());
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
        assert_eq!(params.monitors[0].colour, Colour::NEUTRAL);
        assert_eq!(params.monitors[0].seed_brightness, 0.0);
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
        ];
        for (i, poison) in poison.iter().enumerate() {
            let mut params = crossed();
            poison(&mut params);
            assert!(validate(&params).is_err(), "poison {i} passed");
        }
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
