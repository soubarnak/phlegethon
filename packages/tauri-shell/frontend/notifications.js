/** @typedef {import("@tauri-apps/api/core").Invoke} InvokeFn */

const DESKTOP_NOTIFICATIONS_KEY = "phlegethon-desktop-notifications";

export function loadDesktopNotificationsEnabled() {
  const stored = localStorage.getItem(DESKTOP_NOTIFICATIONS_KEY);
  return stored === null ? true : stored === "1";
}

export function saveDesktopNotificationsEnabled(enabled) {
  localStorage.setItem(DESKTOP_NOTIFICATIONS_KEY, enabled ? "1" : "0");
}

/** Pure helper for unit tests. */
export function shouldShowDesktopNotification(settingsEnabled, appFocused) {
  return settingsEnabled && !appFocused;
}

function getNotificationApi() {
  return window.__TAURI__?.notification;
}

async function ensureNotificationPermission(notificationApi) {
  let granted = await notificationApi.isPermissionGranted();
  if (!granted) {
    granted = (await notificationApi.requestPermission()) === "granted";
  }
  return granted;
}

/**
 * @param {{
 *   invoke: InvokeFn,
 *   settingsEnabled: boolean,
 *   jobId: string,
 *   title: string,
 *   headline: string,
 *   detail: string,
 * }} options
 */
export async function maybeNotifyJob({
  invoke,
  settingsEnabled,
  jobId,
  title,
  headline,
  detail,
}) {
  if (!settingsEnabled) {
    return;
  }

  const focused = await invoke("is_main_window_focused");
  if (!shouldShowDesktopNotification(settingsEnabled, focused)) {
    return;
  }

  const notificationApi = getNotificationApi();
  if (!notificationApi?.sendNotification) {
    return;
  }

  if (!(await ensureNotificationPermission(notificationApi))) {
    return;
  }

  notificationApi.sendNotification({
    title: headline,
    body: `${title}: ${detail}`,
    extra: { jobId },
  });
}

/**
 * @param {{ invoke: InvokeFn, onJobFocus: (jobId: string) => void }} options
 */
export async function initNotificationActivation({ invoke, onJobFocus }) {
  const notificationApi = getNotificationApi();
  if (!notificationApi?.onAction) {
    return;
  }

  await notificationApi.onAction((payload) => {
    const jobId = payload?.extra?.jobId;
    if (!jobId) {
      return;
    }

    void invoke("focus_main_window");
    onJobFocus(String(jobId));
  });
}
