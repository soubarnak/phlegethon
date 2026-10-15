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

export interface JobsState {
  jobs: JobRow[];
  loading: boolean;
  error: string | null;
  refresh: () => Promise<void>;
  /** Kick the Core dispatcher (after enqueue / resume / retry). */
  kick: () => Promise<void>;
  /** Most recent events, for consumers that react to completion (notifications, history). */
  lastEvent: CoreEvent | null;
}

const TICK_MS = 500;

export function useJobs(): JobsState {
  const [jobs, setJobs] = useState<JobRow[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [lastEvent, setLastEvent] = useState<CoreEvent | null>(null);
  const speeds = useRef(new Map<string, SpeedEstimator>());
  const refreshing = useRef(false);

  const refresh = useCallback(async () => {
    if (refreshing.current) return;
    refreshing.current = true;
    try {
      const snapshots = await api.listJobs();
      setJobs((prev) => {
        const prevById = new Map(prev.map((j) => [j.job_id, j]));
        return snapshots.map((snap) => {
          const old = prevById.get(snap.job_id);
          const speed = snap.status === "downloading" ? (old?.speed_bps ?? null) : null;
          if (snap.status !== "downloading") speeds.current.delete(snap.job_id);
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

  // Push events from the Core.
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    onCoreEvent((event) => {
      setLastEvent(event);
      if (event.type === "JobProgress") {
        const now = Date.now();
        setJobs((prev) =>
          prev.map((job) => {
            if (job.job_id !== event.job_id) return job;
            let est = speeds.current.get(job.job_id);
            if (!est) {
              est = new SpeedEstimator();
              speeds.current.set(job.job_id, est);
            }
            const speed = est.sample(event.bytes_downloaded, now);
            return {
              ...job,
              status: job.status === "pending" ? "downloading" : job.status,
              bytes_downloaded: event.bytes_downloaded,
              total_bytes: event.total_bytes ?? job.total_bytes,
              speed_bps: speed,
            };
          }),
        );
        return;
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
    };
  }, [refresh]);

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

  return { jobs, loading, error, refresh, kick, lastEvent };
}
