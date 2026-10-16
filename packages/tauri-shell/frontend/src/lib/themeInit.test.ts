import { readFileSync } from "node:fs";
import path from "node:path";
import { afterEach, describe, expect, it, vi } from "vitest";

const script = readFileSync(path.resolve(__dirname, "../../public/theme-init.js"), "utf8");

function run(stored: string | null, prefersDark: boolean): string | null {
  document.documentElement.removeAttribute("data-theme");
  window.localStorage.clear();
  if (stored) window.localStorage.setItem("phlegethon-theme", stored);
  vi.spyOn(window, "matchMedia").mockImplementation(
    (query: string) => ({ matches: prefersDark && query.includes("dark"), media: query }) as unknown as MediaQueryList,
  );
  new Function(script)();
  return document.documentElement.getAttribute("data-theme");
}

describe("public/theme-init.js (UI-11)", () => {
  afterEach(() => vi.restoreAllMocks());

  it("applies the stored theme before React mounts", () => {
    expect(run("g100", false)).toBe("g100");
    expect(run("g10", true)).toBe("g10");
  });

  it("accepts the legacy light and dark values", () => {
    expect(run("dark", false)).toBe("g100");
    expect(run("light", true)).toBe("g10");
  });

  it("follows the system when nothing is stored", () => {
    expect(run(null, true)).toBe("g100");
    expect(run(null, false)).toBe("g10");
  });

  it("ignores junk in storage", () => {
    expect(run("purple", true)).toBe("g100");
  });
});
