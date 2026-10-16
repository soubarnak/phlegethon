import { t } from "./i18n";

/**
 * Core error strings may carry a machine-readable `[code]` prefix
 * (see download-core toolchain_discovery::tag_error).
 */
export type ErrorCode =
  | "needs_browser_cookies"
  | "needs_js_runtime"
  | "timeout"
  | "unsupported_url"
  | "geo_blocked"
  | "not_found";

const ERROR_CODES: readonly ErrorCode[] = [
  "needs_browser_cookies",
  "needs_js_runtime",
  "timeout",
  "unsupported_url",
  "geo_blocked",
  "not_found",
];

export interface ParsedError {
  code: ErrorCode | null;
  message: string;
}

export function parseCoreError(raw: unknown): ParsedError {
  const text = errorToString(raw);
  const match = /^\[([a-z_]+)\]\s*(.*)$/s.exec(text);
  if (!match) return { code: null, message: text };
  return { code: match[1] as ErrorCode, message: match[2] || text };
}

export function errorToString(raw: unknown): string {
  if (raw == null) return "";
  if (typeof raw === "string") return raw;
  if (raw instanceof Error) return raw.message;
  if (typeof raw === "object" && "message" in raw && typeof (raw as { message: unknown }).message === "string") {
    return (raw as { message: string }).message;
  }
  try {
    return JSON.stringify(raw);
  } catch {
    return String(raw);
  }
}

/** Short, action-oriented hint for a known code (mirrors Core's error_code_hint). */
export function errorHint(code: ErrorCode | null): string | null {
  return code && ERROR_CODES.includes(code) ? t(`error.hint.${code}`) : null;
}
