/**
 * Bandwidth Cap values for the speed-limit prompt. The Core stores bytes per second;
 * the prompt works in whole KB/s.
 */

/** A cap in bytes per second as whole KB/s, or null when uncapped. Never below 1 KB/s. */
export function capToPromptKb(cap: number | null | undefined): number | null {
  if (cap == null || cap <= 0) return null;
  return Math.max(1, Math.round(cap / 1024));
}
