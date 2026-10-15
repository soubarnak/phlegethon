/** Desktop notification policy (pure) + localStorage preference. */

const DESKTOP_NOTIFICATIONS_KEY = "phlegethon-desktop-notifications";

function safeLocalStorage(): Storage | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    return null;
  }
}

export function loadDesktopNotificationsEnabled(): boolean {
  const stored = safeLocalStorage()?.getItem(DESKTOP_NOTIFICATIONS_KEY) ?? null;
  return stored === null ? true : stored === "1";
}

export function saveDesktopNotificationsEnabled(enabled: boolean): void {
  safeLocalStorage()?.setItem(DESKTOP_NOTIFICATIONS_KEY, enabled ? "1" : "0");
}

/** Notify only when the setting is on and the app window is not focused. */
export function shouldShowDesktopNotification(settingsEnabled: boolean, appFocused: boolean): boolean {
  return settingsEnabled && !appFocused;
}

export interface NotificationPort {
  isPermissionGranted(): Promise<boolean>;
  requestPermission(): Promise<string>;
  sendNotification(options: { title: string; body: string }): void | Promise<void>;
}

export interface MaybeNotifyOptions {
  port: NotificationPort | null;
  isMainWindowFocused: () => Promise<boolean>;
  settingsEnabled: boolean;
  headline: string;
  body: string;
}

/** Returns true when a notification was sent. */
export async function maybeNotify({
  port,
  isMainWindowFocused,
  settingsEnabled,
  headline,
  body,
}: MaybeNotifyOptions): Promise<boolean> {
  if (!settingsEnabled || !port) return false;
  let focused = false;
  try {
    focused = await isMainWindowFocused();
  } catch {
    focused = false;
  }
  if (!shouldShowDesktopNotification(settingsEnabled, focused)) return false;
  try {
    let granted = await port.isPermissionGranted();
    if (!granted) {
      granted = (await port.requestPermission()) === "granted";
    }
    if (!granted) return false;
    await port.sendNotification({ title: headline, body });
    return true;
  } catch {
    return false;
  }
}
