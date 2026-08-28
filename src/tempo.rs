//! How often the instrument steps, which is not how often it is shown.
//!
//! The loop evolves one pass per step and every knob compounds once per
//! pass, so the step rate *is* the tempo — see the README. What the display
//! does with those steps is a second clock entirely: the surface is pinned
//! to Fifo, so a present waits for the vertical blank the compositor
//! invents, and that grid is nobody's tempo. Keeping the two apart is the
//! whole of this module: passes fall due here on the wall clock, and a
//! present goes out after whichever batch of them ran.
//!
//! It buys the two things one clock could not have at once. A display grid
//! *slower* than the tempo no longer holds the piece back — the passes it
//! did not show still ran, several to one present (#16, where a nested
//! gamescope handing out 41-46 Hz played the piece a third slow). And a
//! tempo the performer moves is now a thing the instrument can have at all,
//! since it no longer has to be the display's number.

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

/// One press of the tempo keys, as a ratio: the fourth root of two, so four
/// presses halve or double the tempo exactly. A ratio rather than a fixed
/// number of hertz because five a second is the whole range at the bottom
/// and a rounding error at the top.
pub const STEP_UP: f32 = 1.189_207_1;
pub const STEP_DOWN: f32 = 1.0 / STEP_UP;

/// The slowest display grid at which the tempo is still met exactly, which
/// is what bounds the catch-up a single present may run: `rate / FLOOR`
/// passes and no more. Something has to bound it — a machine that cannot
/// make the rate must fall behind rather than owe an ever-growing backlog —
/// and bounding it by the tempo rather than by a number of passes is what
/// keeps the bound honest at both ends: at sixty a present runs at most two
/// passes, so a struggling machine batches barely at all, and at the top of
/// the range the ceiling is still reachable on a 30 Hz screen.
///
/// Below this grid the piece plays slow, and says so: the rate line prints
/// the passes that ran against the tempo asked for.
const FLOOR: f32 = 30.0;

/// The tempo, and when its next pass falls due.
pub struct Tempo {
    rate: f32,
    due: Instant,
}

impl Tempo {
    /// `rate` is clamped to the range this module allows. The first pass is
    /// due at once: the instrument starts playing on the frame it opens.
    pub fn new(rate: f32) -> Tempo {
        Tempo {
            rate: clamped(rate),
            due: Instant::now(),
        }
    }

    /// Passes a second, as the rate line and the readout print it.
    pub fn rate(&self) -> f32 {
        self.rate
    }

    /// When the next pass falls due — what the run loop waits until.
    pub fn next(&self) -> Instant {
        self.due
    }

    /// Take the tempo `ratio` times what it is.
    ///
    /// The deadline already standing is kept, but never further off than one
    /// new beat: without that, speeding the instrument up out of a slow tempo
    /// would be heard at the end of the long beat it interrupted rather than
    /// on the press.
    pub fn scale(&mut self, ratio: f32, now: Instant) {
        self.rate = clamped(self.rate * ratio);
        self.due = self.due.min(now + self.beat());
    }

    /// How many passes fall due by `now`, taking them off the clock.
    ///
    /// Deadlines are absolute — each is the last one plus a beat, never "now
    /// plus a beat" — so a wake-up that comes late does not push the tempo
    /// out with it. Taking the display's grid for the tempo instead is what
    /// played the piece a fifth fast under the TV's nested gamescope (#11).
    ///
    /// Missed deadlines are *run* rather than dropped, up to [`FLOOR`]'s
    /// bound: a present that arrives late owes the piece the passes it did
    /// not show, and running them is what keeps the loop on the wall clock
    /// instead of at whatever rate the display path grants. Past the bound
    /// the backlog is dropped rather than owed — a machine that cannot keep
    /// up must not then run the passes it missed in an ever-growing batch.
    pub fn passes_due(&mut self, now: Instant) -> u32 {
        let beat = self.beat();
        let cap = (self.rate / FLOOR).ceil().max(1.0) as u32;
        let mut passes = 0;
        while self.due <= now && passes < cap {
            self.due += beat;
            passes += 1;
        }
        if self.due <= now {
            self.due = now + beat;
        }
        passes
    }

    fn beat(&self) -> Duration {
        Duration::from_secs_f32(1.0 / self.rate)
    }
}

/// `rate` inside the allowed range. Clamped and not refused, because the
/// place a typed rate is answered is [`crate::cli`], which says no out loud;
/// this is the backstop under `Duration::from_secs_f32`, which panics on a
/// rate of zero's reciprocal or on one that is not a number at all.
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
        /// The most passes any one present ran, which is what [`FLOOR`]
        /// bounds.
        biggest: u32,
    }

    /// A second of the run loop against a display grid of `grid` hertz. The
    /// loop waits for its own deadline, runs whatever fell due, and then —
    /// presenting under Fifo — waits for the next slot the swapchain will
    /// hand it. Every instant is worked out from `start`, so what these
    /// tests assert does not depend on how fast the machine running them is.
    ///
    /// The second it covers ends at a present, so the passes still owed
    /// inside the last one are not counted: `passes` is short of the tempo
    /// by up to one batch, which is why the counts below are asserted to a
    /// batch rather than to the pass.
    fn a_second_on(tempo: &mut Tempo, grid: f32) -> Ran {
        let start = Instant::now();
        let slot = Duration::from_secs_f32(1.0 / grid);
        let mut ran = Ran {
            passes: 0,
            presents: 0,
            biggest: 0,
        };
        let mut now = start;
        while now - start < Duration::from_secs(1) {
            now = now.max(tempo.next());
            if now - start >= Duration::from_secs(1) {
                break;
            }
            let batch = tempo.passes_due(now);
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
        let start = tempo.next();
        let beat = tempo.beat();
        assert_eq!(tempo.passes_due(start), 1);
        // Half a beat later nothing is due: the deadline decides when a pass
        // happens, and a redraw asked for early is not one.
        assert_eq!(tempo.passes_due(start + beat / 2), 0);
        // Landing exactly on the deadline is on time, not late.
        assert_eq!(tempo.passes_due(start + beat), 1);
        assert_eq!(tempo.next(), start + beat * 2);
    }

    #[test]
    fn a_stall_runs_what_it_missed_and_drops_the_rest() {
        let mut tempo = Tempo::new(60.0);
        let start = tempo.next();
        let beat = tempo.beat();
        // Three deadlines and a half went by. Two of them are run — a
        // present that arrived late owes the piece the passes it did not
        // show — and the rest is dropped rather than owed, so the next
        // deadline is a beat off `now` and not four beats off `start`.
        assert_eq!(tempo.passes_due(start + beat * 3 + beat / 2), 2);
        assert_eq!(tempo.next(), start + beat * 3 + beat / 2 + beat);
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
        assert_eq!(ran.biggest, 2);
    }

    #[test]
    fn a_faster_grid_still_gets_sixty_passes_a_second() {
        // The other side, and the bug the absolute deadlines exist for
        // (#11): a grid faster than the tempo — the same gamescope hands out
        // about 72 Hz unfocused — must not run one pass per slot.
        let ran = a_second_on(&mut Tempo::new(60.0), 72.0);
        assert_eq!(ran.passes, 60);
        // Nothing new to show between two passes, so the piece is presented
        // at its own rate rather than at the grid's.
        assert_eq!(ran.presents, 60);
        assert_eq!(ran.biggest, 1);
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
    fn a_grid_under_the_floor_plays_slow_rather_than_batching() {
        // A path slower than [`FLOOR`], where the tempo cannot be met: the
        // catch-up is bounded, so the piece falls behind rather than running
        // an ever-growing batch to keep up. Fifteen presents of two passes,
        // not fifteen of four.
        let ran = a_second_on(&mut Tempo::new(60.0), 15.0);
        assert!(ran.presents.abs_diff(15) <= 1, "{} presents", ran.presents);
        assert_eq!(ran.biggest, 2);
        assert!(ran.passes <= ran.presents * 2, "{} passes", ran.passes);
        // Half the tempo, and nothing like the sixty a met tempo leaves.
        assert!(ran.passes >= ran.presents * 2 - 1, "{} passes", ran.passes);
    }

    #[test]
    fn the_steps_halve_and_double_the_tempo() {
        let now = Instant::now();
        let mut tempo = Tempo::new(DEFAULT_RATE);
        for _ in 0..4 {
            tempo.scale(STEP_DOWN, now);
        }
        assert!(
            (tempo.rate() - DEFAULT_RATE / 2.0).abs() < 1e-3,
            "{}",
            tempo.rate()
        );
        for _ in 0..8 {
            tempo.scale(STEP_UP, now);
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
            tempo.scale(STEP_DOWN, now);
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
        // Asking for sixty must not wait that second out.
        let mut tempo = Tempo::new(MIN_RATE);
        let now = tempo.next();
        assert_eq!(tempo.passes_due(now), 1);
        assert_eq!(tempo.next(), now + Duration::from_secs(1));
        tempo.scale(60.0, now);
        assert_eq!(tempo.rate(), 60.0);
        assert_eq!(tempo.next(), now + tempo.beat());
    }
}
