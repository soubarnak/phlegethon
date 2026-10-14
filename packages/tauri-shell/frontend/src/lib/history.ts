/** History (Download Archive) view helpers. */
import type { ArchiveEntry, ArchiveOutcome } from "@/tauri/types";

export type HistoryFilter = "all" | ArchiveOutcome;

export function filterHistory(entries: ArchiveEntry[], filter: HistoryFilter): ArchiveEntry[] {
  if (filter === "all") return entries;
  return entries.filter((e) => e.outcome === filter);
}

/** Relative / short timestamp for list rows. */
export function formatTimestamp(unixSecs: number, now: Date = new Date()): string {
  const d = new Date(unixSecs * 1000);
  const diffMs = now.getTime() - d.getTime();

  if (diffMs < 60_000) return "Just now";
  if (diffMs < 3600_000) {
    const mins = Math.floor(diffMs / 60_000);
    return `${mins}m ago`;
  }
  if (d.toDateString() === now.toDateString()) {
    return d.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
  }
  const yesterday = new Date(now);
  yesterday.setDate(yesterday.getDate() - 1);
  if (d.toDateString() === yesterday.toDateString()) {
    return "Yesterday " + d.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
  }
  return d.toLocaleDateString(undefined, {
    month: "short",
    day: "numeric",
    year: d.getFullYear() !== now.getFullYear() ? "numeric" : undefined,
  });
}

/** Failed entries point at a folder; only Completed ones have a file to open. */
export function historyEntryHasFile(entry: Pick<ArchiveEntry, "outcome">): boolean {
  return entry.outcome === "Completed";
}
