/**
 * Discard confirmation copy — shared semantics with the Core.
 * Completed: queue-row removal only. Otherwise: delete Job Destination files.
 */
import { t } from "./i18n";

export interface DiscardConfirmCopy {
  title: string;
  body: string;
  confirmLabel: string;
  cancelLabel: string;
  deletesFiles: boolean;
}

export function discardsDeletesFiles(status: string | null | undefined): boolean {
  return String(status ?? "").toLowerCase() !== "completed";
}

export function buildDiscardConfirmCopy(status: string, destination = ""): DiscardConfirmCopy {
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

/** Copy for discarding several jobs at once (batch action). */
export function buildBatchDiscardCopy(statuses: string[]): DiscardConfirmCopy {
  const unfinished = statuses.filter(discardsDeletesFiles).length;
  const total = statuses.length;
  const title = t("discard.batchTitle", { count: total });
  const confirmLabel = t("discard.confirm");
  const cancelLabel = t("discard.cancel");
  if (unfinished === 0) {
    return { title, body: t("discard.batchCompletedBody"), confirmLabel, cancelLabel, deletesFiles: false };
  }
  return {
    title,
    body: `${t("discard.batchUnfinishedBody", { count: unfinished })}\n\n${t("discard.cannotUndo")}`,
    confirmLabel,
    cancelLabel,
    deletesFiles: true,
  };
}
