import { describe, expect, it } from "vitest";
import {
  applyQualityToAll,
  buildSelection,
  commonQualityOptions,
  defaultSelectedIds,
  invertSelection,
  remapQualityAfterEnrich,
  totalSelectedBytes,
} from "./probeSelection";
import { mediaItem, quality } from "@/test/mockApi";

const items = [
  mediaItem({ id: "a", qualities: [quality("1080p", "1080p", 1000), quality("720p", "720p", 500)] }),
  mediaItem({ id: "b", archived: true, qualities: [quality("720p", "720p", 400), quality("480p", "480p")] }),
  mediaItem({ id: "c", qualities: [quality("best", "best")] }),
];

describe("probe selection", () => {
  it("selects everything except archived items by default", () => {
    expect([...defaultSelectedIds(items)]).toEqual(["a", "c"]);
  });
  it("builds the confirm payload from the chosen qualities, falling back to the first", () => {
    const sel = buildSelection("https://u", items, new Set(["a", "b", "c"]), { a: "720p", b: "480p" }, null);
    expect(sel.items).toEqual([
      { item_id: "a", quality_id: "720p" },
      { item_id: "b", quality_id: "480p" },
      { item_id: "c", quality_id: "best" },
    ]);
    expect(sel.media_extras).toBeNull();
  });
  it("sums known sizes and counts unknown ones", () => {
    expect(totalSelectedBytes(items, new Set(["a", "b", "c"]), { a: "1080p", b: "720p" })).toEqual({ bytes: 1400, unknown: 1 });
  });
  it("lists the union of qualities, best resolution first", () => {
    expect(commonQualityOptions(items).map((q) => q.id)).toEqual(["best", "1080p", "720p", "480p"]);
  });
  it("applies a quality to selected items that offer it and leaves others alone", () => {
    const next = applyQualityToAll(items, new Set(["a", "b", "c"]), "720p", { c: "best" });
    expect(next).toEqual({ a: "720p", b: "720p", c: "best" });
  });
  it("inverts the selection", () => {
    expect([...invertSelection(items, new Set(["a"]))]).toEqual(["b", "c"]);
  });
  it("keeps the picked quality after enrich when it still exists", () => {
    expect(remapQualityAfterEnrich("720p", [quality("1080p"), quality("720p")])).toBe("720p");
    expect(remapQualityAfterEnrich("2160p", [quality("1080p")])).toBe("1080p");
    expect(remapQualityAfterEnrich(undefined, [])).toBeNull();
  });
});
