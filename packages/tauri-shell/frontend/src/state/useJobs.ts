/**
 * Queue state: Core snapshots merged with pushed progress events, speed
 * estimates, and the process_pending tick that keeps the Core dispatching.
 */
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api } from "@/tauri/api";
import { onCoreEvent } from "@/tauri/events";
import type { CoreEvent, JobSnapshot } from "@/tauri/types";
import { SpeedEstimator } from "@/lib/progress";

export interface JobRow extends JobSnapshot {
  /** Smoothed bytes/s while downloading; null otherwise. */
  speed_bps: number | null;
}

/** A terminal Core event, with a unique id so consumers can key effects on it. */
export interface QueuedEvent {
  id: number;
  event: Extract<CoreEvent, { type: "JobCompleted" | "JobFailed" }>;
}

export interface JobsState {
  jobs: JobRow[];
  loading: boolean;
  error: string | null;
  refresh: () => Promise<void>;
  /** Kick the Core dispatcher (after enqueue / resume / retry). */
  kick: () => Promise<void>;
  /**
   * Completion and failure events, oldest first, none dropped. A consumer handles
   * every entry, then calls `dismissEvents` with the last id it handled.
   */
  events: QueuedEvent[];
  dismissEvents: (upToId: number) => void;
}

const TICK_MS = 500;
/** Progress events arrive per write; the queue view applies the latest one at this rate. */
export const PROGRESS_FLUSH_MS = 200;

type Progress = Extract<CoreEvent, { type: "JobProgress" }>;

function sameSnapshot(row: JobSnapshot, snap: JobSnapshot): boolean {
  return (Object.keys(snap) as Array<keyof JobSnapshot>).every((key) => row[key] === snap[key]);
}

export function useJobs(): JobsState {
  const [jobs, setJobs] = useState<JobRow[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [events, setEvents] = useState<QueuedEvent[]>([]);
  const speeds = useRef(new Map<string, SpeedEstimator>());
  const refreshing = useRef(false);
  const pendingProgress = useRef(new Map<string, Progress>());
  const flushTimer = useRef<number | null>(null);
  const nextEventId = useRef(1);

  const refresh = useCallback(async () => {
    if (refreshing.current) return;
    refreshing.current = true;
    try {
      const snapshots = await api.listJobs();
      setJobs((prev) => {
        const prevById = new Map(prev.map((j) => [j.job_id, j]));
        const liveIds = new Set(snapshots.map((snap) => snap.job_id));
        // Forget estimators and buffered progress of jobs that are gone (L-54).
        for (const id of [...speeds.current.keys()]) if (!liveIds.has(id)) speeds.current.delete(id);
        for (const id of [...pendingProgress.current.keys()]) if (!liveIds.has(id)) pendingProgress.current.delete(id);
        return snapshots.map((snap) => {
          const old = prevById.get(snap.job_id);
          const speed = snap.status === "downloading" ? (old?.speed_bps ?? null) : null;
          if (snap.status !== "downloading") speeds.current.delete(snap.job_id);
          // Keep the old object when nothing changed, so memoized rows skip rendering.
          if (old && old.speed_bps === speed && sameSnapshot(old, snap)) return old;
          return { ...snap, speed_bps: speed };
        });
      });
      setError(null);
    } catch (e) {
      setError(String(e));
    } finally {
      refreshing.current = false;
      setLoading(false);
    }
  }, []);

  const kick = useCallback(async () => {
    try {
      await api.processPending();
    } catch (e) {
      setError(String(e));
    }
  }, []);

  const dismissEvents = useCallback((upToId: number) => {
    setEvents((queue) => queue.filter((entry) => entry.id > upToId));
  }, []);

  const flushProgress = useCallback(() => {
    flushTimer.current = null;
    const batch = pendingProgress.current;
    if (batch.size === 0) return;
    pendingProgress.current = new Map();
    const now = Date.now();
    setJobs((prev) => {
      let changed = false;
      const next = prev.map((job) => {
        const event = batch.get(job.job_id);
        if (!event) return job;
        changed = true;
        let est = speeds.current.get(job.job_id);
        if (!est) {
          est = new SpeedEstimator();
          speeds.current.set(job.job_id, est);
        }
        const speed = est.sample(event.bytes_downloaded, now);
        return {
          ...job,
          status: job.status === "pending" ? ("downloading" as const) : job.status,
          bytes_downloaded: event.bytes_downloaded,
          total_bytes: event.total_bytes ?? job.total_bytes,
          speed_bps: speed,
        };
      });
      return changed ? next : prev;
    });
  }, []);

  // Push events from the Core. Progress is coalesced per job; everything else is a state change.
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    onCoreEvent((event) => {
      if (event.type === "JobProgress") {
        pendingProgress.current.set(event.job_id, event);
        if (flushTimer.current === null) {
          flushTimer.current = window.setTimeout(flushProgress, PROGRESS_FLUSH_MS);
        }
        return;
      }
      if (event.type === "JobsReordered") {
        // Queue order changed: the snapshot carries the new order.
        void refresh();
        return;
      }
      pendingProgress.current.delete(event.job_id);
      if (event.type === "JobCompleted" || event.type === "JobFailed") {
        const id = nextEventId.current++;
        setEvents((queue) => [...queue, { id, event }]);
      }
      // Any state change: re-read the authoritative snapshot.
      void refresh();
    })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch(() => {
        /* not in Tauri (tests / browser) */
      });
    return () => {
      cancelled = true;
      unlisten?.();
      if (flushTimer.current !== null) {
        window.clearTimeout(flushTimer.current);
        flushTimer.current = null;
      }
    };
  }, [refresh, flushProgress]);

  // Initial load + dispatcher tick while anything is pending/downloading.
  useEffect(() => {
    void refresh();
  }, [refresh]);

  const hasActive = useMemo(
    () => jobs.some((j) => j.status === "pending" || j.status === "downloading"),
    [jobs],
  );

  useEffect(() => {
    if (!hasActive) return;
    const id = window.setInterval(() => {
      void kick();
      void refresh();
    }, TICK_MS);
    return () => window.clearInterval(id);
  }, [hasActive, kick, refresh]);

  return { jobs, loading, error, refresh, kick, events, dismissEvents };
}
