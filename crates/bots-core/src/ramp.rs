use crate::config::RampConfig;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct RampWindow {
    pub step_idx: usize,
    pub start: Instant,
    pub end: Instant,
    pub tps: u64,
}

pub struct RampPlanner {
    config: RampConfig,
}

impl RampPlanner {
    pub fn new(config: RampConfig) -> Self {
        Self { config }
    }

    /// Calculate the total duration of all ramp steps in milliseconds
    pub fn total_duration_ms(&self) -> u64 {
        self.config.steps.iter().map(|s| s.hold_ms).sum()
    }

    /// Generate the sequence of ramp windows with absolute timestamps
    pub fn plan(&self, start: Instant) -> Vec<RampWindow> {
        let mut windows = Vec::new();
        let mut current = start;

        for (idx, step) in self.config.steps.iter().enumerate() {
            let duration = Duration::from_millis(step.hold_ms);
            let end = current + duration;

            windows.push(RampWindow {
                step_idx: idx,
                start: current,
                end,
                tps: step.tps,
            });

            current = end;
        }

        windows
    }

    /// Get the current target TPS at a given time
    pub fn current_tps(&self, elapsed_ms: u64) -> Option<u64> {
        let mut cumulative_ms = 0u64;

        for step in &self.config.steps {
            if elapsed_ms < cumulative_ms + step.hold_ms {
                return Some(step.tps);
            }
            cumulative_ms += step.hold_ms;
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RampStep;

    #[test]
    fn test_ramp_planner_total_duration() {
        let config = RampConfig {
            steps: vec![
                RampStep {
                    tps: 1000,
                    hold_ms: 5000,
                },
                RampStep {
                    tps: 2000,
                    hold_ms: 10000,
                },
            ],
        };

        let planner = RampPlanner::new(config);
        assert_eq!(planner.total_duration_ms(), 15000);
    }

    #[test]
    fn test_ramp_planner_plan() {
        let config = RampConfig {
            steps: vec![
                RampStep {
                    tps: 1000,
                    hold_ms: 5000,
                },
                RampStep {
                    tps: 2000,
                    hold_ms: 10000,
                },
            ],
        };

        let planner = RampPlanner::new(config);
        let start = Instant::now();
        let windows = planner.plan(start);

        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].step_idx, 0);
        assert_eq!(windows[0].tps, 1000);
        assert_eq!(windows[1].step_idx, 1);
        assert_eq!(windows[1].tps, 2000);
    }

    #[test]
    fn test_current_tps() {
        let config = RampConfig {
            steps: vec![
                RampStep {
                    tps: 1000,
                    hold_ms: 5000,
                },
                RampStep {
                    tps: 2000,
                    hold_ms: 10000,
                },
            ],
        };

        let planner = RampPlanner::new(config);
        assert_eq!(planner.current_tps(0), Some(1000));
        assert_eq!(planner.current_tps(4999), Some(1000));
        assert_eq!(planner.current_tps(5000), Some(2000));
        assert_eq!(planner.current_tps(14999), Some(2000));
        assert_eq!(planner.current_tps(15000), None);
    }
}
