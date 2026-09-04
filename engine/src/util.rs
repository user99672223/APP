//! Small shared helpers: wall clock, media clock, rate meters, randomness.

use rand::{Rng, RngExt};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn now_secs() -> u64 {
    now_ms() / 1000
}

pub fn random_u64() -> u64 {
    rand::rng().random()
}

pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    rand::rng().fill_bytes(&mut out);
    out
}

/// 32 alphanumeric characters, the ntfy topic of this device.
pub fn random_topic() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut rng = rand::rng();
    (0..proto::consts::NTFY_TOPIC_LEN)
        .map(|_| ALPHABET[rng.random_range(0..ALPHABET.len())] as char)
        .collect()
}

/// Monotonic clock every media timestamp of this device is derived from.
#[derive(Debug, Clone, Copy)]
pub struct MediaClock {
    start: Instant,
}

impl Default for MediaClock {
    fn default() -> Self {
        Self::new()
    }
}

impl MediaClock {
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
        }
    }

    pub fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }

    pub fn now_us(&self) -> u64 {
        self.elapsed().as_micros() as u64
    }

    /// Audio timestamp: 48 kHz samples, wrapping like the wire field.
    pub fn now_samples(&self) -> u32 {
        (self.elapsed().as_micros() * 48 / 1000) as u32
    }
}

/// Bytes (or events) per second over a sliding one-second window.
#[derive(Debug)]
pub struct RateMeter {
    window: Duration,
    samples: std::collections::VecDeque<(Instant, u64)>,
    total: u64,
}

impl Default for RateMeter {
    fn default() -> Self {
        Self::new(Duration::from_secs(1))
    }
}

impl RateMeter {
    pub fn new(window: Duration) -> Self {
        Self {
            window,
            samples: Default::default(),
            total: 0,
        }
    }

    pub fn add(&mut self, amount: u64) {
        self.add_at(Instant::now(), amount);
    }

    pub fn add_at(&mut self, at: Instant, amount: u64) {
        self.samples.push_back((at, amount));
        self.total = self.total.saturating_add(amount);
        self.trim(at);
    }

    fn trim(&mut self, now: Instant) {
        while let Some((t, amount)) = self.samples.front() {
            if now.duration_since(*t) > self.window {
                self.total = self.total.saturating_sub(*amount);
                self.samples.pop_front();
            } else {
                break;
            }
        }
    }

    /// Amount per second, as of `now`.
    pub fn rate(&mut self) -> f64 {
        self.trim(Instant::now());
        self.total as f64 / self.window.as_secs_f64()
    }

    pub fn total(&self) -> u64 {
        self.total
    }
}

/// Runs a future to completion on `runtime` from any thread, including one that is
/// itself inside a tokio runtime (where `Runtime::block_on` would panic).
pub fn block_on_anywhere<F>(runtime: &tokio::runtime::Runtime, fut: F) -> F::Output
where
    F: std::future::Future + Send,
    F::Output: Send,
{
    if tokio::runtime::Handle::try_current().is_err() {
        return runtime.block_on(fut);
    }
    std::thread::scope(|scope| {
        let handle = scope.spawn(|| runtime.block_on(fut));
        match handle.join() {
            Ok(v) => v,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    })
}

/// Owns the engine's tokio runtime and drops it safely from any thread: a runtime
/// dropped inside an async context must not block, so it is shut down in the background.
pub struct RuntimeBox(Option<tokio::runtime::Runtime>);

impl RuntimeBox {
    pub fn new(runtime: tokio::runtime::Runtime) -> Self {
        Self(Some(runtime))
    }
}

impl std::ops::Deref for RuntimeBox {
    type Target = tokio::runtime::Runtime;

    fn deref(&self) -> &Self::Target {
        // The option is only emptied in Drop.
        self.0
            .as_ref()
            .unwrap_or_else(|| unreachable!("runtime taken before drop"))
    }
}

impl Drop for RuntimeBox {
    fn drop(&mut self) {
        if let Some(rt) = self.0.take() {
            if tokio::runtime::Handle::try_current().is_ok() {
                rt.shutdown_background();
            } else {
                drop(rt);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_is_valid() {
        assert!(proto::consts::is_valid_ntfy_topic(&random_topic()));
        assert_ne!(random_topic(), random_topic());
    }

    #[test]
    fn rate_meter_windows() {
        let mut m = RateMeter::new(Duration::from_secs(1));
        let t0 = Instant::now();
        m.add_at(t0, 1000);
        m.add_at(t0 + Duration::from_millis(500), 1000);
        assert_eq!(m.total(), 2000);
        m.add_at(t0 + Duration::from_millis(1400), 10);
        assert_eq!(m.total(), 1010);
    }
}
