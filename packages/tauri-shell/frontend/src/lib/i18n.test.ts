import { readdirSync, readFileSync, statSync } from "node:fs";
import path from "node:path";
import ts from "typescript";
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
  it("never calls t() at module scope, where it runs before initLocale() (L-59)", () => {
    const offenders: string[] = [];
    for (const file of sources(path.resolve(__dirname, ".."))) {
      const source = ts.createSourceFile(file, readFileSync(file, "utf8"), ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
      const visit = (node: ts.Node, insideFunction: boolean) => {
        const isFunction = ts.isFunctionLike(node) || ts.isClassLike(node);
        if (
          !insideFunction &&
          ts.isCallExpression(node) &&
          ts.isIdentifier(node.expression) &&
          node.expression.text === "t"
        ) {
          const { line } = source.getLineAndCharacterOfPosition(node.getStart());
          offenders.push(`${path.basename(file)}:${line + 1}`);
        }
        ts.forEachChild(node, (child) => visit(child, insideFunction || isFunction));
      };
      visit(source, false);
    }
    expect(offenders).toEqual([]);
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

  // Key families built at runtime, so no literal t("...") names their members. `builder` is the
  // file that builds the key from a template. The test fails if that template goes away.
  const DYNAMIC_KEY_FAMILIES = [
    { prefix: "error.hint.", builder: "lib/errors.ts" },
    { prefix: "toolchain.ejs.", builder: "components/ToolchainPanel.tsx" },
    { prefix: "toolchain.source.", builder: "components/ToolchainPanel.tsx" },
    { prefix: "hash.algorithm.", builder: "components/ExpectedHashModal.tsx" },
  ];

  it("has no unused keys in en.json", () => {
    for (const { prefix, builder } of DYNAMIC_KEY_FAMILIES) {
      const builderText = readFileSync(path.resolve(__dirname, "..", builder), "utf8");
      expect(builderText, `${builder} no longer builds ${prefix}\${...}`).toContain(`\`${prefix}\${`);
      expect(Object.keys(table).some((key) => key.startsWith(prefix)), `no keys for ${prefix}`).toBe(true);
    }
    const dynamic = (key: string) => DYNAMIC_KEY_FAMILIES.some(({ prefix }) => key.startsWith(prefix));
    const files = allSources();
    const literal = (key: string) => files.some(({ text }) => text.includes(`"${key}"`) || text.includes(`'${key}'`));
    expect(Object.keys(table).filter((key) => !dynamic(key) && !literal(key))).toEqual([]);
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
