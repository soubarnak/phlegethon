import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { getCurrentLocale, initLocale, t } from "./i18n.js";

describe("i18n", () => {
  it("defaults to English locale", () => {
    initLocale();
    assert.equal(getCurrentLocale(), "en");
  });

  it("falls back to English for unknown locale", () => {
    initLocale("fr");
    assert.equal(getCurrentLocale(), "en");
  });

  it("resolves queue and probe strings", () => {
    assert.equal(t("queue.action.pause"), "Pause");
    assert.equal(t("probe.confirm"), "Download Selected");
  });

  it("interpolates parameters", () => {
    assert.equal(
      t("probe.qualityFor", { title: "Example" }),
      "Quality for Example",
    );
    assert.equal(
      t("probe.selection.many", { count: 3 }),
      "3 items selected",
    );
  });

  it("returns key when missing", () => {
    assert.equal(t("missing.key"), "missing.key");
  });
});
