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

  const table = en as Record<string, string>;
  const allSources = () =>
    sources(path.resolve(__dirname, "..")).map((file) => ({ file, text: readFileSync(file, "utf8") }));
  const placeholders = (value: string) => new Set([...value.matchAll(/\{([a-zA-Z0-9_]+)\}/g)].map((m) => m[1]));

  it("has no unused keys in en.json", () => {
    const files = allSources();
    const used = (key: string) => {
      const dynamicPrefix = key.slice(0, key.lastIndexOf(".") + 1);
      return files.some(
        ({ text }) =>
          text.includes(`"${key}"`) || text.includes(`'${key}'`) || (dynamicPrefix && text.includes(`${dynamicPrefix}\${`)),
      );
    };
    expect(Object.keys(table).filter((key) => !used(key))).toEqual([]);
  });

  it("passes the placeholders each string expects", () => {
    const mismatched: string[] = [];
    for (const { file, text } of allSources()) {
      for (const m of text.matchAll(/\bt\(\s*["']([a-zA-Z0-9_.]+)["']\s*,\s*\{([^}]*)\}/g)) {
        const value = table[m[1]];
        if (value === undefined) continue;
        const given = new Set(
          m[2]
            .split(",")
            .map((part) => part.split(":")[0].trim())
            .filter(Boolean),
        );
        const wanted = placeholders(value);
        if (given.size !== wanted.size || [...wanted].some((name) => !given.has(name))) {
          mismatched.push(`${path.basename(file)}: ${m[1]} wants {${[...wanted]}} got {${[...given]}}`);
        }
      }
    }
    expect(mismatched).toEqual([]);
  });

  it("keeps user-facing sentences out of errors.ts and history.ts", () => {
    const offenders: string[] = [];
    for (const name of ["errors.ts", "history.ts"]) {
      const code = readFileSync(path.resolve(__dirname, name), "utf8")
        .replace(/\/\*[\s\S]*?\*\//g, "")
        .replace(/^\s*\/\/.*$/gm, "");
      for (const m of code.matchAll(/"([^"\n\\]*)"|'([^'\n\\]*)'|`([^`\n\\]*)`/g)) {
        const literal = m[1] ?? m[2] ?? m[3] ?? "";
        if (/[A-Za-z]{3,} [A-Za-z]{2,}/.test(literal) || /\}[A-Za-z]{2,}/.test(literal)) offenders.push(`${name}: ${literal}`);
      }
    }
    expect(offenders).toEqual([]);
  });
});
