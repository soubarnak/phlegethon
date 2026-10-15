import { readdirSync, readFileSync, statSync } from "node:fs";
import path from "node:path";
import { describe, expect, it } from "vitest";
import en from "@strings/en.json";
import { getCurrentLocale, initLocale, t } from "./i18n";

function sources(dir: string): string[] {
  return readdirSync(dir).flatMap((name) => {
    const full = path.join(dir, name);
    if (statSync(full).isDirectory()) return name === "test" ? [] : sources(full);
    return /\.(ts|tsx)$/.test(name) && !/\.test\./.test(name) ? [full] : [];
  });
}

describe("i18n", () => {
  it("defaults to English and falls back for unknown locales", () => {
    initLocale();
    expect(getCurrentLocale()).toBe("en");
    initLocale("fr");
    expect(getCurrentLocale()).toBe("en");
  });
  it("interpolates parameters", () => {
    expect(t("probe.qualityFor", { title: "Example" })).toBe("Quality for Example");
    expect(t("probe.selection.many", { count: 3 })).toBe("3 items selected");
    expect(t("does.not.exist")).toBe("does.not.exist");
  });
  it("every literal t('key') used in the UI exists in en.json", () => {
    const table = en as Record<string, string>;
    const missing = new Set<string>();
    for (const file of sources(path.resolve(__dirname, ".."))) {
      const text = readFileSync(file, "utf8");
      for (const m of text.matchAll(/\bt\(\s*["']([a-zA-Z0-9_.]+)["']/g)) {
        if (!(m[1] in table)) missing.add(`${path.basename(file)}: ${m[1]}`);
      }
      for (const m of text.matchAll(/label:\s*["']([a-z]+(?:\.[a-zA-Z]+)+)["']/g)) {
        if (!(m[1] in table)) missing.add(`${path.basename(file)}: ${m[1]}`);
      }
    }
    expect([...missing]).toEqual([]);
  });
  it("covers the dynamic toolchain keys", () => {
    const table = en as Record<string, string>;
    for (const k of ["bundled", "python_package", "missing", "unknown"]) expect(table[`toolchain.ejs.${k}`]).toBeTruthy();
    for (const k of ["sidecar_manifest", "env_var", "path", "python_module"]) expect(table[`toolchain.source.${k}`]).toBeTruthy();
  });
});
