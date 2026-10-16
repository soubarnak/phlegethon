//! Rate limiting for `JobProgress` events (H-25).
//!
//! A transfer reports progress for every 8 KB it writes: about 1,200 events a
//! second at 10 MB/s, each one a webview render. The coalescer lets at most one
//! progress event per job through per interval and keeps the newest one for the
//! next flush. Every other event passes straight through, so lifecycle events are
//! never delayed. A pending progress event is flushed before a job's terminal event
//! so the last byte count is not lost, and dropped when the job was discarded.

use crate::EventSummary;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// About 5 Hz per job.
pub const PROGRESS_INTERVAL: Duration = Duration::from_millis(200);

pub struct ProgressCoalescer {
    interval: Duration,
    last_emit: HashMap<String, Instant>,
    pending: HashMap<String, EventSummary>,
}

impl ProgressCoalescer {
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            last_emit: HashMap::new(),
            pending: HashMap::new(),
        }
    }

    /// Events to emit now for `event`, in order.
    pub fn push(&mut self, event: EventSummary, now: Instant) -> Vec<EventSummary> {
        if let EventSummary::JobProgress { job_id, .. } = &event {
            let job_id = job_id.clone();
            let due = self
                .last_emit
                .get(&job_id)
                .map_or(true, |at| now.duration_since(*at) >= self.interval);
            if due {
                self.pending.remove(&job_id);
                self.last_emit.insert(job_id, now);
                return vec![event];
            }
            self.pending.insert(job_id, event);
            return Vec::new();
        }

        let mut out = Vec::new();
        if let Some(job_id) = event.job_id().map(str::to_string) {
            if let Some(progress) = self.pending.remove(&job_id) {
                if !matches!(event, EventSummary::JobDiscarded { .. }) {
                    out.push(progress);
                }
            }
            if event.is_terminal() {
                self.last_emit.remove(&job_id);
            }
        }
        out.push(event);
        out
    }

    /// Pending progress events whose interval has passed.
    pub fn flush_due(&mut self, now: Instant) -> Vec<EventSummary> {
        let due: Vec<String> = self
            .pending
            .keys()
            .filter(|job_id| {
                self.last_emit
                    .get(*job_id)
                    .map_or(true, |at| now.duration_since(*at) >= self.interval)
            })
            .cloned()
            .collect();
        due.into_iter()
            .filter_map(|job_id| {
                self.last_emit.insert(job_id.clone(), now);
                self.pending.remove(&job_id)
            })
            .collect()
    }

    /// Every pending progress event, regardless of the interval (shutdown).
    pub fn flush_all(&mut self) -> Vec<EventSummary> {
        self.pending.drain().map(|(_, event)| event).collect()
    }

    /// How long until the oldest pending event may be flushed; `None` when nothing waits.
    pub fn next_flush_in(&self, now: Instant) -> Option<Duration> {
        self.pending
            .keys()
            .map(|job_id| match self.last_emit.get(job_id) {
                Some(at) => self.interval.saturating_sub(now.duration_since(*at)),
                None => Duration::ZERO,
            })
            .min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn progress(job: &str, bytes: u64) -> EventSummary {
        EventSummary::JobProgress {
            job_id: job.into(),
            bytes_downloaded: bytes,
            total_bytes: Some(1000),
        }
    }

    fn completed(job: &str) -> EventSummary {
        EventSummary::JobCompleted {
            job_id: job.into(),
            bytes_written: 1000,
            path: "/x".into(),
        }
    }

    const STEP: Duration = Duration::from_millis(200);

    #[test]
    fn a_burst_of_progress_becomes_one_event_per_interval() {
        let mut c = ProgressCoalescer::new(STEP);
        let t0 = Instant::now();
        let mut emitted = Vec::new();
        for i in 0..1200u64 {
            // 1,200 events inside one second, as at 10 MB/s with 8 KB writes.
            let now = t0 + Duration::from_micros(i * 800);
            emitted.extend(c.push(progress("a", i), now));
            emitted.extend(c.flush_due(now));
        }
        assert!(
            emitted.len() <= 6,
            "expected about 5 events for one second, got {}",
            emitted.len()
        );
        assert_eq!(emitted.first(), Some(&progress("a", 0)));
    }

    #[test]
    fn the_newest_progress_wins_when_the_interval_ends() {
        let mut c = ProgressCoalescer::new(STEP);
        let t0 = Instant::now();
        assert_eq!(c.push(progress("a", 1), t0).len(), 1);
        assert!(c
            .push(progress("a", 2), t0 + Duration::from_millis(10))
            .is_empty());
        assert!(c
            .push(progress("a", 3), t0 + Duration::from_millis(20))
            .is_empty());
        assert!(c.flush_due(t0 + Duration::from_millis(100)).is_empty());
        assert_eq!(c.flush_due(t0 + STEP), vec![progress("a", 3)]);
    }

    #[test]
    fn jobs_are_limited_independently() {
        let mut c = ProgressCoalescer::new(STEP);
        let t0 = Instant::now();
        assert_eq!(c.push(progress("a", 1), t0).len(), 1);
        assert_eq!(c.push(progress("b", 1), t0).len(), 1);
    }

    #[test]
    fn pending_progress_is_flushed_before_the_terminal_event() {
        let mut c = ProgressCoalescer::new(STEP);
        let t0 = Instant::now();
        c.push(progress("a", 1), t0);
        c.push(progress("a", 999), t0 + Duration::from_millis(5));
        let out = c.push(completed("a"), t0 + Duration::from_millis(6));
        assert_eq!(out, vec![progress("a", 999), completed("a")]);
        // Nothing left to emit after completion.
        assert!(c.flush_due(t0 + Duration::from_secs(5)).is_empty());
    }

    #[test]
    fn pending_progress_of_a_discarded_job_is_dropped() {
        let mut c = ProgressCoalescer::new(STEP);
        let t0 = Instant::now();
        c.push(progress("a", 1), t0);
        c.push(progress("a", 2), t0 + Duration::from_millis(5));
        let out = c.push(
            EventSummary::JobDiscarded { job_id: "a".into() },
            t0 + Duration::from_millis(6),
        );
        assert_eq!(out, vec![EventSummary::JobDiscarded { job_id: "a".into() }]);
        assert!(c.flush_all().is_empty());
    }

    #[test]
    fn lifecycle_events_are_never_delayed() {
        let mut c = ProgressCoalescer::new(STEP);
        let t0 = Instant::now();
        let paused = EventSummary::JobPaused { job_id: "a".into() };
        assert_eq!(c.push(paused.clone(), t0), vec![paused.clone()]);
        assert_eq!(c.push(paused.clone(), t0), vec![paused]);
    }

    #[test]
    fn a_finished_job_does_not_keep_a_rate_limit_entry() {
        let mut c = ProgressCoalescer::new(STEP);
        let t0 = Instant::now();
        c.push(progress("a", 1), t0);
        c.push(completed("a"), t0 + Duration::from_millis(1));
        // The same id coming back (retry) starts a fresh interval.
        assert_eq!(
            c.push(progress("a", 1), t0 + Duration::from_millis(2))
                .len(),
            1
        );
    }

    #[test]
    fn next_flush_in_reports_the_remaining_interval() {
        let mut c = ProgressCoalescer::new(STEP);
        let t0 = Instant::now();
        assert_eq!(c.next_flush_in(t0), None);
        c.push(progress("a", 1), t0);
        c.push(progress("a", 2), t0 + Duration::from_millis(50));
        assert_eq!(
            c.next_flush_in(t0 + Duration::from_millis(50)),
            Some(Duration::from_millis(150))
        );
    }
}
