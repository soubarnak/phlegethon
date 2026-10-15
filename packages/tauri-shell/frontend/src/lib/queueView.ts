/**
 * Queue search / filter / sort — a view overlay over Core queue identity.
 * Reorder ↑↓ must use Core indices (`queue_index`), never the view order.
 */
import type { JobSnapshot } from "@/tauri/types";

export const QUEUE_FILTER_ALL = "all";

export const QUEUE_SORT_QUEUE = "queue";
export const QUEUE_SORT_TITLE = "title";
export const QUEUE_SORT_STATUS = "status";
export const QUEUE_SORT_SIZE = "size";

export type QueueSort =
  | typeof QUEUE_SORT_QUEUE
  | typeof QUEUE_SORT_TITLE
  | typeof QUEUE_SORT_STATUS
  | typeof QUEUE_SORT_SIZE;

export interface QueueViewOptions {
  search?: string;
  stateFilter?: string;
  sort?: string;
}

type SearchableJob = Pick<JobSnapshot, "title" | "url" | "destination"> & {
  path?: string | null;
};

/** Status sort key order (alphabetical by UI status string). */
const STATUS_ORDER: Record<string, number> = {
  completed: 0,
  downloading: 1,
  failed: 2,
  paused: 3,
  pending: 4,
};

export function jobMatchesSearch(job: SearchableJob, query: string): boolean {
  const q = (query || "").trim().toLowerCase();
  if (!q) return true;
  const haystacks = [job.title, job.url, job.destination, job.path];
  return haystacks.some(
    (field) => typeof field === "string" && field.toLowerCase().includes(q),
  );
}

export function jobMatchesStateFilter(job: Pick<JobSnapshot, "status">, stateFilter: string): boolean {
  const filter = (stateFilter || QUEUE_FILTER_ALL).toLowerCase();
  if (!filter || filter === QUEUE_FILTER_ALL) return true;
  if (filter === "queued") return job.status === "pending";
  return job.status === filter;
}

function compareJobs(a: JobSnapshot, b: JobSnapshot, sort: string): number {
  switch (sort) {
    case QUEUE_SORT_TITLE: {
      const cmp = String(a.title || "").localeCompare(String(b.title || ""), undefined, {
        sensitivity: "base",
      });
      return cmp !== 0 ? cmp : String(a.job_id).localeCompare(String(b.job_id));
    }
    case QUEUE_SORT_STATUS: {
      const oa = STATUS_ORDER[a.status] ?? 99;
      const ob = STATUS_ORDER[b.status] ?? 99;
      if (oa !== ob) return oa - ob;
      return String(a.title || "").localeCompare(String(b.title || ""), undefined, {
        sensitivity: "base",
      });
    }
    case QUEUE_SORT_SIZE: {
      const sa = a.total_bytes == null ? -1 : Number(a.total_bytes);
      const sb = b.total_bytes == null ? -1 : Number(b.total_bytes);
      // Known totals first, descending; unknown last.
      if (sa < 0 && sb < 0) {
        return String(a.job_id).localeCompare(String(b.job_id));
      }
      if (sa < 0) return 1;
      if (sb < 0) return -1;
      if (sb !== sa) return sb - sa;
      return String(a.job_id).localeCompare(String(b.job_id));
    }
    case QUEUE_SORT_QUEUE:
    default:
      return 0;
  }
}

/** Filter + sort a copy of jobs. Does not mutate `jobs` (Core queue identity). */
export function applyQueueView<T extends JobSnapshot>(jobs: T[], opts: QueueViewOptions = {}): T[] {
  const search = opts.search ?? "";
  const stateFilter = opts.stateFilter ?? QUEUE_FILTER_ALL;
  const sort = opts.sort ?? QUEUE_SORT_QUEUE;

  const filtered = jobs.filter(
    (job) => jobMatchesSearch(job, search) && jobMatchesStateFilter(job, stateFilter),
  );

  if (sort === QUEUE_SORT_QUEUE) {
    return filtered;
  }

  return filtered.slice().sort((a, b) => compareJobs(a, b, sort));
}
