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
  switch (code) {
    case "needs_browser_cookies":
      return "The site blocked the request (login wall, bot check or Cloudflare challenge). Open the page in your browser once, then set Cookies → from browser in Settings and retry.";
    case "needs_js_runtime":
      return "yt-dlp needs a JavaScript runtime for this site. Install deno (≥ 2.3) or node (≥ 22) and check the Toolchain panel.";
    case "timeout":
      return "The probe took too long. Check the network, or raise the probe timeout in Settings.";
    case "geo_blocked":
      return "The site reports this media is not available in your region. A proxy in Settings may help.";
    case "not_found":
      return "The media could not be found; it may have been removed.";
    case "unsupported_url":
      return "yt-dlp does not support this URL and the page sniffer found no media.";
    default:
      return null;
  }
}
