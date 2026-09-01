//! How often the instrument steps, which is not how often it is shown.
//!
//! The loop evolves one pass at a time and every knob compounds once per
//! pass, so the step rate *is* the tempo — see the README. What the display
//! does with those steps is a second clock: the surface is pinned to Fifo,
//! so a present waits for the vertical blank the compositor invents, and
//! that grid is nobody's tempo. Passes fall due here on the wall clock, and
//! the run loop presents on every blank whether or not one fell due — which
//! is what lets the rate be a control at all. A tempo under the grid shows
//! the same bank twice; a tempo over it runs several passes to a present;
//! neither moves the piece off the wall clock, and the glass stays at vsync
//! either way.
//!
//! The one thing it will not do is play slower than asked to keep a display
//! happy. A grid slower than the tempo runs the passes it did not show
//! (#16, where a nested gamescope handing out 41-46 Hz played the piece a
//! third slow); a window with no blank at all — minimised, or a surface gone
//! stale — keeps evolving on the deadline below, because the piece is not
//! the picture.
//!
//! The rate is not part of a graph: a graph is the rig, and how fast the
//! piece plays is the performer's, kept for as long as the run.

use std::time::Duration;

use web_time::Instant;

/// Passes a second unless `--rate` says otherwise: the tempo every graph
/// that ships was drawn at, and what the per-pass constants in
/// [`crate::params`] were chosen against.
pub const DEFAULT_RATE: f32 = 60.0;

/// The slowest tempo. A pass a second — slow enough to watch a loop evolve
/// one step at a time, which is the low end anyone would ask for.
pub const MIN_RATE: f32 = 1.0;

/// The fastest. Four times the usual sixty, well past any display grid,
/// which is the point: the tempo is no longer the display's number, so the
/// ceiling is what the piece can use rather than what a screen can show.
pub const MAX_RATE: f32 = 240.0;

/// What one press multiplies the rate by: the fourth root of two, so four
/// presses halve or double the tempo exactly. A ratio and not a fixed number
/// of hertz, because five a second is the whole range at the bottom of it
/// and a rounding error at the top.
const PER_PRESS: f32 = 1.189_207_1;

/// How far behind the tempo may fall before it stops owing the passes it
/// missed. Something must bound it, or a machine that cannot make the rate
/// runs a batch that grows every present until it is doing nothing else.
///
/// A span rather than a number of passes, so it means the same thing at
/// every tempo: a tenth of a second is six passes at sixty and twenty-four
/// at the top of the range. Wide enough that no display grid worth calling
/// one trips it — ten frames a second still leaves the tempo intact — and
/// narrow enough that a machine which genuinely cannot keep up judders
/// rather than stalling. What falls past it is dropped, not owed, and the
/// rate line says so.
const BACKLOG: Duration = Duration::from_millis(100);

/// Which way a press moves the rate. Named rather than passed as the ratio
/// itself: "times 1.19" and "sixty passes a second" are the same shape of
/// argument, and a table of bindings is exactly where the two would be
/// confused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Slower,
    Faster,
}

/// The tempo, and when its next pass falls due.
pub struct Tempo {
    rate: f32,
    due: Instant,
}

impl Tempo {
    /// `rate` is clamped to the range this module allows.
    pub fn new(rate: f32) -> Tempo {
        Tempo {
            rate: clamped(rate),
            due: Instant::now(),
        }
    }

    /// Passes a second, as the rate line prints it.
    pub fn rate(&self) -> f32 {
        self.rate
    }

    /// Play from now, dropping the deadline that stood before.
    pub fn start(&mut self) {
        self.due = Instant::now();
    }

    /// When the next pass falls due. Read only when there is no present to
    /// pace the loop — see [`crate::app`], where a frame that went out asks
    /// for the next one instead.
    pub fn due(&self) -> Instant {
        self.due
    }

    /// Move the rate one press.
    ///
    /// The deadline already standing is kept, but never further off than one
    /// new beat: without that, speeding the instrument up out of a slow
    /// tempo would be heard at the end of the long beat it interrupted
    /// rather than on the press. Slowing down keeps the nearer deadline, so
    /// one last pass lands at the old spacing — a beat, not a lurch.
    pub fn step(&mut self, step: Step, now: Instant) {
        self.rate = clamped(match step {
            Step::Faster => self.rate * PER_PRESS,
            Step::Slower => self.rate / PER_PRESS,
        });
        self.due = self.due.min(now + self.beat());
    }

    /// How many passes fall due by `now`, taking them off the clock. Zero is
    /// the ordinary answer when the piece plays slower than the display.
    ///
    /// Deadlines are absolute — each is the last one plus a beat, never "now
    /// plus a beat" — so a present that came late does not push the tempo out
    /// with it. Taking the display's grid for the tempo instead is what
    /// played the piece a fifth fast under the TV's nested gamescope (#11).
    ///
    /// Missed deadlines are *run* rather than dropped, back to [`BACKLOG`]: a
    /// present that arrives late owes the piece the passes it did not show,
    /// and running them is what keeps the loop on the wall clock instead of
    /// at whatever rate the display path grants.
    pub fn take_due(&mut self, now: Instant) -> u32 {
        // Anything older than the backlog is given up on rather than owed.
        // `checked_sub` because a monotonic clock younger than the span is a
        // panic and not a tempo: nothing is stale on a clock that new.
        if let Some(oldest) = now.checked_sub(BACKLOG) {
            self.due = self.due.max(oldest);
        }
        let beat = self.beat();
        let mut passes = 0;
        while self.due <= now {
            self.due += beat;
            passes += 1;
        }
        passes
    }

    fn beat(&self) -> Duration {
        Duration::from_secs_f32(1.0 / self.rate)
    }
}

/// `rate` inside the allowed range. Clamped and not refused, because the
/// place a typed rate is answered is [`crate::cli`], which says no out loud;
/// this is the backstop under `Duration::from_secs_f32`, which panics on the
/// reciprocal of zero and on a rate that is not a number at all.
fn clamped(rate: f32) -> f32 {
    if rate.is_nan() {
        return MIN_RATE;
    }
    rate.clamp(MIN_RATE, MAX_RATE)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a second of the run loop did.
    struct Ran {
        passes: u32,
        presents: u32,
        /// The most passes any one present ran, which is what [`BACKLOG`]
        /// bounds.
        biggest: u32,
    }

    /// A second of the run loop against a display grid of `grid` hertz. The
    /// loop runs whatever fell due and presents, and the present — Fifo —
    /// waits for the next slot the swapchain will hand it, which is the only
    /// thing pacing the loop. Every instant is worked out from `start`, so
    /// what these tests assert does not depend on how fast the machine
    /// running them is.
    ///
    /// The second it covers ends at a present, so passes still owed inside
    /// the last one are not counted: the totals below run a batch short of
    /// the tempo, and are asserted to that.
    fn a_second_on(tempo: &mut Tempo, grid: f32) -> Ran {
        // The tempo's own deadline and not `Instant::now()`: the gap between
        // building one and reading the clock again is real machine time, and
        // a test whose first batch is two passes on a loaded machine and one
        // on an idle one asserts the machine rather than the tempo.
        let start = tempo.due();
        let slot = Duration::from_secs_f32(1.0 / grid);
        let mut ran = Ran {
            passes: 0,
            presents: 0,
            biggest: 0,
        };
        let mut now = start;
        while now - start < Duration::from_secs(1) {
            let batch = tempo.take_due(now);
            ran.passes += batch;
            ran.biggest = ran.biggest.max(batch);
            ran.presents += 1;
            let slots = ((now - start).as_nanos() / slot.as_nanos()) as u32 + 1;
            now = start + slot * slots;
        }
        ran
    }

    #[test]
    fn an_on_time_pass_owes_one_beat() {
        let mut tempo = Tempo::new(60.0);
        let start = tempo.due();
        let beat = tempo.beat();
        assert_eq!(tempo.take_due(start), 1);
        // Half a beat later nothing is due: the deadline decides when a pass
        // happens, so a present in between shows the bank again rather than
        // stepping it.
        assert_eq!(tempo.take_due(start + beat / 2), 0);
        // Landing exactly on the deadline is on time, not late.
        assert_eq!(tempo.take_due(start + beat), 1);
        assert_eq!(tempo.due(), start + beat * 2);
    }

    #[test]
    fn a_stall_runs_what_it_missed_and_drops_what_is_past_the_backlog() {
        let mut tempo = Tempo::new(60.0);
        let start = tempo.due();
        let beat = tempo.beat();
        // Three beats and a half went by, so four deadlines fell — nought,
        // one, two and three — all inside the backlog. A present that
        // arrived late owes the piece every one of them.
        assert_eq!(tempo.take_due(start + beat * 3 + beat / 2), 4);

        // A whole second gone is past the backlog, so what it owes is the
        // backlog's worth and not the second's: sixty passes run back to
        // back would be a lurch, not a repair.
        let mut tempo = Tempo::new(60.0);
        let start = tempo.due();
        let stalled = start + Duration::from_secs(1);
        let owed = tempo.take_due(stalled);
        let bound = (BACKLOG.as_secs_f32() * tempo.rate()).ceil() as u32 + 1;
        assert!(
            (2..=bound).contains(&owed),
            "{owed} passes for a second gone"
        );
        assert!(tempo.due() > stalled, "the clock is still behind");
    }

    #[test]
    fn a_slower_grid_still_gets_sixty_passes_a_second() {
        // The whole of #16: a display path that hands out fewer frames than
        // the tempo — the nested gamescope the TV runs measured 41 — must
        // still leave sixty passes in a second, several to a present, rather
        // than playing the piece at the grid's rate.
        let ran = a_second_on(&mut Tempo::new(60.0), 41.0);
        assert_eq!(ran.passes, 60);
        assert!(ran.presents.abs_diff(41) <= 1, "{} presents", ran.presents);
        assert!(ran.biggest <= 2, "{} passes to one present", ran.biggest);
    }

    #[test]
    fn a_faster_grid_still_gets_sixty_passes_a_second() {
        // The other side, and the bug the absolute deadlines exist for
        // (#11): a grid faster than the tempo — the same gamescope hands out
        // about 72 Hz unfocused — must not run one pass per slot.
        let ran = a_second_on(&mut Tempo::new(60.0), 72.0);
        assert_eq!(ran.passes, 60);
        // And the display keeps its own clock: every blank is presented,
        // twelve of them showing the bank a second time.
        assert!(ran.presents.abs_diff(72) <= 1, "{} presents", ran.presents);
        assert_eq!(ran.biggest, 1, "a pass ran twice inside one frame");
    }

    #[test]
    fn a_slow_tempo_still_presents_at_the_grid() {
        // The requirement, the way round that is easy to lose: the
        // piece plays at one pass a second and the glass is still at vsync.
        let ran = a_second_on(&mut Tempo::new(1.0), 60.0);
        assert!(ran.presents.abs_diff(60) <= 1, "{} presents", ran.presents);
        assert_eq!(ran.passes, 1);
    }

    #[test]
    fn a_tempo_over_the_grid_runs_several_passes_to_a_present() {
        // What the decoupling buys at the top of the range: the tempo is no
        // longer bounded by the display's number. Four passes to a present
        // at 240 over a 60 Hz grid, and the second's own last batch is the
        // whole of what the count is short by.
        let ran = a_second_on(&mut Tempo::new(240.0), 60.0);
        assert_eq!(ran.biggest, 4);
        assert!(
            ran.passes.abs_diff(240) <= ran.biggest,
            "{} passes",
            ran.passes
        );
        assert!(ran.presents.abs_diff(60) <= 1, "{} presents", ran.presents);
    }

    #[test]
    fn a_grid_far_under_the_tempo_keeps_the_tempo_anyway() {
        // A 15 Hz path is four beats to a present, inside the backlog, so
        // the piece plays at sixty on a display showing a quarter of it.
        // The bound is on how far behind the *clock* falls, not on how few
        // passes a slow display may be handed.
        let ran = a_second_on(&mut Tempo::new(60.0), 15.0);
        assert!(ran.presents.abs_diff(15) <= 1, "{} presents", ran.presents);
        assert_eq!(ran.biggest, 4);
        assert!(
            ran.passes.abs_diff(60) <= ran.biggest,
            "{} passes",
            ran.passes
        );
    }

    #[test]
    fn the_steps_halve_and_double_the_tempo() {
        let now = Instant::now();
        let mut tempo = Tempo::new(DEFAULT_RATE);
        for _ in 0..4 {
            tempo.step(Step::Slower, now);
        }
        assert!(
            (tempo.rate() - DEFAULT_RATE / 2.0).abs() < 1e-3,
            "{}",
            tempo.rate()
        );
        for _ in 0..8 {
            tempo.step(Step::Faster, now);
        }
        assert!(
            (tempo.rate() - DEFAULT_RATE * 2.0).abs() < 1e-3,
            "{}",
            tempo.rate()
        );
    }

    #[test]
    fn the_tempo_stays_inside_its_range() {
        let now = Instant::now();
        let mut tempo = Tempo::new(MAX_RATE * 10.0);
        assert_eq!(tempo.rate(), MAX_RATE);
        for _ in 0..100 {
            tempo.step(Step::Slower, now);
        }
        assert_eq!(tempo.rate(), MIN_RATE);
        // A rate that is not a number would panic `from_secs_f32`, so it is
        // the floor instead — and the floor is a tempo, not a stopped
        // instrument.
        assert_eq!(Tempo::new(f32::NAN).rate(), MIN_RATE);
    }

    #[test]
    fn speeding_up_is_heard_on_the_press() {
        // Out of the slowest tempo, the deadline standing is a second away.
        // A press must not wait that second out.
        let mut tempo = Tempo::new(MIN_RATE);
        let now = tempo.due();
        assert_eq!(tempo.take_due(now), 1);
        assert_eq!(tempo.due(), now + Duration::from_secs(1));
        tempo.step(Step::Faster, now);
        assert_eq!(tempo.due(), now + tempo.beat());
        // And the other way the deadline is left where it stands, so the
        // last pass of the old tempo lands on the old spacing.
        let standing = tempo.due();
        tempo.step(Step::Slower, now);
        assert_eq!(tempo.due(), standing);
    }
}
