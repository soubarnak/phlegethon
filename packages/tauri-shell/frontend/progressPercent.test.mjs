import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { displayProgressPercent } from "./progressPercent.js";

describe("displayProgressPercent", () => {
  it("returns null when total unknown", () => {
    assert.equal(displayProgressPercent(1024, null), null);
    assert.equal(displayProgressPercent(1024, 0), null);
    assert.equal(displayProgressPercent(1024, undefined), null);
  });

  it("returns rounded percent under 100", () => {
    assert.equal(displayProgressPercent(50, 100), 50);
    assert.equal(displayProgressPercent(1, 3), 33);
  });

  it("never exceeds 100 when downloaded exceeds total (merged streams)", () => {
    assert.equal(displayProgressPercent(137, 100), 100);
    assert.equal(displayProgressPercent(2_000_000, 1_000_000), 100);
  });
});
