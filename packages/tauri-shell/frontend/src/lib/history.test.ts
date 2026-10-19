import { describe, expect, it } from "vitest";
import {
  HISTORY_SORT_FINISHED,
  HISTORY_SORT_NAME,
  HISTORY_SORT_SIZE,
  applyHistoryView,
  filterHistory,
  formatTimestamp,
  historyEntryHasFile,
  historyFilename,
  paginateHistory,
} from "./history";
import type { ArchiveEntry } from "@/tauri/types";

const entry = (id: string, outcome: ArchiveEntry["outcome"], overrides: Partial<ArchiveEntry> = {}): ArchiveEntry => ({
  media_id: id,
  title: id,
  url: `https://x/${id}`,
  completed_at_unix_secs: 0,
  destination: "/d",
  job_id: id,
  outcome,
  size_bytes: null,
  ...overrides,
});

const ids = (list: ArchiveEntry[]) => list.map((e) => e.media_id);

describe("history", () => {
  it("filters by outcome", () => {
    const list = [entry("a", "Completed"), entry("b", "Failed")];
    expect(filterHistory(list, "all")).toHaveLength(2);
    expect(filterHistory(list, "Failed").map((e) => e.media_id)).toEqual(["b"]);
  });
  it("only completed entries have a file to open", () => {
    expect(historyEntryHasFile(entry("a", "Completed"))).toBe(true);
    expect(historyEntryHasFile(entry("b", "Failed"))).toBe(false);
  });
  it("formats recent timestamps relatively", () => {
    const now = new Date("2026-10-14T12:00:00Z");
    expect(formatTimestamp(now.getTime() / 1000 - 10, now)).toBe("Just now");
    expect(formatTimestamp(now.getTime() / 1000 - 600, now)).toBe("10m ago");
  });
});

describe("historyFilename", () => {
  it("is the last path segment of a Completed entry's final file path", () => {
    expect(historyFilename(entry("a", "Completed", { destination: "/lib/jobs/j1/Clip [1080p].mp4" }))).toBe("Clip [1080p].mp4");
  });
  it("is empty for a Failed entry, whose destination is only a folder", () => {
    expect(historyFilename(entry("b", "Failed", { destination: "/lib/jobs/j2" }))).toBe("");
  });
});

describe("applyHistoryView search", () => {
  const list = [
    entry("a", "Completed", { title: "Mountain Walk", url: "https://video.example/m1", destination: "/lib/jobs/a/walk-final.mp4" }),
    entry("b", "Completed", { title: "Cooking Show", url: "https://video.example/c2", destination: "/lib/jobs/b/recipe.webm" }),
    entry("c", "Failed", { title: "Lost Clip", url: "https://other.example/x", destination: "/lib/jobs/mountain-folder" }),
  ];

  it("matches the title, ignoring case", () => {
    expect(ids(applyHistoryView(list, { search: "MOUNTAIN" }))).toEqual(["a"]);
  });
  it("matches the URL", () => {
    expect(ids(applyHistoryView(list, { search: "other.example" }))).toEqual(["c"]);
  });
  it("matches the file name of a Completed entry", () => {
    expect(ids(applyHistoryView(list, { search: "recipe" }))).toEqual(["b"]);
  });
  it("does not match a Failed entry's folder path, which is not a file name", () => {
    expect(ids(applyHistoryView(list, { search: "mountain-folder" }))).toEqual([]);
  });
  it("treats a blank query as no search", () => {
    expect(applyHistoryView(list, { search: "   " })).toHaveLength(3);
  });
  it("combines with the outcome filter", () => {
    expect(ids(applyHistoryView(list, { search: "video.example", filter: "Failed" }))).toEqual([]);
    expect(ids(applyHistoryView(list, { search: "video.example", filter: "Completed" }))).toEqual(["a", "b"]);
  });
});

describe("applyHistoryView sort", () => {
  const list = [
    entry("m", "Completed", { title: "bravo", completed_at_unix_secs: 200, size_bytes: 500 }),
    entry("n", "Completed", { title: "Alpha", completed_at_unix_secs: 100, size_bytes: 9000 }),
    entry("o", "Failed", { title: "charlie", completed_at_unix_secs: 300, size_bytes: null }),
    entry("p", "Completed", { title: "Delta", completed_at_unix_secs: 400, size_bytes: 20 }),
  ];

  it("defaults to finished time, newest first", () => {
    expect(ids(applyHistoryView(list))).toEqual(["p", "o", "m", "n"]);
  });
  it("sorts by finished time ascending", () => {
    expect(ids(applyHistoryView(list, { sort: HISTORY_SORT_FINISHED, dir: "asc" }))).toEqual(["n", "m", "o", "p"]);
  });
  it("sorts by name A to Z, ignoring case", () => {
    expect(ids(applyHistoryView(list, { sort: HISTORY_SORT_NAME, dir: "asc" }))).toEqual(["n", "m", "o", "p"]);
  });
  it("sorts by name Z to A", () => {
    expect(ids(applyHistoryView(list, { sort: HISTORY_SORT_NAME, dir: "desc" }))).toEqual(["p", "o", "m", "n"]);
  });
  it("sorts by size, largest first, with unknown sizes last", () => {
    expect(ids(applyHistoryView(list, { sort: HISTORY_SORT_SIZE, dir: "desc" }))).toEqual(["n", "m", "p", "o"]);
  });
  it("sorts by size, smallest first, still with unknown sizes last", () => {
    expect(ids(applyHistoryView(list, { sort: HISTORY_SORT_SIZE, dir: "asc" }))).toEqual(["p", "m", "n", "o"]);
  });
  it("breaks ties on media_id so the order is stable", () => {
    const tied = [entry("z", "Completed", { size_bytes: 1 }), entry("y", "Completed", { size_bytes: 1 })];
    expect(ids(applyHistoryView(tied, { sort: HISTORY_SORT_SIZE, dir: "desc" }))).toEqual(["y", "z"]);
  });
  it("does not reorder the input list", () => {
    const before = ids(list);
    applyHistoryView(list, { sort: HISTORY_SORT_NAME, dir: "asc" });
    expect(ids(list)).toEqual(before);
  });
});

describe("paginateHistory", () => {
  const items = Array.from({ length: 26 }, (_, i) => i + 1);

  it("returns the first page of the requested size", () => {
    const page = paginateHistory(items, 1, 25);
    expect(page.items).toHaveLength(25);
    expect(page.items[0]).toBe(1);
    expect(page.pageCount).toBe(2);
    expect(page.page).toBe(1);
  });
  it("returns the remainder on the last page", () => {
    expect(paginateHistory(items, 2, 25).items).toEqual([26]);
  });
  it("clamps a page past the end to the last page", () => {
    const page = paginateHistory(items, 5, 25);
    expect(page.page).toBe(2);
    expect(page.items).toEqual([26]);
  });
  it("shows page 1 of an empty list", () => {
    expect(paginateHistory([], 3, 25)).toEqual({ items: [], page: 1, pageCount: 1 });
  });
  it("honours 100 per page", () => {
    expect(paginateHistory(items, 1, 100).items).toHaveLength(26);
  });
});
