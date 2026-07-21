import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { getFocusableElements } from "./focusTrap.js";

describe("focusTrap", () => {
  it("filters disabled and hidden elements", () => {
    const visible = { offsetParent: {}, disabled: false };
    const hidden = { offsetParent: null, disabled: false };
    const disabled = { offsetParent: {}, disabled: true };
    const root = {
      querySelectorAll() {
        return [visible, hidden, disabled];
      },
    };

    const focusable = getFocusableElements(root);
    assert.equal(focusable.length, 1);
    assert.equal(focusable[0], visible);
  });
});
