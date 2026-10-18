import { describe, expect, it } from "vitest";
import { errorHint, errorToString, parseCoreError } from "./errors";

describe("core errors", () => {
  it("splits a [code] prefix from the message", () => {
    expect(parseCoreError("[needs_browser_cookies] yt-dlp probe failed: 403")).toEqual({
      code: "needs_browser_cookies",
      message: "yt-dlp probe failed: 403",
    });
    expect(parseCoreError("plain failure")).toEqual({ code: null, message: "plain failure" });
  });
  it("stringifies unknown error shapes", () => {
    expect(errorToString(new Error("boom"))).toBe("boom");
    expect(errorToString({ message: "m" })).toBe("m");
    expect(errorToString(null)).toBe("");
  });
  it("has an actionable hint for cookie walls", () => {
    expect(errorHint("needs_browser_cookies")).toMatch(/Cookies/);
    expect(errorHint("toolchain_missing")).toMatch(/Toolchain/);
    expect(errorHint(null)).toBeNull();
  });
});
