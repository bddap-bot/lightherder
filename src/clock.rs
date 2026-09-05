use std::time::Duration;

use web_time::Instant;

pub const RATE: f32 = 60.0;

const BACKLOG: Duration = Duration::from_millis(100);

pub struct Clock {
    beat: Duration,
    due: Instant,
}

impl Clock {
    pub fn new(rate: f32) -> Clock {
        Clock {
            beat: Duration::from_secs_f32(1.0 / rate),
            due: Instant::now(),
        }
    }

    pub fn due(&self) -> Instant {
        self.due
    }

    pub fn take_due(&mut self, now: Instant) -> u32 {
        if let Some(oldest) = now.checked_sub(BACKLOG) {
            self.due = self.due.max(oldest);
        }
        let mut passes = 0;
        while self.due <= now {
            self.due += self.beat;
            passes += 1;
        }
        passes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Ran {
        passes: u32,
        presents: u32,
        biggest: u32,
    }

    fn a_second_on(clock: &mut Clock, grid: f32) -> Ran {
        let start = clock.due();
        let slot = Duration::from_secs_f32(1.0 / grid);
        let mut ran = Ran {
            passes: 0,
            presents: 0,
            biggest: 0,
        };
        let mut now = start;
        while now - start < Duration::from_secs(1) {
            let batch = clock.take_due(now);
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
        let mut clock = Clock::new(RATE);
        let start = clock.due();
        let beat = clock.beat;
        assert_eq!(clock.take_due(start), 1);
        assert_eq!(clock.take_due(start + beat / 2), 0);
        assert_eq!(clock.take_due(start + beat), 1);
        assert_eq!(clock.due(), start + beat * 2);
    }

    #[test]
    fn a_stall_runs_what_it_missed_and_drops_what_is_past_the_backlog() {
        let mut clock = Clock::new(RATE);
        let start = clock.due();
        let beat = clock.beat;
        assert_eq!(clock.take_due(start + beat * 3 + beat / 2), 4);

        let mut clock = Clock::new(RATE);
        let start = clock.due();
        let stalled = start + Duration::from_secs(1);
        let owed = clock.take_due(stalled);
        let bound = (BACKLOG.as_secs_f32() * RATE).ceil() as u32 + 1;
        assert!(
            (2..=bound).contains(&owed),
            "{owed} passes for a second gone"
        );
        assert!(clock.due() > stalled, "the clock is still behind");
    }

    #[test]
    fn a_slower_grid_still_gets_sixty_passes_a_second() {
        let ran = a_second_on(&mut Clock::new(RATE), 41.0);
        assert_eq!(ran.passes, 60);
        assert!(ran.presents.abs_diff(41) <= 1, "{} presents", ran.presents);
        assert!(ran.biggest <= 2, "{} passes to one present", ran.biggest);
    }

    #[test]
    fn a_faster_grid_still_gets_sixty_passes_a_second() {
        let ran = a_second_on(&mut Clock::new(RATE), 72.0);
        assert_eq!(ran.passes, 60);
        assert!(ran.presents.abs_diff(72) <= 1, "{} presents", ran.presents);
        assert_eq!(ran.biggest, 1, "a pass ran twice inside one frame");
    }

    #[test]
    fn a_grid_far_under_sixty_keeps_sixty_anyway() {
        let ran = a_second_on(&mut Clock::new(RATE), 15.0);
        assert!(ran.presents.abs_diff(15) <= 1, "{} presents", ran.presents);
        assert_eq!(ran.biggest, 4);
        assert!(
            ran.passes.abs_diff(60) <= ran.biggest,
            "{} passes",
            ran.passes
        );
    }

    #[test]
    fn a_recording_clock_runs_at_its_own_rate() {
        let ran = a_second_on(&mut Clock::new(30.0), 60.0);
        assert!(ran.passes.abs_diff(30) <= 1, "{} frames", ran.passes);
        assert_eq!(ran.biggest, 1);
    }
}
