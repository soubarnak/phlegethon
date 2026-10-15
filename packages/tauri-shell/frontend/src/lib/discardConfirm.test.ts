import { describe, expect, it } from "vitest";
import { buildBatchDiscardCopy, buildDiscardConfirmCopy, discardsDeletesFiles } from "./discardConfirm";

describe("discard confirm copy", () => {
  it("completed: queue row only, files remain", () => {
    const copy = buildDiscardConfirmCopy("completed", "/lib/jobs/abc");
    expect(copy.deletesFiles).toBe(false);
    expect(copy.body).toMatch(/queue only/i);
    expect(copy.body).toMatch(/will not be deleted/i);
    expect(copy.body).not.toMatch(/Job Destination/);
    expect(copy.confirmLabel).toBe("Discard");
  });

  it.each(["pending", "downloading", "paused", "failed", "queued"])("%s: warns and shows the path", (status) => {
    const dest = "/lib/jobs/job-1";
    const copy = buildDiscardConfirmCopy(status, dest);
    expect(discardsDeletesFiles(status)).toBe(true);
    expect(copy.deletesFiles).toBe(true);
    expect(copy.body).toMatch(/Job Destination will be deleted/i);
    expect(copy.body).toContain(dest);
    expect(copy.body).toMatch(/cannot be undone/i);
  });

  it("unfinished without a path still warns", () => {
    expect(buildDiscardConfirmCopy("paused", "").body).toMatch(/Job Destination will be deleted/i);
  });

  it("batch copy counts unfinished jobs", () => {
    const mixed = buildBatchDiscardCopy(["completed", "failed", "paused"]);
    expect(mixed.deletesFiles).toBe(true);
    expect(mixed.title).toContain("3");
    expect(mixed.body).toContain("2");
    expect(buildBatchDiscardCopy(["completed", "completed"]).deletesFiles).toBe(false);
  });
});
