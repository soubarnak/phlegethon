/**
 * Thin adapters over Tauri plugins so components do not import plugin
 * modules directly (keeps tests simple: these are mocked as a unit).
 */
import { api } from "./api";
import type { NotificationPort } from "@/lib/notifications";

/** True when running inside the Tauri webview (false in vitest / plain browser). */
export function isTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

/** Folder picker: portal/zenity/kdialog via the Rust command; null when cancelled. */
export async function pickFolder(title?: string): Promise<string | null> {
  return api.pickFolder(title);
}

export async function readClipboardText(): Promise<string> {
  if (!isTauri()) {
    try {
      return await navigator.clipboard.readText();
    } catch {
      return "";
    }
  }
  const { readText } = await import("@tauri-apps/plugin-clipboard-manager");
  return (await readText()) ?? "";
}

export async function writeClipboardText(text: string): Promise<void> {
  if (!isTauri()) {
    await navigator.clipboard.writeText(text);
    return;
  }
  const { writeText } = await import("@tauri-apps/plugin-clipboard-manager");
  await writeText(text);
}

export async function notificationPort(): Promise<NotificationPort | null> {
  if (!isTauri()) return null;
  const mod = await import("@tauri-apps/plugin-notification");
  return {
    isPermissionGranted: () => mod.isPermissionGranted(),
    requestPermission: () => mod.requestPermission(),
    sendNotification: (options) => mod.sendNotification(options),
  };
}

export async function openExternal(url: string): Promise<void> {
  if (!isTauri()) {
    window.open(url, "_blank", "noopener");
    return;
  }
  const { openUrl } = await import("@tauri-apps/plugin-opener");
  await openUrl(url);
}
