/**
 * History (Download Archive) view helpers: outcome filter, search, sort and paging.
 * A view overlay over the archive list; it never reorders the Core archive itself.
 */
import { t } from "./i18n";
import type { ArchiveEntry, ArchiveOutcome } from "@/tauri/types";

export type HistoryFilter = "all" | ArchiveOutcome;

export const HISTORY_SORT_FINISHED = "finished";
export const HISTORY_SORT_NAME = "name";
export const HISTORY_SORT_SIZE = "size";

export type HistorySort = typeof HISTORY_SORT_FINISHED | typeof HISTORY_SORT_NAME | typeof HISTORY_SORT_SIZE;
export type HistorySortDir = "asc" | "desc";

/** Choices for the History pagination control. */
export const HISTORY_PAGE_SIZES = [25, 50, 100];

export interface HistoryViewOptions {
  search?: string;
  filter?: HistoryFilter;
  sort?: HistorySort;
  dir?: HistorySortDir;
}

export function filterHistory(entries: ArchiveEntry[], filter: HistoryFilter): ArchiveEntry[] {
  if (filter === "all") return entries;
  return entries.filter((e) => e.outcome === filter);
}

/**
 * File name of a Completed entry. Its `destination` is the final file path; a Failed entry's
 * `destination` is the Job Destination folder, so it has no file name to match or show.
 */
export function historyFilename(entry: Pick<ArchiveEntry, "outcome" | "destination">): string {
  if (entry.outcome !== "Completed") return "";
  return entry.destination.split(/[\\/]/).pop() ?? "";
}

/** Case-insensitive match on title, URL or file name. An empty query matches everything. */
export function historyMatchesSearch(entry: ArchiveEntry, query: string): boolean {
  const q = query.trim().toLowerCase();
  if (!q) return true;
  return [entry.title, entry.url, historyFilename(entry)].some((field) => field.toLowerCase().includes(q));
}

/** Ascending comparison for one sort key. Size is handled by the caller so unknown sizes can sort last. */
function compareEntries(a: ArchiveEntry, b: ArchiveEntry, sort: HistorySort): number {
  switch (sort) {
    case HISTORY_SORT_NAME:
      return a.title.localeCompare(b.title, undefined, { sensitivity: "base" });
    case HISTORY_SORT_SIZE:
      return (a.size_bytes ?? 0) - (b.size_bytes ?? 0);
    case HISTORY_SORT_FINISHED:
    default:
      return a.completed_at_unix_secs - b.completed_at_unix_secs;
  }
}

/** Filter, search and sort a copy of the archive list. Ties break on media_id so the order is stable. */
export function applyHistoryView(entries: ArchiveEntry[], opts: HistoryViewOptions = {}): ArchiveEntry[] {
  const sort = opts.sort ?? HISTORY_SORT_FINISHED;
  const sign = (opts.dir ?? "desc") === "asc" ? 1 : -1;
  return filterHistory(entries, opts.filter ?? "all")
    .filter((entry) => historyMatchesSearch(entry, opts.search ?? ""))
    .sort((a, b) => {
      // Unknown size stays last in both directions.
      if (sort === HISTORY_SORT_SIZE && (a.size_bytes == null || b.size_bytes == null)) {
        if (a.size_bytes == null && b.size_bytes == null) return a.media_id.localeCompare(b.media_id);
        return a.size_bytes == null ? 1 : -1;
      }
      return sign * compareEntries(a, b, sort) || a.media_id.localeCompare(b.media_id);
    });
}

export interface HistoryPage<T> {
  items: T[];
  /** The page actually shown, clamped into range when the list shrank. */
  page: number;
  pageCount: number;
}

/** One page of a list, 1-based. Out-of-range pages clamp to the nearest real page. */
export function paginateHistory<T>(list: T[], page: number, pageSize: number): HistoryPage<T> {
  const size = Math.max(1, Math.floor(pageSize));
  const pageCount = Math.max(1, Math.ceil(list.length / size));
  const current = Math.min(Math.max(1, Math.floor(page)), pageCount);
  const start = (current - 1) * size;
  return { items: list.slice(start, start + size), page: current, pageCount };
}

/** Relative / short timestamp for list rows. */
export function formatTimestamp(unixSecs: number, now: Date = new Date()): string {
  const d = new Date(unixSecs * 1000);
  const diffMs = now.getTime() - d.getTime();

  if (diffMs < 60_000) return t("history.justNow");
  if (diffMs < 3600_000) {
    const mins = Math.floor(diffMs / 60_000);
    return t("history.minutesAgo", { count: mins });
  }
  if (d.toDateString() === now.toDateString()) {
    return d.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
  }
  const yesterday = new Date(now);
  yesterday.setDate(yesterday.getDate() - 1);
  if (d.toDateString() === yesterday.toDateString()) {
    const time = d.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
    return t("history.yesterday", { time });
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
