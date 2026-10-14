import { describe, expect, it } from "vitest";
import { filterHistory, formatTimestamp, historyEntryHasFile } from "./history";
import type { ArchiveEntry } from "@/tauri/types";

const entry = (id: string, outcome: ArchiveEntry["outcome"]): ArchiveEntry => ({
  media_id: id, title: id, url: `https://x/${id}`, completed_at_unix_secs: 0, destination: "/d", job_id: id, outcome,
});

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
