import { describe, expect, it } from "vitest";
import { capToPromptKb } from "./bandwidthCap";

describe("capToPromptKb", () => {
  it("reports no value for an uncapped job", () => {
    expect(capToPromptKb(null)).toBeNull();
    expect(capToPromptKb(undefined)).toBeNull();
    expect(capToPromptKb(0)).toBeNull();
  });

  it("converts bytes per second to whole KB/s", () => {
    expect(capToPromptKb(512 * 1024)).toBe(512);
    expect(capToPromptKb(1024)).toBe(1);
  });

  it("never shows 0 for a cap below 1 KB/s, since 0 means no limit", () => {
    expect(capToPromptKb(500)).toBe(1);
    expect(capToPromptKb(1)).toBe(1);
  });

  it("rounds to the nearest whole KB/s", () => {
    expect(capToPromptKb(1536)).toBe(2);
    expect(capToPromptKb(1535)).toBe(1);
  });
});
