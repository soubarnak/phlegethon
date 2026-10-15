import { describe, expect, it } from "vitest";
import {
  QUEUE_FILTER_ALL,
  QUEUE_SORT_QUEUE,
  QUEUE_SORT_SIZE,
  QUEUE_SORT_STATUS,
  QUEUE_SORT_TITLE,
  applyQueueView,
  jobMatchesSearch,
  jobMatchesStateFilter,
} from "./queueView";
import { job } from "@/test/mockApi";

const jobs = [
  job({ job_id: "a", title: "Alpha Video", url: "https://example.com/alpha", destination: "/dl/alpha", status: "pending", total_bytes: 100, queue_index: 0 }),
  job({ job_id: "b", title: "Beta Song", url: "https://cdn.example.com/beta.mp3", destination: "/music", path: "/music/beta.mp3", status: "completed", total_bytes: 50, queue_index: 1 }),
  job({ job_id: "c", title: "Gamma Live", url: "https://live.example.com/gamma", destination: "", status: "downloading", total_bytes: 200, queue_index: 2 }),
  job({ job_id: "d", title: "Delta Fail", url: "https://example.com/delta", destination: "/dl/delta", status: "failed", total_bytes: null, queue_index: 3 }),
  job({ job_id: "e", title: "Epsilon Pause", url: "https://example.com/epsilon", destination: "/dl/eps", status: "paused", total_bytes: 80, queue_index: 4 }),
];
const ids = (list: Array<{ job_id: string }>) => list.map((j) => j.job_id);

describe("queue search", () => {
  it("matches title, url, destination and path case-insensitively", () => {
    expect(jobMatchesSearch(jobs[0], "alpha")).toBe(true);
    expect(jobMatchesSearch(jobs[0], "EXAMPLE.COM/ALPHA")).toBe(true);
    expect(jobMatchesSearch(jobs[0], "/dl/alpha")).toBe(true);
    expect(jobMatchesSearch(jobs[1], "beta.mp3")).toBe(true);
    expect(jobMatchesSearch(jobs[0], "zzz")).toBe(false);
  });
  it("empty query matches everything", () => {
    expect(jobMatchesSearch(jobs[0], "")).toBe(true);
    expect(jobMatchesSearch(jobs[0], "   ")).toBe(true);
  });
});

describe("queue state filter", () => {
  it("all passes everything; queued is an alias for pending", () => {
    for (const j of jobs) expect(jobMatchesStateFilter(j, QUEUE_FILTER_ALL)).toBe(true);
    expect(jobMatchesStateFilter(jobs[0], "queued")).toBe(true);
    expect(jobMatchesStateFilter(jobs[2], "queued")).toBe(false);
    expect(jobMatchesStateFilter(jobs[2], "downloading")).toBe(true);
  });
});

describe("applyQueueView", () => {
  it("keeps Core order by default and never mutates the source", () => {
    const source = jobs.map((j) => ({ ...j }));
    const view = applyQueueView(source, { sort: QUEUE_SORT_TITLE, search: "/dl" });
    expect(ids(view)).toEqual(["a", "d", "e"]);
    expect(ids(source)).toEqual(["a", "b", "c", "d", "e"]);
    expect(ids(applyQueueView(jobs, { sort: QUEUE_SORT_QUEUE }))).toEqual(["a", "b", "c", "d", "e"]);
  });
  it("sorts by status then title", () => {
    expect(ids(applyQueueView(jobs, { sort: QUEUE_SORT_STATUS }))).toEqual(["b", "c", "d", "e", "a"]);
  });
  it("sorts by size descending with unknown sizes last", () => {
    expect(ids(applyQueueView(jobs, { sort: QUEUE_SORT_SIZE }))).toEqual(["c", "a", "e", "b", "d"]);
  });
  it("view order never changes queue_index (reorder uses Core indices)", () => {
    const view = applyQueueView(jobs, { sort: QUEUE_SORT_TITLE });
    expect(view.map((j) => j.queue_index)).not.toEqual([0, 1, 2, 3, 4]);
    for (const j of view) expect(j.queue_index).toBe(jobs.find((x) => x.job_id === j.job_id)!.queue_index);
  });
});
