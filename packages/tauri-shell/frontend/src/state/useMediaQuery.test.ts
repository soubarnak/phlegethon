import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import { NARROW_QUERY } from "./useMediaQuery";

describe("NARROW_QUERY", () => {
  it("matches the $ph-bp-md breakpoint in index.scss", () => {
    const scss = readFileSync(resolve(process.cwd(), "src/styles/index.scss"), "utf8");
    const md = Number(/\$ph-bp-md:\s*(\d+)px;/.exec(scss)?.[1]);
    expect(md).toBeGreaterThan(0);
    // The stylesheet's `below()` mixin emits max-width: (bp - 0.02px).
    expect(NARROW_QUERY).toBe(`(max-width: ${md - 0.02}px)`);
  });
});
