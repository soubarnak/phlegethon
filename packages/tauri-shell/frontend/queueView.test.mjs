import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  QUEUE_FILTER_ALL,
  QUEUE_SORT_QUEUE,
  QUEUE_SORT_SIZE,
  QUEUE_SORT_STATUS,
  QUEUE_SORT_TITLE,
  applyQueueView,
  jobMatchesSearch,
  jobMatchesStateFilter,
} from "./queueView.js";

const jobs = [
  {
    job_id: "a",
    title: "Alpha Video",
    url: "https://example.com/alpha",
    destination: "D:/dl/alpha",
    path: null,
    status: "pending",
    bytes_downloaded: 0,
    total_bytes: 100,
  },
  {
    job_id: "b",
    title: "Beta Song",
    url: "https://cdn.example.com/beta.mp3",
    destination: "D:/music",
    path: "D:/music/beta.mp3",
    status: "completed",
    bytes_downloaded: 50,
    total_bytes: 50,
  },
  {
    job_id: "c",
    title: "Gamma Live",
    url: "https://live.example.com/gamma",
    destination: "",
    path: null,
    status: "downloading",
    bytes_downloaded: 10,
    total_bytes: 200,
  },
  {
    job_id: "d",
    title: "Delta Fail",
    url: "https://example.com/delta",
    destination: "D:/dl/delta",
    path: null,
    status: "failed",
    bytes_downloaded: 5,
    total_bytes: null,
  },
  {
    job_id: "e",
    title: "Epsilon Pause",
    url: "https://example.com/epsilon",
    destination: "D:/dl/eps",
    path: null,
    status: "paused",
    bytes_downloaded: 20,
    total_bytes: 80,
  },
];

describe("jobMatchesSearch", () => {
  it("matches title, url, destination, and path case-insensitively", () => {
    assert.equal(jobMatchesSearch(jobs[0], "alpha"), true);
    assert.equal(jobMatchesSearch(jobs[0], "EXAMPLE.COM/ALPHA"), true);
    assert.equal(jobMatchesSearch(jobs[0], "d:/dl/alpha"), true);
    assert.equal(jobMatchesSearch(jobs[1], "beta.mp3"), true);
    assert.equal(jobMatchesSearch(jobs[0], "zzz"), false);
  });

  it("empty query matches all", () => {
    assert.equal(jobMatchesSearch(jobs[0], ""), true);
    assert.equal(jobMatchesSearch(jobs[0], "   "), true);
  });
});

describe("jobMatchesStateFilter", () => {
  it("all passes every status", () => {
    for (const job of jobs) {
      assert.equal(jobMatchesStateFilter(job, QUEUE_FILTER_ALL), true);
    }
  });

  it("filters by exact UI status", () => {
    assert.equal(jobMatchesStateFilter(jobs[0], "pending"), true);
    assert.equal(jobMatchesStateFilter(jobs[0], "completed"), false);
    assert.equal(jobMatchesStateFilter(jobs[2], "downloading"), true);
    assert.equal(jobMatchesStateFilter(jobs[4], "paused"), true);
  });

  it("queued alias matches pending", () => {
    assert.equal(jobMatchesStateFilter(jobs[0], "queued"), true);
    assert.equal(jobMatchesStateFilter(jobs[2], "queued"), false);
  });
});

describe("applyQueueView", () => {
  it("returns Core queue order by default", () => {
    const view = applyQueueView(jobs, {
      search: "",
      stateFilter: QUEUE_FILTER_ALL,
      sort: QUEUE_SORT_QUEUE,
    });
    assert.deepEqual(
      view.map((j) => j.job_id),
      ["a", "b", "c", "d", "e"],
    );
  });

  it("filters then sorts without mutating source order identity", () => {
    const source = jobs.map((j) => ({ ...j }));
    const view = applyQueueView(source, {
      search: "D:/dl",
      stateFilter: QUEUE_FILTER_ALL,
      sort: QUEUE_SORT_TITLE,
    });
    assert.deepEqual(
      view.map((j) => j.job_id),
      ["a", "d", "e"],
    );
    assert.deepEqual(
      source.map((j) => j.job_id),
      ["a", "b", "c", "d", "e"],
    );
  });

  it("filters by state", () => {
    const view = applyQueueView(jobs, {
      search: "",
      stateFilter: "failed",
      sort: QUEUE_SORT_QUEUE,
    });
    assert.deepEqual(
      view.map((j) => j.job_id),
      ["d"],
    );
  });

  it("sorts by status then title", () => {
    const view = applyQueueView(jobs, {
      search: "",
      stateFilter: QUEUE_FILTER_ALL,
      sort: QUEUE_SORT_STATUS,
    });
    assert.deepEqual(
      view.map((j) => j.job_id),
      ["b", "c", "d", "e", "a"],
    );
  });

  it("sorts by size descending (known totals first)", () => {
    const view = applyQueueView(jobs, {
      search: "",
      stateFilter: QUEUE_FILTER_ALL,
      sort: QUEUE_SORT_SIZE,
    });
    assert.deepEqual(
      view.map((j) => j.job_id),
      ["c", "a", "e", "b", "d"],
    );
  });
});
