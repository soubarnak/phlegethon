/**
 * Determinate progress percent for queue rows.
 * Clamps at 100 so merged-stream bytes over the format size never show >100%.
 */
export function displayProgressPercent(
  bytesDownloaded: number,
  totalBytes: number | null | undefined,
): number | null {
  if (totalBytes == null || totalBytes <= 0) return null;
  const pct = Math.round((bytesDownloaded / totalBytes) * 100);
  return Math.min(100, pct);
}

const UNITS = ["B", "KB", "MB", "GB", "TB"] as const;

/** Human-readable byte count (`12.3 MB`). */
export function formatBytes(bytes: number | null | undefined): string {
  if (bytes == null || !Number.isFinite(bytes) || bytes < 0) return "—";
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < UNITS.length - 1) {
    value /= 1024;
    unit += 1;
  }
  const digits = unit === 0 ? 0 : value >= 100 ? 0 : 1;
  return `${value.toFixed(digits)} ${UNITS[unit]}`;
}

/** `2.1 MB/s`, or an empty string when the rate is unknown. */
export function formatRate(bytesPerSecond: number | null | undefined): string {
  if (bytesPerSecond == null || !Number.isFinite(bytesPerSecond) || bytesPerSecond <= 0) return "";
  return `${formatBytes(bytesPerSecond)}/s`;
}

/** Exponential moving average of transfer speed from byte samples. */
export class SpeedEstimator {
  private lastBytes: number | null = null;
  private lastAt: number | null = null;
  private ema: number | null = null;

  constructor(private readonly alpha = 0.3) {}

  /** Feed a cumulative byte count sampled at `nowMs`; returns bytes/s or null. */
  sample(bytes: number, nowMs: number): number | null {
    if (this.lastBytes != null && this.lastAt != null) {
      const dt = (nowMs - this.lastAt) / 1000;
      if (dt > 0) {
        const delta = bytes - this.lastBytes;
        if (delta < 0) {
          // Restart from zero (retry / segmented restart): reset the window.
          this.ema = null;
        } else {
          const instant = delta / dt;
          this.ema = this.ema == null ? instant : this.alpha * instant + (1 - this.alpha) * this.ema;
        }
      }
    }
    this.lastBytes = bytes;
    this.lastAt = nowMs;
    return this.ema;
  }

  reset(): void {
    this.lastBytes = null;
    this.lastAt = null;
    this.ema = null;
  }
}

/** Remaining time estimate in seconds, or null when unknown. */
export function estimateEtaSeconds(
  bytesDownloaded: number,
  totalBytes: number | null | undefined,
  bytesPerSecond: number | null | undefined,
): number | null {
  if (totalBytes == null || totalBytes <= 0 || !bytesPerSecond || bytesPerSecond <= 0) return null;
  const remaining = Math.max(0, totalBytes - bytesDownloaded);
  return remaining / bytesPerSecond;
}

/** `1h 02m`, `3m 05s`, `12s`. */
export function formatEta(seconds: number | null | undefined): string {
  if (seconds == null || !Number.isFinite(seconds) || seconds < 0) return "";
  const s = Math.round(seconds);
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ${String(s % 60).padStart(2, "0")}s`;
  const h = Math.floor(m / 60);
  return `${h}h ${String(m % 60).padStart(2, "0")}m`;
}
