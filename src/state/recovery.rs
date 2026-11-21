//! Service-rate episodes use receipt time. Sampling completed seconds makes the
//! result independent of repaint frequency and avoids scanning the window per arrival.
use super::{BIN_MS, COMPARISON_WINDOW_MS, Intake, Recovery, WINDOW_MS, rising_rate};
use std::collections::{BTreeMap, VecDeque};

const SAMPLE_MS: u64 = 1_000;
const MARKER_MS: u64 = 30_000;

#[derive(Debug)]
enum Episode {
    Noisy {
        start: u64,
        baseline: u64,
        baseline_ms: u64,
        quiet_since: Option<u64>,
    },
    Recovered {
        confirmed: u64,
        duration_ms: u64,
    },
}

#[derive(Debug, Default)]
pub(super) struct Recoveries {
    // An episode needs traffic in the last 5s to remain noisy. Confirming
    // recovery and displaying its marker together take less than the intake
    // window, so every key is backed by a retained unit name. Capacity loss
    // clears the table together with the evidence for its rate comparisons.
    episodes: BTreeMap<Box<str>, Episode>,
}

impl Recoveries {
    pub(super) fn clear(&mut self) {
        self.episodes.clear();
    }

    pub(super) fn marker(&self, unit: &str, now: u64) -> Option<Recovery> {
        match self.episodes.get(unit) {
            Some(Episode::Recovered {
                duration_ms,
                confirmed,
            }) => Some(Recovery {
                duration_ms: *duration_ms,
                age_ms: now - (confirmed - BIN_MS),
            }),
            _ => None,
        }
    }

    pub(super) fn advance(
        &mut self,
        from: u64,
        to: u64,
        intake: &VecDeque<Intake>,
        last_drop: Option<u64>,
    ) {
        if from / SAMPLE_MS == to / SAMPLE_MS {
            return;
        }
        // No arrivals between clock updates. After a whole quiet minute all
        // episodes have ended and their markers have expired, even on a huge jump.
        if to - from >= WINDOW_MS || intake.is_empty() {
            self.clear();
            return;
        }
        for second in from / SAMPLE_MS + 1..=to / SAMPLE_MS {
            let end = second * SAMPLE_MS;
            if last_drop.is_none_or(|at| end - at > COMPARISON_WINDOW_MS) {
                self.sample(end, intake);
            }
        }
    }

    fn sample(&mut self, end: u64, intake: &VecDeque<Intake>) {
        let baseline_ms = end.saturating_sub(BIN_MS).min(WINDOW_MS);
        let mut counts = BTreeMap::<&str, (u64, u64)>::new();
        for entry in intake.iter().rev() {
            let age = end - entry.at;
            if age > COMPARISON_WINDOW_MS {
                break;
            }
            let (recent, baseline) = counts.entry(&entry.event.unit).or_default();
            if age <= BIN_MS {
                *recent += entry.count;
            } else {
                *baseline += entry.count;
            }
        }

        self.episodes.retain(|unit, episode| match episode {
            Episode::Noisy {
                start,
                baseline,
                baseline_ms,
                quiet_since,
            } => {
                let recent = counts.get(unit.as_ref()).map_or(0, |(n, _)| *n);
                if u128::from(recent) * u128::from(*baseline_ms)
                    <= u128::from(*baseline) * u128::from(BIN_MS)
                {
                    let quiet_start = *quiet_since.get_or_insert(end);
                    if end - quiet_start >= BIN_MS {
                        *episode = Episode::Recovered {
                            confirmed: end,
                            duration_ms: quiet_start - *start,
                        };
                    }
                } else {
                    *quiet_since = None;
                }
                true
            }
            Episode::Recovered { confirmed, .. } => end - *confirmed < MARKER_MS,
        });
        for (unit, (recent, baseline)) in counts {
            if rising_rate(recent, baseline, baseline_ms)
                && !matches!(self.episodes.get(unit), Some(Episode::Noisy { .. }))
            {
                self.episodes.insert(
                    unit.into(),
                    Episode::Noisy {
                        start: end,
                        baseline,
                        baseline_ms,
                        quiet_since: None,
                    },
                );
            }
        }
    }
}
