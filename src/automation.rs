use std::collections::VecDeque;
use std::fmt;

use crate::params::{Cadence, Focus, Knob, Node, Params};

const LONGEST: usize = 10 * 60 * Cadence::SECOND as usize;

pub(crate) struct Lane {
    knob: Knob,
    monitor: usize,
    values: VecDeque<f32>,
}

impl Lane {
    fn focus(&self) -> Focus {
        Focus::default().with(Node::Monitor, self.monitor)
    }

    fn moved(&self) -> bool {
        self.values.iter().any(|value| *value != self.values[0])
    }
}

#[derive(Default)]
pub(crate) enum Automation {
    #[default]
    Off,
    Recording(Vec<Lane>),
    Looping {
        lanes: Vec<Lane>,
        next: usize,
    },
}

impl Automation {
    pub(crate) fn press(&mut self) {
        *self = match std::mem::take(self) {
            Automation::Recording(lanes) => Automation::close(lanes),
            Automation::Off | Automation::Looping { .. } => Automation::Recording(
                (0..crate::rig::MONITORS)
                    .flat_map(|monitor| {
                        Knob::ALL
                            .into_iter()
                            .filter(|knob| knob.node() == Node::Monitor && *knob != Knob::Height)
                            .map(move |knob| Lane {
                                knob,
                                monitor,
                                values: VecDeque::new(),
                            })
                    })
                    .collect(),
            ),
        };
    }

    fn close(mut lanes: Vec<Lane>) -> Automation {
        lanes.retain(Lane::moved);
        match lanes.is_empty() {
            true => Automation::Off,
            false => Automation::Looping { lanes, next: 0 },
        }
    }

    pub(crate) fn pass(&mut self, params: &mut Params) {
        match self {
            Automation::Off => {}
            Automation::Recording(lanes) => {
                for lane in lanes.iter_mut() {
                    if lane.values.len() == LONGEST {
                        lane.values.pop_front();
                    }
                    lane.values.push_back(params.knob(lane.knob, lane.focus()));
                }
            }
            Automation::Looping { lanes, next } => {
                for lane in lanes.iter() {
                    params.set(lane.knob, lane.values[*next], lane.focus());
                }
                *next = (*next + 1) % lanes[0].values.len();
            }
        }
    }

    pub(crate) fn take_back(&mut self, knob: Knob, focus: Focus) {
        if let Automation::Looping { lanes, .. } = self {
            lanes.retain(|lane| lane.knob != knob || lane.monitor != focus.monitor);
            if lanes.is_empty() {
                *self = Automation::Off;
            }
        }
    }

    pub(crate) fn armed(&self) -> bool {
        matches!(self, Automation::Recording(_))
    }
}

impl fmt::Display for Automation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Automation::Off => write!(f, "off"),
            Automation::Recording(_) => write!(f, "recording the monitor knobs"),
            Automation::Looping { lanes, .. } => write!(
                f,
                "looping {} of the monitor knobs over {} passes",
                lanes.len(),
                lanes[0].values.len()
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(monitor: usize) -> Focus {
        Focus::default().with(Node::Monitor, monitor)
    }

    fn reading(params: &Params, knobs: &[(Knob, usize)]) -> Vec<f32> {
        knobs
            .iter()
            .map(|&(knob, monitor)| params.knob(knob, at(monitor)))
            .collect()
    }

    fn looped(automation: &Automation) -> bool {
        matches!(automation, Automation::Looping { .. })
    }

    #[test]
    fn a_take_loops_the_monitor_knobs_that_moved_pass_for_pass_and_nothing_else() {
        let mut params = crate::config::instrument();
        let mut automation = Automation::default();
        let tracked = [(Knob::Brightness, 1), (Knob::Temperature, 3)];
        automation.press();
        assert!(automation.armed());
        let mut taken = Vec::new();
        for pass in 0..5 {
            params.nudge(Knob::Brightness, -0.05, at(1));
            if pass == 2 {
                params.nudge(Knob::Temperature, 1.0, at(3));
                params.nudge(Knob::Height, 0.1, at(3));
                params.nudge(Knob::Slide, 0.1, Focus::default());
            }
            automation.pass(&mut params);
            taken.push(reading(&params, &tracked));
        }
        assert_eq!(taken[0], [-0.05, 0.0]);
        assert_eq!(taken[4], [-0.25, 1.0]);
        automation.press();
        assert!(!automation.armed());
        assert_eq!(
            automation.to_string(),
            "looping 2 of the monitor knobs over 5 passes"
        );
        params.nudge(Knob::Contrast, 0.5, at(1));
        let hand = params.clone();
        for pass in 0..12 {
            automation.pass(&mut params);
            assert_eq!(reading(&params, &tracked), taken[pass % 5], "pass {pass}");
            let mut want = hand.clone();
            for (&(knob, monitor), value) in tracked.iter().zip(&taken[pass % 5]) {
                want.set(knob, *value, at(monitor));
            }
            assert_eq!(params, want, "pass {pass} moved an untracked knob");
        }
    }

    #[test]
    fn a_take_in_which_nothing_moved_loops_nothing() {
        let mut params = crate::config::instrument();
        let mut automation = Automation::default();
        automation.press();
        for _ in 0..3 {
            automation.pass(&mut params);
        }
        automation.press();
        assert!(matches!(automation, Automation::Off));
        automation.press();
        automation.press();
        assert!(matches!(automation, Automation::Off), "a take of no passes");
    }

    #[test]
    fn a_dip_that_comes_back_to_where_it_started_still_loops() {
        let mut params = crate::config::instrument();
        let mut automation = Automation::default();
        automation.press();
        automation.pass(&mut params);
        params.nudge(Knob::Saturation, -0.5, at(0));
        automation.pass(&mut params);
        params.nudge(Knob::Saturation, 0.5, at(0));
        automation.pass(&mut params);
        automation.press();
        assert!(looped(&automation));
        automation.pass(&mut params);
        automation.pass(&mut params);
        assert_eq!(params.knob(Knob::Saturation, at(0)), 0.5);
    }

    #[test]
    fn a_press_under_the_loop_stops_it_where_it_stands_and_records_afresh() {
        let mut params = crate::config::instrument();
        let mut automation = Automation::default();
        automation.press();
        for _ in 0..4 {
            params.nudge(Knob::Brightness, 0.1, at(2));
            automation.pass(&mut params);
        }
        automation.press();
        automation.pass(&mut params);
        automation.pass(&mut params);
        let stopped = params.clone();
        automation.press();
        assert!(automation.armed());
        for _ in 0..3 {
            automation.pass(&mut params);
        }
        assert_eq!(params, stopped);
        automation.press();
        assert!(matches!(automation, Automation::Off));
    }

    #[test]
    fn a_hand_on_a_looping_knob_takes_back_that_knob_on_that_monitor_alone() {
        let mut params = crate::config::instrument();
        let mut automation = Automation::default();
        let tracked = [(Knob::Brightness, 1), (Knob::Brightness, 4), (Knob::Hue, 1)];
        automation.press();
        let mut taken = Vec::new();
        for _ in 0..3 {
            for &(knob, monitor) in &tracked {
                params.nudge(knob, 0.1, at(monitor));
            }
            automation.pass(&mut params);
            taken.push(reading(&params, &tracked));
        }
        automation.press();
        automation.take_back(Knob::Brightness, at(1));
        automation.take_back(Knob::Slide, at(4));
        params.set(Knob::Brightness, -0.3, at(1));
        for pass in 0..6 {
            automation.pass(&mut params);
            let now = reading(&params, &tracked);
            assert_eq!(now[0], -0.3, "pass {pass}");
            assert_eq!(now[1], taken[pass % 3][1], "pass {pass}");
            assert!((now[2] - taken[pass % 3][2]).abs() < 1e-6, "pass {pass}");
        }
        automation.take_back(Knob::Brightness, at(4));
        automation.take_back(Knob::Hue, at(1));
        assert!(matches!(automation, Automation::Off));
    }

    #[test]
    fn a_take_keeps_its_last_ten_minutes_and_stays_armed() {
        let ten_minutes = 10 * 60 * Cadence::SECOND as usize;
        let mut params = crate::config::instrument();
        let mut automation = Automation::default();
        automation.press();
        params.nudge(Knob::Hue, 1.0, at(0));
        automation.pass(&mut params);
        params.nudge(Knob::Hue, 1.0, at(0));
        for _ in 1..ten_minutes {
            automation.pass(&mut params);
        }
        params.nudge(Knob::Sharpness, 1.0, at(0));
        automation.pass(&mut params);
        assert!(automation.armed());
        automation.press();
        assert_eq!(
            automation.to_string(),
            format!("looping 1 of the monitor knobs over {ten_minutes} passes")
        );
        automation.pass(&mut params);
        assert_eq!(params.knob(Knob::Sharpness, at(0)), 0.0);
    }
}
