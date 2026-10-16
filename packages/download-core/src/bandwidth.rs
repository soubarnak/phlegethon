//! Token-bucket bandwidth limiting with an injectable clock for tests.

use crate::event::CoreEvent;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Live transfer progress reported from body writes (issue #36).
#[derive(Clone)]
pub struct TransferProgress {
    pub job_id: String,
    pub event_tx: crossbeam_channel::Sender<CoreEvent>,
    pub total_bytes: Option<u64>,
    /// Absolute bytes downloaded so far (shared across segments).
    pub live_bytes: Arc<AtomicU64>,
}

/// Monotonic time source for bandwidth refill.
pub trait Clock: Send + Sync {
    fn now(&self) -> Duration;

    /// Block for `duration` of this clock's time. A transfer waiting for
    /// Bandwidth Cap tokens sleeps here, so tests with a manual clock do not
    /// wait in real time.
    fn sleep(&self, duration: Duration);
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

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
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

    /// Sleeping advances the manual clock instead of waiting.
    fn sleep(&self, duration: Duration) {
        self.advance(duration);
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

    pub fn clock(&self) -> &Arc<dyn Clock> {
        &self.clock
    }

    /// How long until `wanted` bytes of tokens are available.
    pub fn wait_for(&mut self, wanted: u64) -> Duration {
        self.refill();
        let missing = wanted as f64 - self.tokens;
        if missing <= 0.0 || self.rate == 0 {
            return Duration::ZERO;
        }
        Duration::from_secs_f64(missing / self.rate as f64).max(Duration::from_millis(1))
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

impl std::fmt::Debug for TokenBucket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenBucket")
            .field("rate", &self.rate)
            .field("tokens", &self.tokens)
            .finish()
    }
}

/// Shared bucket: one global bucket on `Core` and one per capped job.
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
    progress: Option<TransferProgress>,
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
            progress: None,
        }
    }

    pub fn with_progress(mut self, progress: TransferProgress) -> Self {
        self.progress = Some(progress);
        self
    }

    pub fn bytes_written(&self) -> u64 {
        self.written
    }

    pub fn was_paused(&self) -> bool {
        self.paused
    }

    fn emit_progress(&self, delta: u64) {
        let Some(progress) = &self.progress else {
            return;
        };
        let downloaded = progress.live_bytes.fetch_add(delta, Ordering::AcqRel) + delta;
        let _ = progress.event_tx.send(CoreEvent::JobProgress {
            job_id: progress.job_id.clone(),
            bytes_downloaded: downloaded,
            total_bytes: progress.total_bytes,
        });
    }

    /// Take tokens for up to `requested` bytes without waiting.
    fn try_allowed_bytes(&mut self, requested: u64) -> u64 {
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

impl<W: Write> TransferWriter<'_, W> {
    /// Wait until the buckets grant tokens, polling the stop flag.
    ///
    /// Returns 0 only when a pause or Discard was requested (`paused` is set).
    fn wait_allowed_bytes(&mut self, requested: u64) -> u64 {
        // Grant in slices of about 50 ms of the tightest cap, so a tiny cap
        // still writes steadily and a stop request is seen quickly.
        let rates = [&self.global, &self.job]
            .into_iter()
            .flatten()
            .map(|b| b.lock().unwrap().rate());
        let slice = rates.min().map_or(requested, |rate| (rate / 20).max(1));
        let wanted = requested.min(slice);
        loop {
            if self.pause_requested.load(Ordering::Acquire) {
                self.paused = true;
                return 0;
            }
            let allowed = self.try_allowed_bytes(wanted);
            if allowed > 0 {
                return allowed;
            }
            let mut wait = Duration::ZERO;
            let mut clock = None;
            for bucket in [&self.global, &self.job].into_iter().flatten() {
                let mut bucket = bucket.lock().unwrap();
                wait = wait.max(bucket.wait_for(1));
                clock = Some(bucket.clock().clone());
            }
            match clock {
                Some(clock) => clock.sleep(wait.clamp(Duration::from_millis(1), MAX_WAIT_SLICE)),
                None => return wanted,
            }
        }
    }
}

/// Longest single sleep while waiting for tokens, so a pause is noticed fast.
const MAX_WAIT_SLICE: Duration = Duration::from_millis(100);

impl<W: Write> Write for TransferWriter<'_, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let allowed = self.wait_allowed_bytes(buf.len() as u64) as usize;
        if allowed == 0 {
            return Ok(0);
        }
        let n = self.inner.write(&buf[..allowed])?;
        if n < allowed {
            self.refund_unused((allowed - n) as u64);
        }
        self.written += n as u64;
        if n > 0 {
            self.emit_progress(n as u64);
        }
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
