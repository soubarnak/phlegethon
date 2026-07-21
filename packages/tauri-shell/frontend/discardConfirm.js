/**
 * Discard confirmation copy — shared semantics for Platform Shells.
 * Completed: queue-row removal only. Otherwise: delete Job Destination files.
 */

import { t } from "./i18n.js";

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
  const title = t("discard.title");
  const confirmLabel = t("discard.confirm");
  const cancelLabel = t("discard.cancel");

  if (!deletesFiles) {
    return {
      title,
      body: t("discard.completedBody"),
      confirmLabel,
      cancelLabel,
      deletesFiles,
    };
  }

  const path = String(destination ?? "").trim();
  const pathLine = path
    ? t("discard.unfinishedBodyWithPath", { path })
    : t("discard.unfinishedBody");

  return {
    title,
    body: `${pathLine}\n\n${t("discard.cannotUndo")}`,
    confirmLabel,
    cancelLabel,
    deletesFiles,
  };
}
