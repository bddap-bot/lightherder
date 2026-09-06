//! Play a written recipe into the instrument off screen and write the frame
//! it lands on.
//!
//! A recipe here is the same control sequence a hand plays on the
//! nanoKONTROL2, one control to a line, so what this renders and what the
//! prose in `recipes/README.md` tells a performer to do are the same
//! sequence read twice. It opens no window and no surface: the rig is
//! stepped from the same [`Feedback`] the display drives and the frame is
//! written by the same [`Capture`] the marker-set button uses.
//!
//! ```text
//! cargo run --release --example recipe -- recipes/single-spiral.txt ours.png
//! ```

use std::path::{Path, PathBuf};

use lightherder::affine::Axis;
use lightherder::capture::Capture;
use lightherder::feedback::Feedback;
use lightherder::gpu::Gpu;
use lightherder::input::{Input, Pattern, Source};
use lightherder::params::{Focus, Knob, Limit, Node, Params};
use lightherder::present::{Present, View};
use lightherder::rig::{self, Rig};

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// The surface's own arithmetic, which is what makes a written throw mean
/// the same here as under a hand: a fader moves a continuous knob by the
/// fraction of its travel it moved, scaled by the precision, and banks a
/// count knob up a whole step at a time deaf to it.
struct Board {
    params: Params,
    focus: Focus,
    halvings: u8,
    owed: [f32; Knob::ALL.len()],
    cut: Option<usize>,
    solo: bool,
    played: u64,
}

impl Board {
    fn new(params: Params) -> Board {
        Board {
            params,
            focus: Focus::default(),
            halvings: 2,
            owed: [0.0; Knob::ALL.len()],
            cut: None,
            solo: false,
            played: 0,
        }
    }

    fn turn(&mut self, knob: Knob, throw: f32) {
        let limit = knob.limit(&self.params);
        let moved = throw * limit.travel();
        let paid = match limit {
            Limit::Whole(_) => {
                let owed = &mut self.owed[knob as usize];
                *owed += moved;
                let paid = owed.round();
                *owed -= paid;
                paid
            }
            _ => moved / f32::from(1u8 << self.halvings),
        };
        self.params.nudge(knob, paid, self.focus);
    }
}

fn knob(name: &str) -> Result<Knob, String> {
    let name = name.replace('-', " ");
    Knob::ALL
        .into_iter()
        .find(|k| k.name() == name)
        .ok_or_else(|| format!("no knob is called {name:?}"))
}

fn index(word: Option<&str>, node: Node) -> Result<usize, String> {
    let count = rig::count(node);
    let n: usize = word
        .ok_or_else(|| format!("{} needs a number", node.short()))?
        .parse()
        .map_err(|_| format!("{} takes a number", node.short()))?;
    match (1..=count).contains(&n) {
        true => Ok(n - 1),
        false => Err(format!("the rig has {count} of {}", node.short())),
    }
}

fn axis(word: Option<&str>) -> Result<Axis, String> {
    match word {
        Some("x") => Ok(Axis::X),
        Some("y") => Ok(Axis::Y),
        other => Err(format!("flip takes x or y, not {other:?}")),
    }
}

fn number(word: Option<&str>, what: &str) -> Result<f32, String> {
    word.ok_or_else(|| format!("{what} needs a number"))?
        .parse()
        .map_err(|_| format!("{what} takes a number"))
}

/// `FORMAT:NAME`, which is ffmpeg's `-f` and its `-i`. Split at the first
/// colon and no other: `lavfi:testsrc2=size=640x480:rate=30` is one source.
fn seed(value: &str) -> Result<Input, String> {
    let (format, device) = value
        .split_once(':')
        .ok_or_else(|| format!("seed {value:?} is not FORMAT:NAME"))?;
    match format.is_empty() || device.is_empty() {
        true => Err(format!("seed {value:?} names no source")),
        false => Ok(Input::Capture {
            format: format.into(),
            device: device.into(),
        }),
    }
}

/// Everything a script says before the first pass runs: how big a monitor is
/// and what is plugged into the switcher. The rest of the lines are played.
struct Script {
    resolution: (u32, u32),
    seed: Input,
    lines: Vec<(usize, Vec<String>)>,
}

fn read(path: &Path) -> Result<Script, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut script = Script {
        resolution: (1280, 720),
        seed: Input::Pattern(Pattern::Bars),
        lines: Vec::new(),
    };
    for (n, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        let words: Vec<String> = line.split_whitespace().map(str::to_string).collect();
        let arg = |i: usize| words.get(i).map(String::as_str);
        match arg(0) {
            Some("resolution") => {
                let value = arg(1).ok_or("resolution needs WIDTHxHEIGHT")?;
                let (w, h) = value.split_once('x').ok_or("resolution is WIDTHxHEIGHT")?;
                let side = |s: &str| s.parse::<u32>().map_err(|e| format!("{s}: {e}"));
                script.resolution = (side(w)?, side(h)?);
            }
            Some("seed") => {
                let value = arg(1).ok_or("seed needs FORMAT:NAME")?;
                script.seed = match value {
                    "bars" => Input::Pattern(Pattern::Bars),
                    value => seed(value).map_err(|why| format!("line {}: {why}", n + 1))?,
                };
            }
            _ => script.lines.push((n + 1, words)),
        }
    }
    Ok(script)
}

struct Rig3 {
    gpu: Gpu,
    feedback: Feedback,
    source: Source,
}

fn play(board: &mut Board, rig: &mut Rig3, words: &[String]) -> Result<(), String> {
    let arg = |i: usize| words.get(i).map(String::as_str);
    match arg(0) {
        Some("cam") => board.focus = board.focus.with(Node::Camera, index(arg(1), Node::Camera)?),
        Some("mon") => {
            board.focus = board
                .focus
                .with(Node::Monitor, index(arg(1), Node::Monitor)?)
        }
        Some("sw") => {
            board.focus = board
                .focus
                .with(Node::Switcher, index(arg(1), Node::Switcher)?)
        }
        Some("turn") => {
            let knob = knob(arg(1).ok_or("turn needs a knob")?)?;
            board.turn(knob, number(arg(2), "turn")?);
        }
        Some("finer") => board.halvings = (board.halvings + 1).min(4),
        Some("coarser") => board.halvings = board.halvings.saturating_sub(1),
        Some("select") => {
            board.params.rig.select(board.focus.monitor);
        }
        Some("reverse") => board.params.rig.flip(board.focus.switcher),
        Some("flip") => board.params.monitors[board.focus.monitor].flip(axis(arg(1))?),
        Some("cut") => {
            let switcher = board.focus.switcher;
            board.params.rig.flip(switcher);
            board.cut = Some(switcher);
        }
        Some("release") => {
            if let Some(switcher) = board.cut.take() {
                board.params.rig.flip(switcher);
            }
        }
        Some("blank") => rig.feedback.clear(&rig.gpu.device, &rig.gpu.queue),
        Some("solo") => board.solo = !board.solo,
        Some("run") => {
            for _ in 0..number(arg(1), "run")? as u64 {
                board.played += 1;
                board.params.rig.beat(board.played);
                if let Some(frame) = rig.source.frame() {
                    rig.feedback.write_seed(&rig.gpu.queue, frame);
                }
                rig.feedback
                    .step(&rig.gpu.device, &rig.gpu.queue, &board.params);
            }
        }
        other => return Err(format!("{other:?} is not a control on the board")),
    }
    Ok(())
}

fn main() -> Result<(), String> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let mut args = std::env::args().skip(1);
    let path = PathBuf::from(args.next().ok_or("usage: recipe SCRIPT OUT.png")?);
    let out = PathBuf::from(args.next().ok_or("usage: recipe SCRIPT OUT.png")?);

    let script = read(&path)?;
    let mut params = Rig::IDENTITY.params();
    params.input.source = script.seed.clone();

    let gpu = pollster::block_on(Gpu::open(None, "lightherder recipe"))?;
    let (width, height) = script.resolution;
    lightherder::feedback::bank_fits(&params, script.resolution)?;
    let feedback = Feedback::new(&gpu.device, width, height, &params);
    let present = Present::new(&gpu.device, &feedback, FORMAT);
    let source = pollster::block_on(Source::open(&params.input.source, (width, height)))?;

    let mut board = Board::new(params);
    let mut rig = Rig3 {
        gpu,
        feedback,
        source,
    };
    for (n, words) in &script.lines {
        play(&mut board, &mut rig, words).map_err(|why| format!("line {n}: {why}"))?;
    }

    let dir = out
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let view = match board.solo {
        true => View::Solo(board.focus.monitor),
        false => View::Bank { focus: None },
    };
    let mut capture = Capture::still(&rig.gpu.device, dir, rig.feedback.size(), FORMAT)?;
    capture.frame(
        &rig.gpu.device,
        &rig.gpu.queue,
        &present,
        &rig.feedback,
        view,
        None,
    )?;
    let written = capture.finish()?;
    std::fs::rename(&written, &out).map_err(|e| format!("{}: {e}", out.display()))?;
    println!("{} passes -> {}", board.played, out.display());
    println!("{}", state(&board));
    Ok(())
}

/// Where the eight routing levers and the shaft stand when the recipe ends,
/// and the front panel of every monitor a knob was turned on: the rig-state a
/// written recipe lands on, in the form a reader can check against the code.
fn state(board: &Board) -> String {
    let rig = &board.params.rig;
    let mut out = format!(
        "rig: zoom {:.4} rotation {:+.4}\nswitchers {:.3?} periods {:?}\nselects {:?}",
        board.params.framing.zoom,
        board.params.framing.rotation,
        rig.switchers,
        rig.periods,
        std::array::from_fn::<_, { rig::SELECTS }, _>(|m| match rig.on_program(m) {
            true => "program",
            false => "direct",
        }),
    );
    for m in 0..board.params.monitors.len() {
        let focus = board.focus.with(Node::Monitor, m);
        if board.params.monitors[m] != Default::default() {
            let panel = board.params.describe(focus);
            let line = panel.lines().find(|l| l.starts_with("mon ")).unwrap_or("");
            out += &format!("\n{line}");
        }
    }
    out
}
