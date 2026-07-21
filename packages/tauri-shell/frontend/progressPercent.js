/**
 * Determinate progress percent for queue row meta / aria.
 * Clamps at 100 so merged-stream bytes over format size never show >100%.
 * Parity with WinUI ProgressDisplay.DisplayPercent (Tauri rounds to int;
 * WinUI subtitle keeps one decimal).
 *
 * @param {number} bytesDownloaded
 * @param {number|null|undefined} totalBytes
 * @returns {number|null} rounded percent 0–100, or null when indeterminate
 */
export function displayProgressPercent(bytesDownloaded, totalBytes) {
  if (totalBytes == null || totalBytes <= 0) return null;
  const pct = Math.round((bytesDownloaded / totalBytes) * 100);
  return Math.min(100, pct);
}
