//! Token-bucket bandwidth limiting with an injectable clock for tests.

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Monotonic time source for bandwidth refill.
pub trait Clock: Send + Sync {
    fn now(&self) -> Duration;
}

/// Production clock backed by `std::time::Instant`.
pub struct SystemClock {
    start: std::time::Instant,
}

impl SystemClock {
    pub fn new() -> Self {
        Self {
            start: std::time::Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now(&self) -> Duration {
        self.start.elapsed()
    }
}

/// Test clock that advances only when told to.
pub struct ManualClock {
    now: Mutex<Duration>,
}

impl ManualClock {
    pub fn new() -> Self {
        Self {
            now: Mutex::new(Duration::ZERO),
        }
    }

    pub fn advance(&self, delta: Duration) {
        let mut now = self.now.lock().unwrap();
        *now += delta;
    }

    pub fn set(&self, value: Duration) {
        let mut now = self.now.lock().unwrap();
        *now = value;
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Duration {
        *self.now.lock().unwrap()
    }
}

/// Effective bytes/sec limit from optional global and per-job caps.
pub fn effective_rate_bytes_per_sec(global: Option<u64>, per_job: Option<u64>) -> Option<u64> {
    match (global, per_job) {
        (Some(g), Some(j)) => Some(g.min(j)),
        (Some(g), None) => Some(g),
        (None, Some(j)) => Some(j),
        (None, None) => None,
    }
}

/// Token bucket enforcing a sustained bytes/sec rate with one second of burst.
pub struct TokenBucket {
    rate: u64,
    tokens: f64,
    last_refill: Duration,
    clock: Arc<dyn Clock>,
}

impl TokenBucket {
    pub fn new(rate_bytes_per_sec: u64, clock: Arc<dyn Clock>) -> Self {
        let burst = rate_bytes_per_sec as f64;
        Self {
            rate: rate_bytes_per_sec,
            tokens: burst,
            last_refill: clock.now(),
            clock,
        }
    }

    pub fn rate(&self) -> u64 {
        self.rate
    }

    fn refill(&mut self) {
        let now = self.clock.now();
        if now <= self.last_refill {
            return;
        }
        let elapsed = now.saturating_sub(self.last_refill);
        let added = elapsed.as_secs_f64() * self.rate as f64;
        let burst = self.rate as f64;
        self.tokens = (self.tokens + added).min(burst);
        self.last_refill = now;
    }

    /// Consume up to `requested` bytes from the bucket.
    pub fn consume(&mut self, requested: u64) -> u64 {
        if requested == 0 {
            return 0;
        }
        self.refill();
        let available = self.tokens.floor() as u64;
        let allowed = available.min(requested);
        self.tokens -= allowed as f64;
        allowed
    }

    /// Return unconsumed tokens after a smaller write than reserved.
    pub fn refund(&mut self, unused: u64) {
        if unused == 0 {
            return;
        }
        let burst = self.rate as f64;
        self.tokens = (self.tokens + unused as f64).min(burst);
    }
}

/// Shared global bucket used across jobs in one scheduling round.
pub type SharedTokenBucket = Arc<Mutex<TokenBucket>>;

pub fn shared_token_bucket(rate_bytes_per_sec: u64, clock: Arc<dyn Clock>) -> SharedTokenBucket {
    Arc::new(Mutex::new(TokenBucket::new(rate_bytes_per_sec, clock)))
}

/// Write sink that enforces bandwidth limits and honors pause requests.
pub struct TransferWriter<'a, W: Write> {
    inner: &'a mut W,
    global: Option<SharedTokenBucket>,
    job: Option<SharedTokenBucket>,
    pause_requested: &'a AtomicBool,
    written: u64,
    paused: bool,
}

impl<'a, W: Write> TransferWriter<'a, W> {
    pub fn new(
        inner: &'a mut W,
        global: Option<SharedTokenBucket>,
        job: Option<SharedTokenBucket>,
        pause_requested: &'a AtomicBool,
    ) -> Self {
        Self {
            inner,
            global,
            job,
            pause_requested,
            written: 0,
            paused: false,
        }
    }

    pub fn bytes_written(&self) -> u64 {
        self.written
    }

    pub fn was_paused(&self) -> bool {
        self.paused
    }

    fn allowed_bytes(&mut self, requested: u64) -> u64 {
        if requested == 0 {
            return 0;
        }
        if self.pause_requested.load(Ordering::Acquire) {
            self.paused = true;
            return 0;
        }

        let mut allowed = requested;
        if let Some(global) = &self.global {
            allowed = global.lock().unwrap().consume(allowed);
        }
        if let Some(job) = &self.job {
            let job_allowed = job.lock().unwrap().consume(allowed);
            if job_allowed < allowed {
                let unused = allowed - job_allowed;
                if let Some(global) = &self.global {
                    global.lock().unwrap().refund(unused);
                }
                allowed = job_allowed;
            }
        }
        allowed
    }

    fn refund_unused(&mut self, unused: u64) {
        if unused == 0 {
            return;
        }
        if let Some(global) = &self.global {
            global.lock().unwrap().refund(unused);
        }
        if let Some(job) = &self.job {
            job.lock().unwrap().refund(unused);
        }
    }
}

impl<W: Write> Write for TransferWriter<'_, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let allowed = self.allowed_bytes(buf.len() as u64) as usize;
        if allowed == 0 {
            return Ok(0);
        }
        let n = self.inner.write(&buf[..allowed])?;
        if n < allowed {
            self.refund_unused((allowed - n) as u64);
        }
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_bucket_refills_with_manual_clock() {
        let clock = Arc::new(ManualClock::new());
        let mut bucket = TokenBucket::new(10, clock.clone());

        assert_eq!(bucket.consume(10), 10);
        assert_eq!(bucket.consume(5), 0);

        clock.advance(Duration::from_secs(1));
        assert_eq!(bucket.consume(10), 10);
        assert_eq!(bucket.consume(1), 0);
    }

    #[test]
    fn effective_rate_is_min_of_global_and_per_job() {
        assert_eq!(effective_rate_bytes_per_sec(Some(100), Some(10)), Some(10));
        assert_eq!(effective_rate_bytes_per_sec(Some(5), Some(100)), Some(5));
        assert_eq!(effective_rate_bytes_per_sec(Some(100), None), Some(100));
        assert_eq!(effective_rate_bytes_per_sec(None, Some(50)), Some(50));
        assert_eq!(effective_rate_bytes_per_sec(None, None), None);
    }
}
