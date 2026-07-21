/**
 * Phlegethon Chrome Companion — courier only.
 * Hands off page URL ± optional cookies via native messaging. No download intercept.
 */

const NATIVE_HOST = "com.phlegethon.bridge";
const PROTOCOL_VERSION = 1;

chrome.runtime.onInstalled.addListener(() => {
  chrome.contextMenus.create({
    id: "phlegethon-send-page",
    title: "Send page to Phlegethon",
    contexts: ["page"],
  });
  chrome.contextMenus.create({
    id: "phlegethon-send-link",
    title: "Send link to Phlegethon",
    contexts: ["link"],
  });
  chrome.contextMenus.create({
    id: "phlegethon-send-page-cookies",
    title: "Send page to Phlegethon (with cookies)",
    contexts: ["page"],
  });
  chrome.contextMenus.create({
    id: "phlegethon-send-link-cookies",
    title: "Send link to Phlegethon (with cookies)",
    contexts: ["link"],
  });
});

chrome.action.onClicked.addListener(async (tab) => {
  if (!tab?.url) return;
  await sendToPhlegethon({ pageUrl: tab.url, withCookies: false });
});

chrome.contextMenus.onClicked.addListener(async (info, tab) => {
  const withCookies =
    info.menuItemId === "phlegethon-send-page-cookies" ||
    info.menuItemId === "phlegethon-send-link-cookies";

  const pageUrl = tab?.url ?? info.pageUrl;
  const linkUrl = info.linkUrl;

  if (!pageUrl && !linkUrl) return;

  await sendToPhlegethon({
    pageUrl: pageUrl ?? linkUrl,
    mediaUrl: linkUrl && linkUrl !== pageUrl ? linkUrl : null,
    withCookies,
  });
});

/**
 * @param {{ pageUrl: string, mediaUrl?: string | null, withCookies?: boolean }} opts
 */
async function sendToPhlegethon({ pageUrl, mediaUrl = null, withCookies = false }) {
  const requestId = crypto.randomUUID();
  /** @type {import("./types.js").CompanionRequest} */
  const envelope = {
    v: PROTOCOL_VERSION,
    op: "enqueue",
    request_id: requestId,
    page_url: pageUrl,
    media_url: mediaUrl,
    intent: "probe_then_confirm",
    referrer: pageUrl,
    session: null,
  };

  if (withCookies) {
    envelope.session = await collectSessionCookies(pageUrl);
  }

  try {
    const response = await chrome.runtime.sendNativeMessage(NATIVE_HOST, envelope);
    if (!response?.ok) {
      console.error("Phlegethon hand-off failed:", response?.error ?? "unknown error");
      return;
    }
    console.info("Phlegethon hand-off ok:", response.result);
  } catch (err) {
    console.error("Phlegethon native messaging error:", err);
  }
}

/**
 * Collect same-origin cookies for the page URL (explicit user gesture required).
 * @param {string} pageUrl
 * @returns {Promise<import("./types.js").CompanionSession | null>}
 */
async function collectSessionCookies(pageUrl) {
  try {
    const origin = new URL(pageUrl).origin;
    const cookies = await chrome.cookies.getAll({ url: origin });
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
  } catch (err) {
    console.error("Failed to collect cookies:", err);
    return null;
  }
}
