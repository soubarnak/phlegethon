import { describe, expect, it, vi } from "vitest";
import {
  loadDesktopNotificationsEnabled,
  maybeNotify,
  saveDesktopNotificationsEnabled,
  shouldShowDesktopNotification,
  type NotificationPort,
} from "./notifications";

function port(granted: boolean, request = "granted"): NotificationPort & { sent: unknown[] } {
  const sent: unknown[] = [];
  return {
    sent,
    isPermissionGranted: async () => granted,
    requestPermission: async () => request,
    sendNotification: (o) => void sent.push(o),
  };
}

describe("notification policy", () => {
  it("only when enabled and the window is unfocused", () => {
    expect(shouldShowDesktopNotification(true, false)).toBe(true);
    expect(shouldShowDesktopNotification(true, true)).toBe(false);
    expect(shouldShowDesktopNotification(false, false)).toBe(false);
  });
  it("persists the preference, default on", () => {
    expect(loadDesktopNotificationsEnabled()).toBe(true);
    saveDesktopNotificationsEnabled(false);
    expect(loadDesktopNotificationsEnabled()).toBe(false);
  });
  it("sends when allowed, skips when focused or permission denied", async () => {
    const p = port(true);
    expect(await maybeNotify({ port: p, isMainWindowFocused: async () => false, settingsEnabled: true, headline: "h", body: "b" })).toBe(true);
    expect(p.sent).toEqual([{ title: "h", body: "b" }]);
    expect(await maybeNotify({ port: p, isMainWindowFocused: async () => true, settingsEnabled: true, headline: "h", body: "b" })).toBe(false);
    const denied = port(false, "denied");
    expect(await maybeNotify({ port: denied, isMainWindowFocused: async () => false, settingsEnabled: true, headline: "h", body: "b" })).toBe(false);
    expect(await maybeNotify({ port: null, isMainWindowFocused: vi.fn(), settingsEnabled: true, headline: "h", body: "b" })).toBe(false);
  });
});
