import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  buildDiscardConfirmCopy,
  discardsDeletesFiles,
} from "./discardConfirm.js";

describe("discard confirm copy", () => {
  it("completed: queue row only, files remain", () => {
    const copy = buildDiscardConfirmCopy("completed", "D:\\lib\\jobs\\abc");
    assert.equal(copy.deletesFiles, false);
    assert.match(copy.body, /queue only/i);
    assert.match(copy.body, /will not be deleted/i);
    assert.doesNotMatch(copy.body, /Job Destination/);
    assert.equal(copy.confirmLabel, "Discard");
  });

  for (const status of ["pending", "downloading", "paused", "failed", "queued"]) {
    it(`${status}: mentions deletion and shows Job Destination path`, () => {
      const dest = "D:\\lib\\jobs\\job-1";
      const copy = buildDiscardConfirmCopy(status, dest);
      assert.equal(discardsDeletesFiles(status), true);
      assert.equal(copy.deletesFiles, true);
      assert.match(copy.body, /Job Destination will be deleted/i);
      assert.ok(copy.body.includes(dest), "body includes Job Destination path");
      assert.match(copy.body, /cannot be undone/i);
    });
  }

  it("unfinished without path still warns about Job Destination deletion", () => {
    const copy = buildDiscardConfirmCopy("paused", "");
    assert.equal(copy.deletesFiles, true);
    assert.match(copy.body, /Job Destination will be deleted/i);
  });
});
