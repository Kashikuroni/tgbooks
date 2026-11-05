//! Presentation cadence only: the worker keeps measuring at its normal rate.

use std::time::{Duration, Instant};

use tgbooks_core::{DownloadProgress, TransferMetrics};

const SPEED_PERIOD: Duration = Duration::from_secs(10);
const ETA_PERIOD: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct DisplayMetrics {
    pub values: TransferMetrics,
    speed_updated: Option<Instant>,
    eta_updated: Option<Instant>,
}

impl DisplayMetrics {
    /// Leading sample followed by throttling, not trailing-edge debounce:
    /// continuous telemetry must not postpone the next display update forever.
    pub fn update(&mut self, metrics: TransferMetrics, now: Instant) {
        if self
            .speed_updated
            .is_none_or(|at| now.saturating_duration_since(at) >= SPEED_PERIOD)
            // Startup heartbeat zeroes are not a measured transfer speed yet.
            // Arm the throttle only after the first positive sample is shown.
            && (metrics.bytes_per_second.is_some_and(|rate| rate > 0)
                || self.speed_updated.is_some())
        {
            self.values.bytes_per_second = metrics.bytes_per_second;
            self.speed_updated = Some(now);
        }
        if metrics.remaining_seconds.is_none() {
            // Invalid estimates disappear immediately, but do not restart the
            // minute timer; a stalling/recovering transfer must not flicker.
            self.values.remaining_seconds = None;
        } else if self
            .eta_updated
            .is_none_or(|at| now.saturating_duration_since(at) >= ETA_PERIOD)
        {
            self.values.remaining_seconds = metrics.remaining_seconds;
            self.eta_updated = Some(now);
        }
    }
}

pub(super) fn labels(progress: DownloadProgress, metrics: TransferMetrics) -> (String, String) {
    if progress.total_bytes() == Some(progress.downloaded_bytes()) {
        return ("—".into(), "—".into());
    }
    let speed = metrics
        .bytes_per_second
        .map(speed_label)
        .unwrap_or_else(|| "—".into());
    let time = progress
        .total_bytes()
        .and(metrics.remaining_seconds)
        .map(|seconds| format!("{} мин", seconds.div_ceil(60).max(1)))
        .unwrap_or_else(|| "—".into());
    (speed, time)
}

fn speed_label(bytes: u64) -> String {
    const UNITS: [&str; 7] = ["Б/с", "КБ/с", "МБ/с", "ГБ/с", "ТБ/с", "ПБ/с", "ЭБ/с"];
    let bytes = u128::from(bytes);
    let mut divisor = 1;
    let mut unit = 0;
    while unit + 1 < UNITS.len() && bytes >= divisor * 1024 {
        divisor *= 1024;
        unit += 1;
    }
    let mut rounded = (bytes + divisor / 2) / divisor;
    if rounded == 1024 && unit + 1 < UNITS.len() {
        rounded = 1;
        unit += 1;
    }
    format!("{rounded} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speed_and_eta_have_independent_cadences_under_continuous_updates() {
        let start = Instant::now();
        let mut display = DisplayMetrics::default();
        display.update(TransferMetrics::default(), start);
        assert_eq!(display, DisplayMetrics::default());
        for second in 1..=61 {
            display.update(
                TransferMetrics {
                    bytes_per_second: Some(second * 1024),
                    remaining_seconds: Some(6000 - second),
                },
                start + Duration::from_secs(second),
            );
            let speed_sample = 1 + (second - 1) / 10 * 10;
            let time_sample = if second < 61 { 1 } else { 61 };
            assert_eq!(display.values.bytes_per_second, Some(speed_sample * 1024));
            assert_eq!(display.values.remaining_seconds, Some(6000 - time_sample));
        }
    }

    #[test]
    fn startup_zeroes_do_not_delay_first_speed_but_later_zeroes_are_displayed() {
        let start = Instant::now();
        let mut display = DisplayMetrics::default();
        let zero = TransferMetrics {
            bytes_per_second: Some(0),
            remaining_seconds: None,
        };
        for second in 0..=12 {
            display.update(zero, start + Duration::from_secs(second));
            assert_eq!(display, DisplayMetrics::default());
        }
        let first_at = start + Duration::from_millis(12_500);
        let first = TransferMetrics {
            bytes_per_second: Some(409_600),
            remaining_seconds: Some(2760),
        };
        display.update(first, first_at);
        assert_eq!(
            display.values, first,
            "first real speed waited for the startup timer"
        );
        assert_eq!(display.speed_updated, Some(first_at));

        let faster = TransferMetrics {
            bytes_per_second: Some(819_200),
            ..first
        };
        display.update(faster, first_at + SPEED_PERIOD - Duration::from_millis(1));
        assert_eq!(display.values.bytes_per_second, first.bytes_per_second);
        display.update(faster, first_at + SPEED_PERIOD);
        assert_eq!(display.values.bytes_per_second, faster.bytes_per_second);
        display.update(zero, first_at + SPEED_PERIOD * 2);
        assert_eq!(
            display.values.bytes_per_second,
            Some(0),
            "a real stall was hidden"
        );

        let mut retry = DisplayMetrics::default();
        retry.update(zero, first_at + SPEED_PERIOD * 3);
        retry.update(
            first,
            first_at + SPEED_PERIOD * 3 + Duration::from_millis(1),
        );
        assert_eq!(retry.values, first);
    }

    #[test]
    fn unavailable_eta_clears_without_flickering_back_on_recovery() {
        let start = Instant::now();
        let mut display = DisplayMetrics::default();
        let measured = TransferMetrics {
            bytes_per_second: Some(409_600),
            remaining_seconds: Some(2760),
        };
        display.update(measured, start);
        display.update(
            TransferMetrics {
                bytes_per_second: Some(0),
                remaining_seconds: None,
            },
            start + Duration::from_secs(5),
        );
        assert_eq!(display.values.bytes_per_second, measured.bytes_per_second);
        assert_eq!(display.values.remaining_seconds, None);
        display.update(measured, start + Duration::from_secs(6));
        assert_eq!(display.values.remaining_seconds, None);
        display.update(measured, start + Duration::from_secs(60));
        assert_eq!(display.values, measured);
        let mut new_attempt = DisplayMetrics::default();
        new_attempt.update(measured, start + Duration::from_secs(61));
        assert_eq!(new_attempt.values, measured);
    }

    #[test]
    fn labels_are_whole_numbers_with_a_minimum_of_one_minute()
    -> Result<(), tgbooks_core::InvalidProgress> {
        let progress = DownloadProgress::new(100, Some(1_400_000_000))?;
        assert_eq!(
            labels(
                progress,
                TransferMetrics {
                    bytes_per_second: Some(409_600),
                    remaining_seconds: Some(2760)
                }
            ),
            ("400 КБ/с".into(), "46 мин".into())
        );
        for (seconds, minutes) in [
            (0, 1),
            (1, 1),
            (59, 1),
            (60, 1),
            (61, 2),
            (3661, 62),
            (90000, 1500),
        ] {
            assert_eq!(
                labels(
                    progress,
                    TransferMetrics {
                        bytes_per_second: Some(1536),
                        remaining_seconds: Some(seconds)
                    }
                ),
                ("2 КБ/с".into(), format!("{minutes} мин"))
            );
        }
        assert_eq!(speed_label(0), "0 Б/с");
        assert_eq!(speed_label(1_048_575), "1 МБ/с");
        assert_eq!(speed_label(u64::MAX), "16 ЭБ/с");
        assert_eq!(
            labels(progress, TransferMetrics::default()),
            ("—".into(), "—".into())
        );
        assert_eq!(
            labels(
                DownloadProgress::new(100, None)?,
                TransferMetrics {
                    bytes_per_second: Some(1024),
                    remaining_seconds: Some(60)
                }
            ),
            ("1 КБ/с".into(), "—".into())
        );
        assert_eq!(
            labels(
                DownloadProgress::new(100, Some(100))?,
                TransferMetrics {
                    bytes_per_second: Some(1024),
                    remaining_seconds: Some(0)
                }
            ),
            ("—".into(), "—".into())
        );
        assert!(
            !labels(
                progress,
                TransferMetrics {
                    bytes_per_second: Some(u64::MAX),
                    remaining_seconds: Some(u64::MAX)
                }
            )
            .1
            .contains('.')
        );
        Ok(())
    }
}
