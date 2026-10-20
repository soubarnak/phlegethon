import { readdirSync, readFileSync, statSync } from "node:fs";
import path from "node:path";
import { describe, expect, it } from "vitest";

function sources(dir: string): string[] {
  return readdirSync(dir).flatMap((name) => {
    const full = path.join(dir, name);
    if (statSync(full).isDirectory()) return sources(full);
    return /\.(ts|tsx)$/.test(name) ? [full] : [];
  });
}

describe("IPC boundary", () => {
  it("only src/tauri/api.ts imports invoke from @tauri-apps/api/core", () => {
    const root = path.resolve(__dirname, "..");
    const allowed = path.join(root, "tauri", "api.ts");
    // Static, dynamic and re-export forms, plus the package root, which re-exports `core`.
    const forms = /(?:from\s*|import\s*\(\s*|import\s+)["']@tauri-apps\/api(?:\/core)?["']/;
    const offenders = sources(root)
      .filter((file) => file !== allowed && !/\.test\.tsx?$/.test(file))
      .filter((file) => forms.test(readFileSync(file, "utf8")))
      .map((file) => path.relative(root, file));
    expect(offenders).toEqual([]);
  });
});
