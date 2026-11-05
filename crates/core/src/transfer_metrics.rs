//! Volatile estimates for one transfer, independent of clocks, UI and storage.

use std::{collections::VecDeque, time::Duration};

use crate::DownloadProgress;

const WINDOW: Duration = Duration::from_secs(5);
const MIN_SAMPLE: Duration = Duration::from_millis(250);
const WARMUP: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TransferMetrics {
    /// Effective file bytes per second, not MTProto wire traffic.
    pub bytes_per_second: Option<u64>,
    /// Approximate time to receive the remaining bytes, excluding final publication.
    pub remaining_seconds: Option<u64>,
}

pub struct TransferMeter {
    samples: VecDeque<(Duration, u64)>,
    last: (Duration, u64),
}

impl Default for TransferMeter {
    fn default() -> Self {
        Self {
            samples: VecDeque::from([(Duration::ZERO, 0)]),
            last: (Duration::ZERO, 0),
        }
    }
}

impl TransferMeter {
    /// The adapter supplies elapsed monotonic time, including time without bytes.
    /// Reset if an attempt restarts or the supplied clock moves backwards.
    pub fn observe(&mut self, progress: DownloadProgress, elapsed: Duration) -> TransferMetrics {
        let bytes = progress.downloaded_bytes();
        if elapsed < self.last.0 || bytes < self.last.1 {
            self.samples.clear();
            self.samples.push_back((elapsed, bytes));
            self.last = (elapsed, bytes);
            return TransferMetrics::default();
        }
        self.last = (elapsed, bytes);
        let cutoff = elapsed.saturating_sub(WINDOW);
        while self.samples.len() > 1 && self.samples[1].0 <= cutoff {
            self.samples.pop_front();
        }
        let (start, initial) = self.samples[0];
        if self
            .samples
            .back()
            .is_some_and(|sample| elapsed.saturating_sub(sample.0) >= MIN_SAMPLE)
        {
            self.samples.push_back((elapsed, bytes));
        }
        let span = elapsed.saturating_sub(start);
        if span < WARMUP {
            return TransferMetrics::default();
        }
        let received = u128::from(bytes - initial);
        let rate = received * 1_000_000_000 / span.as_nanos();
        let bytes_per_second = u64::try_from(rate).unwrap_or(u64::MAX);
        let remaining_seconds = progress.total_bytes().and_then(|total| {
            let remaining = total.checked_sub(bytes)?;
            if remaining == 0 || received == 0 || bytes_per_second == 0 {
                return None;
            }
            // Round up, using the unrounded rate. Checked arithmetic also covers
            // synthetic extreme durations/sizes without a panic or bogus ETA.
            let numerator = u128::from(remaining).checked_mul(span.as_nanos())?;
            let denominator = received * 1_000_000_000;
            u64::try_from(numerator.div_ceil(denominator)).ok()
        });
        TransferMetrics {
            bytes_per_second: Some(bytes_per_second),
            remaining_seconds,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InvalidProgress;

    fn sample(
        meter: &mut TransferMeter,
        bytes: u64,
        total: Option<u64>,
        millis: u64,
    ) -> Result<TransferMetrics, InvalidProgress> {
        Ok(meter.observe(
            DownloadProgress::new(bytes, total)?,
            Duration::from_millis(millis),
        ))
    }

    #[test]
    fn warms_up_and_estimates_large_zip_from_recent_bytes() -> Result<(), InvalidProgress> {
        let mut meter = TransferMeter::default();
        assert_eq!(
            sample(&mut meter, 524_288, Some(1_400_000_000), 500)?,
            TransferMetrics::default()
        );
        let metrics = sample(&mut meter, 1_048_576, Some(1_400_000_000), 1000)?;
        assert_eq!(metrics.bytes_per_second, Some(1_048_576));
        assert_eq!(metrics.remaining_seconds, Some(1335));
        Ok(())
    }

    #[test]
    fn rolling_window_includes_stalls_and_recovers_without_a_lifetime_average()
    -> Result<(), InvalidProgress> {
        let mut meter = TransferMeter::default();
        for second in 1..=5 {
            sample(&mut meter, second * 1000, Some(100_000), second * 1000)?;
        }
        let slowing = sample(&mut meter, 5000, Some(100_000), 6000)?;
        assert_eq!(slowing.bytes_per_second, Some(800));
        for second in 7..=10 {
            sample(&mut meter, 5000, Some(100_000), second * 1000)?;
        }
        let stalled = sample(&mut meter, 5000, Some(100_000), 11_000)?;
        assert_eq!(stalled.bytes_per_second, Some(0));
        assert_eq!(stalled.remaining_seconds, None);
        for second in 12..=16 {
            sample(
                &mut meter,
                5000 + (second - 11) * 2000,
                Some(100_000),
                second * 1000,
            )?;
        }
        assert_eq!(
            sample(&mut meter, 17_000, Some(100_000), 17_000)?.bytes_per_second,
            Some(2000)
        );
        Ok(())
    }

    #[test]
    fn unknown_empty_completed_retry_and_clock_reset_have_no_misleading_eta()
    -> Result<(), InvalidProgress> {
        let mut meter = TransferMeter::default();
        let unknown = sample(&mut meter, 100, None, 1000)?;
        assert_eq!(unknown.bytes_per_second, Some(100));
        assert_eq!(unknown.remaining_seconds, None);
        assert_eq!(
            sample(&mut meter, 100, Some(100), 2000)?.remaining_seconds,
            None
        );
        assert_eq!(
            sample(&mut meter, 0, Some(0), 3000)?,
            TransferMetrics::default()
        );
        assert_eq!(
            sample(&mut meter, 0, Some(100), 0)?,
            TransferMetrics::default()
        );
        assert_eq!(
            sample(&mut meter, 50, Some(100), 1000)?.remaining_seconds,
            Some(1)
        );
        Ok(())
    }

    #[test]
    fn extreme_values_do_not_overflow_and_frequent_samples_stay_bounded()
    -> Result<(), InvalidProgress> {
        let mut meter = TransferMeter::default();
        assert_eq!(
            sample(&mut meter, u64::MAX - 1, Some(u64::MAX), 1000)?.remaining_seconds,
            Some(1)
        );
        meter.observe(
            DownloadProgress::new(u64::MAX - 1, Some(u64::MAX))?,
            Duration::MAX,
        );
        let mut meter = TransferMeter::default();
        for millis in 0..20_000 {
            sample(&mut meter, millis, None, millis)?;
        }
        assert!(meter.samples.len() <= 22);
        Ok(())
    }
}
