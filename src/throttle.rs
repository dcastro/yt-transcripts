//! Delays, jitter, batch pauses, backoff and Ctrl-C handling (PLAN.md, section 7).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The short pause between the requests for one video is in this range.
const STEP_PAUSE_MIN: Duration = Duration::from_secs(1);
const STEP_PAUSE_MAX: Duration = Duration::from_secs(3);
/// The first backoff after a transient error. Each next backoff is two times longer.
const BACKOFF_BASE: Duration = Duration::from_secs(30);
/// The number of retries after a transient error.
pub(crate) const MAX_TRANSIENT_RETRIES: u32 = 3;
/// The number of cool-downs after HTTP 429 in one run.
pub(crate) const MAX_COOLDOWNS: u32 = 3;
/// Stop after this number of unexpected responses in sequence.
pub(crate) const MAX_UNEXPECTED_IN_SEQUENCE: u32 = 5;

#[derive(Debug, Clone)]
pub(crate) struct ThrottleSettings {
    pub(crate) delay: Duration,
    /// The random change of the delay, as a fraction from 0 to 1.
    pub(crate) jitter: f64,
    /// The number of videos before a batch pause. 0 means no batch pause.
    pub(crate) batch_size: usize,
    pub(crate) batch_pause: Duration,
    pub(crate) cooldown: Duration,
}

/// Calculates each wait. The runner does the waits with a `Sleeper`.
#[derive(Debug)]
pub(crate) struct Throttle {
    settings: ThrottleSettings,
    rng: SplitMix64,
}

impl Throttle {
    pub(crate) fn new(settings: ThrottleSettings) -> Self {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |time| time.as_nanos() as u64);
        Self::with_seed(settings, seed)
    }

    pub(crate) fn with_seed(settings: ThrottleSettings, seed: u64) -> Self {
        Self {
            settings,
            rng: SplitMix64(seed),
        }
    }

    /// The wait before video number `index` (from 0) of this run. There is no wait before the
    /// first video. Before each `batch_size` videos, the wait includes the batch pause.
    pub(crate) fn before_video(&mut self, index: usize) -> Duration {
        if index == 0 {
            return Duration::ZERO;
        }
        let wait = self.jittered(self.settings.delay);
        let batch = self.settings.batch_size;
        if batch > 0 && index.is_multiple_of(batch) {
            wait + self.settings.batch_pause
        } else {
            wait
        }
    }

    /// The short pause between the requests for one video.
    pub(crate) fn step_pause(&mut self) -> Duration {
        let range = STEP_PAUSE_MAX - STEP_PAUSE_MIN;
        STEP_PAUSE_MIN + range.mul_f64(self.rng.next_f64())
    }

    /// The wait before retry number `retry` (from 0) after a transient error.
    pub(crate) fn backoff(&mut self, retry: u32) -> Duration {
        self.jittered(BACKOFF_BASE * 2u32.saturating_pow(retry))
    }

    /// The wait after HTTP 429: the `Retry-After` value if YouTube gives it, or the cool-down.
    pub(crate) fn cooldown(&self, retry_after: Option<Duration>) -> Duration {
        retry_after.unwrap_or(self.settings.cooldown)
    }

    fn jittered(&mut self, base: Duration) -> Duration {
        let jitter = self.settings.jitter.clamp(0.0, 1.0);
        let factor = 1.0 + jitter * (2.0 * self.rng.next_f64() - 1.0);
        base.mul_f64(factor)
    }
}

/// A small pseudo-random generator. The jitter does not need a better one.
#[derive(Debug)]
struct SplitMix64(u64);

impl SplitMix64 {
    /// A value from 0 (included) to 1 (not included).
    fn next_f64(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (z >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// The user stopped the run with Ctrl-C.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Interrupted;

/// Does the waits. Tests use a fake sleeper, so that they run fast.
pub(crate) trait Sleeper {
    /// A long wait. Ctrl-C stops it.
    fn sleep(&mut self, duration: Duration) -> Result<(), Interrupted>;
    /// A short pause inside one video. Ctrl-C does not stop it, so that the current video can
    /// finish.
    fn pause(&mut self, duration: Duration);
}

/// The real sleeper. It also handles Ctrl-C.
#[derive(Debug)]
pub(crate) struct RealSleeper {
    stop: Arc<AtomicBool>,
}

impl RealSleeper {
    /// Install the Ctrl-C handler. The first Ctrl-C stops the run after the current video. The
    /// second Ctrl-C stops the program immediately.
    pub(crate) fn install() -> Result<Self, ctrlc::Error> {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        ctrlc::set_handler(move || {
            if flag.swap(true, Ordering::SeqCst) {
                std::process::exit(130);
            }
            tracing::warn!(
                "Stopping after the current video. Press Ctrl-C again to stop immediately"
            );
        })?;
        Ok(Self { stop })
    }

    fn is_interrupted(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }
}

impl Sleeper for RealSleeper {
    fn sleep(&mut self, duration: Duration) -> Result<(), Interrupted> {
        const SLICE: Duration = Duration::from_millis(100);
        let end = Instant::now() + duration;
        loop {
            if self.is_interrupted() {
                return Err(Interrupted);
            }
            let now = Instant::now();
            if now >= end {
                return Ok(());
            }
            thread::sleep(SLICE.min(end - now));
        }
    }

    fn pause(&mut self, duration: Duration) {
        thread::sleep(duration);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{Throttle, ThrottleSettings};

    fn settings(jitter: f64, batch_size: usize) -> ThrottleSettings {
        ThrottleSettings {
            delay: Duration::from_secs(10),
            jitter,
            batch_size,
            batch_pause: Duration::from_secs(300),
            cooldown: Duration::from_secs(600),
        }
    }

    #[test]
    fn no_wait_before_the_first_video() {
        let mut throttle = Throttle::with_seed(settings(0.5, 50), 1);
        assert_eq!(throttle.before_video(0), Duration::ZERO);
    }

    #[test]
    fn delay_with_jitter_stays_in_range() {
        let mut throttle = Throttle::with_seed(settings(0.5, 0), 7);
        let waits: Vec<Duration> = (1..1000)
            .map(|index| throttle.before_video(index))
            .collect();
        let min = waits.iter().min().unwrap();
        let max = waits.iter().max().unwrap();
        assert!(*min >= Duration::from_secs(5), "min {min:?}");
        assert!(*max <= Duration::from_secs(15), "max {max:?}");
        assert!(*min < Duration::from_secs(6) && *max > Duration::from_secs(14));
    }

    #[test]
    fn batch_pause_before_each_batch() {
        let mut throttle = Throttle::with_seed(settings(0.0, 2), 1);
        let waits: Vec<u64> = (0..5)
            .map(|index| throttle.before_video(index).as_secs())
            .collect();
        assert_eq!(waits, [0, 10, 310, 10, 310]);
    }

    #[test]
    fn backoff_doubles() {
        let mut throttle = Throttle::with_seed(settings(0.0, 0), 1);
        let waits: Vec<u64> = (0..3)
            .map(|retry| throttle.backoff(retry).as_secs())
            .collect();
        assert_eq!(waits, [30, 60, 120]);
    }

    #[test]
    fn cooldown_uses_retry_after() {
        let throttle = Throttle::with_seed(settings(0.0, 0), 1);
        assert_eq!(throttle.cooldown(None), Duration::from_secs(600));
        assert_eq!(
            throttle.cooldown(Some(Duration::from_secs(90))),
            Duration::from_secs(90)
        );
    }

    #[test]
    fn step_pause_is_one_to_three_seconds() {
        let mut throttle = Throttle::with_seed(settings(0.0, 0), 3);
        for _ in 0..1000 {
            let pause = throttle.step_pause();
            assert!(pause >= Duration::from_secs(1) && pause <= Duration::from_secs(3));
        }
    }
}
