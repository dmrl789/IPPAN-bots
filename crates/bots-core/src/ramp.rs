use crate::config::{RampConfig, RampStep};

/// Deterministic ramp planner: steps are applied strictly in order,
/// using integer milliseconds and integer TPS only.
#[derive(Debug, Clone)]
pub struct RampPlanner {
    steps: Vec<RampStep>,
}

impl RampPlanner {
    pub fn new(config: RampConfig) -> Self {
        Self {
            steps: config.steps,
        }
    }

    /// Iterate steps strictly in order.
    pub fn steps(&self) -> &[RampStep] {
        &self.steps
    }

    /// Calculate the total duration of all ramp steps in milliseconds.
    pub fn total_duration_ms(&self) -> u64 {
        self.steps.iter().map(|s| s.hold_ms).sum()
    }

    /// Get the current target TPS at a given elapsed time in ms.
    pub fn current_tps(&self, elapsed_ms: u64) -> Option<u64> {
        let mut cumulative_ms = 0u64;
        for step in &self.steps {
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
    fn test_ramp_planner_steps() {
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
        let steps = planner.steps();

        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].tps, 1000);
        assert_eq!(steps[1].tps, 2000);
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
