//! The command line: which graph to play, how much detail it carries, and
//! whether the window covers the display it opens on.

use crate::config::PRESETS;

/// How big every monitor in the bank is, and with it the resolution the whole
/// loop runs at: a camera pass is one fragment per texel of the monitor it
/// draws to, so this is what the GPU is actually being asked for.
///
/// It is fixed for a run rather than following the window, so resizing
/// rescales the view instead of scrambling the loops' state, and the framing
/// numbers keep meaning the same thing throughout. It is not part of a graph
/// either: every position in this instrument is in screen units and every
/// weight is a ratio, so the size changes how much detail the loop carries
/// and nothing about what it does — which makes it a property of the machine
/// it is deployed on rather than of the piece being played.
pub const DEFAULT_RESOLUTION: (u32, u32) = (1920, 1080);

/// The largest monitor accepted, per side. Not the same cap as
/// [`crate::feedback::bank_fits`], which bounds the bank in bytes and so
/// catches many layers of a merely large monitor: this one is the side of a
/// texture, where every GPU worth deploying on stops at 8192 and a request
/// past it is a validation error inside wgpu rather than a line here.
pub const MAX_RESOLUTION: u32 = 7680;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mode {
    /// Open a window and play.
    Play,
    /// Print the controls — the keys, and the surface as it is actually
    /// mapped — and exit.
    Cheatsheet,
    /// Step the graph off screen, as fast as the GPU will take it, and report
    /// what a frame costs. The only way to see how much of a pass is spare: on
    /// a display the loop is paced by the tempo, so a window reports the rate
    /// it was asked for at every resolution it can still make in time.
    Bench,
    /// Print how to start it, and stop.
    Usage,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cli {
    /// A preset name or the path to a graph file.
    pub graph: String,
    pub resolution: (u32, u32),
    /// Passes a second — the speed the piece plays at, which the tempo keys
    /// move from here while it runs. See [`crate::tempo`].
    pub rate: f32,
    /// Deployed, this instrument is the only thing on its display, so the
    /// window covers it unless asked otherwise — `--windowed` is there
    /// because a machine being worked on is not a machine being played.
    pub fullscreen: bool,
    pub mode: Mode,
}

impl Default for Cli {
    fn default() -> Cli {
        Cli {
            graph: PRESETS[0].0.into(),
            resolution: DEFAULT_RESOLUTION,
            rate: crate::tempo::DEFAULT_RATE,
            fullscreen: true,
            mode: Mode::Play,
        }
    }
}

pub fn usage() -> String {
    let names: Vec<&str> = PRESETS.iter().map(|(name, _)| *name).collect();
    format!(
        "usage: lightherder [options] [{} | graph.toml]\n\
         \x20 --windowed          open a window instead of covering the display\n\
         \x20 --resolution WxH    how big every monitor is (default {}x{})\n\
         \x20 --rate HZ           passes a second, the speed the piece plays at\n\
         \x20                     (default {}, {} to {}; the 7 and 8 keys and the\n\
         \x20                     surface's track pair move it from there)\n\
         \x20 --cheatsheet        print the controls and exit\n\
         \x20 --bench             time {} frames off screen and exit\n\
         \x20 --help              this\n",
        names.join(" | "),
        DEFAULT_RESOLUTION.0,
        DEFAULT_RESOLUTION.1,
        crate::tempo::DEFAULT_RATE,
        crate::tempo::MIN_RATE,
        crate::tempo::MAX_RATE,
        crate::bench::FRAMES,
    )
}

/// `args` is the command line with the program name already dropped.
///
/// Anything that is not a flag is the graph, of which there may be one: a
/// second is a typo, and playing the first of two graphs silently is the
/// wrong answer to it.
pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Cli, String> {
    let mut cli = Cli::default();
    let mut graph: Option<String> = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--windowed" => cli.fullscreen = false,
            // One mode a run, for the reason a second graph is refused below:
            // last-flag-wins on `--bench --cheatsheet` is a typo answered by
            // doing one of the two things silently.
            "--cheatsheet" => mode(&mut cli.mode, Mode::Cheatsheet)?,
            "--bench" => mode(&mut cli.mode, Mode::Bench)?,
            "--help" | "-h" => mode(&mut cli.mode, Mode::Usage)?,
            // Both spellings, because a flag with a value is written with a
            // space as often as with an equals, and the one a parser refuses
            // lands as a second graph rather than as an error about sizes.
            "--resolution" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("--resolution needs a size\n{}", usage()))?;
                cli.resolution = resolution(&value)?;
            }
            "--rate" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("--rate needs a number of passes\n{}", usage()))?;
                cli.rate = rate(&value)?;
            }
            _ => {
                if let Some(value) = arg.strip_prefix("--resolution=") {
                    cli.resolution = resolution(value)?;
                } else if let Some(value) = arg.strip_prefix("--rate=") {
                    cli.rate = rate(value)?;
                } else if arg.starts_with('-') {
                    return Err(format!("no such option: {arg}\n{}", usage()));
                } else if let Some(first) = &graph {
                    return Err(format!(
                        "two graphs, {first:?} and {arg:?}; there is one instrument\n{}",
                        usage()
                    ));
                } else {
                    graph = Some(arg);
                }
            }
        }
    }
    if let Some(graph) = graph {
        cli.graph = graph;
    }
    Ok(cli)
}

/// Take `mode`, unless one has already been taken.
fn mode(current: &mut Mode, wanted: Mode) -> Result<(), String> {
    if *current != Mode::Play {
        return Err(format!(
            "{current:?} and {wanted:?} are two different runs; ask for one\n{}",
            usage()
        ));
    }
    *current = wanted;
    Ok(())
}

/// `WIDTHxHEIGHT`, the way a display is spelled everywhere else.
fn resolution(value: &str) -> Result<(u32, u32), String> {
    let shape = || format!("resolution {value:?} is not WIDTHxHEIGHT, e.g. 3840x2160");
    let (w, h) = value.split_once(['x', 'X']).ok_or_else(shape)?;
    let side = |text: &str| -> Result<u32, String> {
        let n: u32 = text.parse().map_err(|_| shape())?;
        match n {
            0 => Err("a monitor with a zero side shows nothing".into()),
            n if n > MAX_RESOLUTION => Err(format!(
                "{n} is past {MAX_RESOLUTION}, the widest monitor this builds"
            )),
            n => Ok(n),
        }
    };
    Ok((side(w)?, side(h)?))
}

/// Passes a second. Refused rather than clamped when it is outside the range
/// the instrument plays at: a performer who typed 6000 meant something, and
/// silently playing 240 instead would answer neither the number typed nor the
/// mistake behind it.
fn rate(value: &str) -> Result<f32, String> {
    use crate::tempo::{MAX_RATE, MIN_RATE};
    let hz: f32 = value
        .parse()
        .map_err(|_| format!("rate {value:?} is not a number of passes a second"))?;
    if !(MIN_RATE..=MAX_RATE).contains(&hz) {
        return Err(format!(
            "{hz} passes a second is outside {MIN_RATE} to {MAX_RATE}, the range this plays at"
        ));
    }
    Ok(hz)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_argv(args: &[&str]) -> Result<Cli, String> {
        parse(args.iter().map(|a| a.to_string()))
    }

    #[test]
    fn an_empty_command_line_plays_the_first_preset_over_the_whole_display() {
        let cli = parse_argv(&[]).unwrap();
        assert_eq!(cli.graph, PRESETS[0].0);
        assert!(cli.fullscreen);
        assert_eq!(cli.mode, Mode::Play);
        assert_eq!(cli.resolution, DEFAULT_RESOLUTION);
        assert_eq!(cli.rate, crate::tempo::DEFAULT_RATE);
    }

    #[test]
    fn a_bare_word_is_the_graph_wherever_it_stands() {
        // Before the flags and after them: a performer types the piece where
        // it comes to mind.
        assert_eq!(parse_argv(&["crossed"]).unwrap().graph, "crossed");
        assert_eq!(
            parse_argv(&["--windowed", "my.toml"]).unwrap().graph,
            "my.toml"
        );
        assert_eq!(
            parse_argv(&["my.toml", "--windowed"]).unwrap().graph,
            "my.toml"
        );
    }

    #[test]
    fn the_flags_do_what_they_say() {
        assert!(!parse_argv(&["--windowed"]).unwrap().fullscreen);
        assert_eq!(parse_argv(&["--bench"]).unwrap().mode, Mode::Bench);
        assert_eq!(
            parse_argv(&["--cheatsheet"]).unwrap().mode,
            Mode::Cheatsheet
        );
        assert_eq!(parse_argv(&["--help"]).unwrap().mode, Mode::Usage);
        // Both spellings of a flag with a value, since the usage shows one
        // and hands reach for the other.
        for spelling in [
            vec!["--resolution=3840x2160"],
            vec!["--resolution", "3840x2160"],
        ] {
            assert_eq!(parse_argv(&spelling).unwrap().resolution, (3840, 2160));
        }
        assert!(parse_argv(&["--resolution"])
            .unwrap_err()
            .contains("needs a size"));
    }

    #[test]
    fn a_rate_outside_the_range_is_refused_rather_than_clamped() {
        for spelling in [vec!["--rate=15"], vec!["--rate", "15"]] {
            assert_eq!(parse_argv(&spelling).unwrap().rate, 15.0);
        }
        // Clamping would play 240 for a piece asked to run at 6000, which
        // is neither the number typed nor a word about the mistake.
        let why = parse_argv(&["--rate=6000"]).unwrap_err();
        assert!(why.contains("outside"), "{why}");
        assert!(parse_argv(&["--rate=0"]).is_err());
        assert!(parse_argv(&["--rate=fast"])
            .unwrap_err()
            .contains("not a number"));
        assert!(parse_argv(&["--rate=NaN"]).is_err());
        assert!(parse_argv(&["--rate"]).unwrap_err().contains("needs a"));
    }

    #[test]
    fn a_second_run_is_refused_the_same_way_a_second_graph_is() {
        // Last-flag-wins here would answer a typo by doing one of the two
        // things silently, which is the case the graphs are refused for.
        let why = parse_argv(&["--bench", "--cheatsheet"]).unwrap_err();
        assert!(why.contains("Bench") && why.contains("Cheatsheet"), "{why}");
        assert!(parse_argv(&["--help", "--bench"]).is_err());
        // Once each is not two: repeating one flag is not a second run.
        assert_eq!(
            parse_argv(&["--windowed", "--windowed"]).unwrap().mode,
            Mode::Play
        );
    }

    #[test]
    fn a_second_graph_is_refused_rather_than_ignored() {
        let why = parse_argv(&["single", "crossed"]).unwrap_err();
        assert!(why.contains("single") && why.contains("crossed"), "{why}");
        // The shape this is really for: a size that missed its flag would
        // otherwise be read as a filename and the graph played at the wrong
        // resolution with nothing said.
        let why = parse_argv(&["single", "3840x2160"]).unwrap_err();
        assert!(why.contains("3840x2160"), "{why}");
    }

    #[test]
    fn a_misspelled_flag_stops_rather_than_becoming_a_filename() {
        let why = parse_argv(&["--fullscreen"]).unwrap_err();
        assert!(why.contains("no such option"), "{why}");
        assert!(why.contains("--windowed"), "the usage is missing: {why}");
    }

    #[test]
    fn a_resolution_that_is_not_one_says_so() {
        for bad in ["1920", "1920*1080", "wide x tall", "1920x", "x1080"] {
            let why = parse_argv(&[&format!("--resolution={bad}")]).unwrap_err();
            assert!(why.contains("WIDTHxHEIGHT"), "{bad}: {why}");
        }
        assert!(parse_argv(&["--resolution=1920x0"])
            .unwrap_err()
            .contains("zero side"));
        let why = parse_argv(&["--resolution=38400x2160"]).unwrap_err();
        assert!(why.contains(&MAX_RESOLUTION.to_string()), "{why}");
    }

    #[test]
    fn the_usage_names_every_flag_the_parser_answers_to() {
        let usage = usage();
        for flag in [
            "--windowed",
            "--resolution",
            "--rate",
            "--cheatsheet",
            "--bench",
        ] {
            assert!(usage.contains(flag), "{flag} is not in the usage");
        }
        // And every preset, so the list a performer is shown is the loader's
        // rather than one typed beside it.
        for (name, _) in PRESETS {
            assert!(usage.contains(name), "{name} is not in the usage");
        }
    }
}
