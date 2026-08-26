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

/// The largest monitor accepted, per side. The real limit is the bank, which
/// is [`crate::feedback::bank_fits`]'s to enforce because only it knows how
/// many layers a graph has; this one keeps a typo like `--resolution 38400x2160`
/// from being reported as a memory figure.
pub const MAX_RESOLUTION: u32 = 7680;

/// How many frames [`Mode::Bench`] times, after a warm-up. Ten seconds' worth
/// at sixty, long enough that a shader compile or a first-touch allocation
/// cannot be most of the answer.
pub const BENCH_FRAMES: u32 = 600;

/// Frames run and thrown away first: pipelines are built and every texture in
/// the bank is touched for the first time on the way through frame one.
pub const BENCH_WARMUP: u32 = 60;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mode {
    /// Open a window and play.
    Play,
    /// Print the controls — the keys, and the surface as it is actually
    /// mapped — and exit.
    Cheatsheet,
    /// Step the graph off screen, as fast as the GPU will take it, and report
    /// what a frame costs. The only way to see how much of a frame is spare:
    /// on a display the loop is paced by the vertical blank, so a window
    /// reports sixty at every resolution it can still make in time.
    Bench,
    /// Print how to start it, and stop.
    Usage,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cli {
    /// A preset name or the path to a graph file.
    pub graph: String,
    pub resolution: (u32, u32),
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
         \x20 --cheatsheet        print the controls and exit\n\
         \x20 --bench             time {BENCH_FRAMES} frames off screen and exit\n\
         \x20 --help              this\n",
        names.join(" | "),
        DEFAULT_RESOLUTION.0,
        DEFAULT_RESOLUTION.1,
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
            "--cheatsheet" => cli.mode = Mode::Cheatsheet,
            "--bench" => cli.mode = Mode::Bench,
            "--help" | "-h" => cli.mode = Mode::Usage,
            // Both spellings, because a flag with a value is written with a
            // space as often as with an equals, and the one a parser refuses
            // lands as a second graph rather than as an error about sizes.
            "--resolution" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("--resolution needs a size\n{}", usage()))?;
                cli.resolution = resolution(&value)?;
            }
            _ => match arg.strip_prefix("--resolution=") {
                Some(value) => cli.resolution = resolution(value)?,
                None if arg.starts_with('-') => {
                    return Err(format!("no such option: {arg}\n{}", usage()))
                }
                None => match &graph {
                    Some(first) => {
                        return Err(format!(
                            "two graphs, {first:?} and {arg:?}; there is one instrument\n{}",
                            usage()
                        ))
                    }
                    None => graph = Some(arg),
                },
            },
        }
    }
    if let Some(graph) = graph {
        cli.graph = graph;
    }
    Ok(cli)
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
        for flag in ["--windowed", "--resolution", "--cheatsheet", "--bench"] {
            assert!(usage.contains(flag), "{flag} is not in the usage");
        }
        // And every preset, so the list a performer is shown is the loader's
        // rather than one typed beside it.
        for (name, _) in PRESETS {
            assert!(usage.contains(name), "{name} is not in the usage");
        }
    }
}
