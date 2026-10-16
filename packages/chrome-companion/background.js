/**
 * Phlegethon Chrome Companion — courier only.
 * Hands off page URL ± optional cookies via native messaging. No download intercept.
 */

import {
  cookieSourceUrl,
  describeFailure,
  originPattern,
  toCompanionSession,
} from "./session.js";

const NATIVE_HOST = "com.phlegethon.bridge";
const PROTOCOL_VERSION = 1;

chrome.runtime.onInstalled.addListener(async () => {
  // Menu ids survive an extension update; creating them again throws "duplicate id".
  await chrome.contextMenus.removeAll();
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

/** Show a failure on the toolbar icon; the next successful hand-off clears it. */
function showFailure(message) {
  console.error("Phlegethon hand-off failed:", message);
  chrome.action.setBadgeBackgroundColor({ color: "#da1e28" });
  chrome.action.setBadgeText({ text: "!" });
  chrome.action.setTitle({ title: `Phlegethon: ${message}` });
}

function clearFailure() {
  chrome.action.setBadgeText({ text: "" });
  chrome.action.setTitle({ title: "Send to Phlegethon" });
}

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

  const mediaUrl = linkUrl && linkUrl !== pageUrl ? linkUrl : null;
  // Ask for access before any other await, so the click still counts as a user gesture.
  const cookiesGranted = withCookies
    ? requestCookieAccess(cookieSourceUrl({ pageUrl: pageUrl ?? linkUrl, mediaUrl }))
    : Promise.resolve(false);

  await sendToPhlegethon({
    pageUrl: pageUrl ?? linkUrl,
    mediaUrl,
    withCookies,
    cookiesGranted,
  });
});

/**
 * @param {{ pageUrl: string, mediaUrl?: string | null, withCookies?: boolean, cookiesGranted?: Promise<boolean> }} opts
 */
async function sendToPhlegethon({
  pageUrl,
  mediaUrl = null,
  withCookies = false,
  cookiesGranted = Promise.resolve(false),
}) {
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
    // The shell checks every cookie domain against the URL it enqueues, which is the link
    // target when there is one, so collect for that URL and not for the page.
    const sourceUrl = cookieSourceUrl({ pageUrl, mediaUrl });
    try {
      envelope.session = await collectSessionCookies(sourceUrl, cookiesGranted);
    } catch (err) {
      showFailure(describeFailure(err));
      return;
    }
    if (!envelope.session) {
      showFailure("no cookies sent (permission denied or none for this site)");
      return;
    }
  }

  try {
    const response = await chrome.runtime.sendNativeMessage(NATIVE_HOST, envelope);
    if (!response?.ok) {
      showFailure(describeFailure(response));
      return;
    }
    clearFailure();
    console.info("Phlegethon hand-off ok:", response.result);
  } catch (err) {
    showFailure(describeFailure(err));
  }
}

/**
 * Ask for the `cookies` permission and host access to one origin, on demand (M-57).
 * Resolves false when the URL is not http(s) or the user declines.
 * @param {string} url
 * @returns {Promise<boolean>}
 */
async function requestCookieAccess(url) {
  const pattern = originPattern(url);
  if (!pattern) return false;
  try {
    return await chrome.permissions.request({ permissions: ["cookies"], origins: [pattern] });
  } catch (err) {
    console.error("Cookie permission request failed:", err);
    return false;
  }
}

/**
 * Collect the cookies the browser would send to `url` (explicit user gesture required).
 * @param {string} url
 * @param {Promise<boolean>} granted
 * @returns {Promise<import("./types.js").CompanionSession | null>}
 */
async function collectSessionCookies(url, granted) {
  if (!(await granted)) return null;
  const cookies = await chrome.cookies.getAll({ url: new URL(url).origin });
  return toCompanionSession(cookies);
}
