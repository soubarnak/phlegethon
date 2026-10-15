/**
 * Locale wiring for the Platform Shell. English is the shipped locale.
 * Additional locales: add packages/shell-strings/{locale}.json and extend SUPPORTED_LOCALES.
 */
import en from "@strings/en.json";

type StringTable = Record<string, string>;

const TABLES: Record<string, StringTable> = { en: en as StringTable };
const SUPPORTED_LOCALES = ["en"] as const;

let currentLocale = "en";

export function initLocale(locale = "en"): void {
  currentLocale = (SUPPORTED_LOCALES as readonly string[]).includes(locale) ? locale : "en";
  if (typeof document !== "undefined") {
    document.documentElement.lang = currentLocale;
  }
}

export function t(key: string, params?: Record<string, string | number>): string {
  const table = TABLES[currentLocale] ?? TABLES.en;
  let value = table[key] ?? TABLES.en[key] ?? key;
  if (params) {
    for (const [name, replacement] of Object.entries(params)) {
      value = value.replaceAll(`{${name}}`, String(replacement));
    }
  }
  return value;
}

export function getCurrentLocale(): string {
  return currentLocale;
}

export function getSupportedLocales(): string[] {
  return [...SUPPORTED_LOCALES];
}
