/**
 * Discard confirmation copy — shared semantics for Platform Shells.
 * Completed: queue-row removal only. Otherwise: delete Job Destination files.
 */

/**
 * @param {string} status Core/UI job status
 * @returns {boolean}
 */
export function discardsDeletesFiles(status) {
  return String(status ?? "").toLowerCase() !== "completed";
}

/**
 * @param {string} status
 * @param {string} [destination]
 * @returns {{ title: string, body: string, confirmLabel: string, cancelLabel: string, deletesFiles: boolean }}
 */
export function buildDiscardConfirmCopy(status, destination = "") {
  const deletesFiles = discardsDeletesFiles(status);
  const title = "Discard job?";
  const confirmLabel = "Discard";
  const cancelLabel = "Cancel";

  if (!deletesFiles) {
    return {
      title,
      body: "This removes the job from the queue only. Files on disk will not be deleted.",
      confirmLabel,
      cancelLabel,
      deletesFiles,
    };
  }

  const path = String(destination ?? "").trim();
  const pathLine = path
    ? `Files under the Job Destination will be deleted:\n\n${path}`
    : "Files under the Job Destination will be deleted.";

  return {
    title,
    body: `${pathLine}\n\nThis cannot be undone.`,
    confirmLabel,
    cancelLabel,
    deletesFiles,
  };
}
