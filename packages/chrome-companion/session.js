/**
 * Pure helpers for the service worker, kept free of `chrome.*` so `node --test` can run them.
 */

/**
 * URL the shell will enqueue, and so the URL whose cookies belong in the hand-off.
 * Mirrors `CompanionRequest::effective_url` in companion-protocol: a link target wins over
 * the page it was found on, so cookies for the page must not be sent for a link elsewhere.
 * @param {{ pageUrl: string, mediaUrl?: string | null }} target
 * @returns {string}
 */
export function cookieSourceUrl({ pageUrl, mediaUrl = null }) {
  return mediaUrl && mediaUrl.trim() ? mediaUrl : pageUrl;
}

/**
 * Match pattern to request host access for `url`, or null when it is not http(s).
 * @param {string} url
 * @returns {string | null}
 */
export function originPattern(url) {
  try {
    const parsed = new URL(url);
    if (parsed.protocol !== "http:" && parsed.protocol !== "https:") return null;
    return `${parsed.origin}/*`;
  } catch {
    return null;
  }
}

/**
 * @param {Array<{name: string, value: string, domain: string, path?: string, secure: boolean, httpOnly: boolean, expirationDate?: number}>} cookies
 * @returns {import("./types.js").CompanionSession | null}
 */
export function toCompanionSession(cookies) {
  if (!cookies.length) return null;
  return {
    cookies: cookies.map((c) => ({
      name: c.name,
      value: c.value,
      domain: c.domain,
      path: c.path || "/",
      secure: c.secure,
      http_only: c.httpOnly,
      expiration_date: c.expirationDate ? Math.floor(c.expirationDate) : 0,
    })),
  };
}

/**
 * One-line message for the badge tooltip.
 * @param {unknown} failure thrown value, or a native-host response with `ok: false`
 * @returns {string}
 */
export function describeFailure(failure) {
  if (failure && typeof failure === "object") {
    if ("error" in failure && typeof failure.error === "string" && failure.error) {
      return failure.error;
    }
    if ("message" in failure && typeof failure.message === "string" && failure.message) {
      return failure.message;
    }
  }
  return "unknown error";
}
