/**
 * Locale wiring for Platform Shell. English is the shipped locale.
 * Additional locales: add strings/{locale}.json and extend SUPPORTED_LOCALES.
 */

import en from "../../shell-strings/en.json" with { type: "json" };

const TABLES = { en };
const SUPPORTED_LOCALES = ["en"];

let currentLocale = "en";

/**
 * @param {string} [locale]
 */
export function initLocale(locale = "en") {
  currentLocale = SUPPORTED_LOCALES.includes(locale) ? locale : "en";
  if (typeof document !== "undefined") {
    document.documentElement.lang = currentLocale;
  }
}

/**
 * @param {string} key
 * @param {Record<string, string | number>} [params]
 * @returns {string}
 */
export function t(key, params) {
  const table = TABLES[currentLocale] ?? TABLES.en;
  let value = table[key] ?? TABLES.en[key] ?? key;
  if (params) {
    for (const [name, replacement] of Object.entries(params)) {
      value = value.replaceAll(`{${name}}`, String(replacement));
    }
  }
  return value;
}

export function getCurrentLocale() {
  return currentLocale;
}

export function getSupportedLocales() {
  return [...SUPPORTED_LOCALES];
}
