/**
 * Thin adapters over Tauri plugins so components do not import plugin
 * modules directly (keeps tests simple: these are mocked as a unit).
 */
import { api } from "./api";
import type { NotificationPort } from "@/lib/notifications";
import type { PickFolderResult } from "./types";

/** True when running inside the Tauri webview (false in vitest / plain browser). */
export function isTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

/**
 * Folder picker: xdg-desktop-portal, else zenity/kdialog, via the Rust command.
 * `path` is null when cancelled; `method: "manual"` means no chooser exists and
 * the caller should ask for a typed path.
 */
export async function pickFolder(title?: string): Promise<PickFolderResult> {
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

let lastOwnClipboardWrite: string | null = null;

/** True when `text` is what this app last wrote to the clipboard (Copy URL, for example). */
export function isOwnClipboardWrite(text: string): boolean {
  return lastOwnClipboardWrite !== null && text === lastOwnClipboardWrite;
}

export async function writeClipboardText(text: string): Promise<void> {
  lastOwnClipboardWrite = text;
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
    sendNotification: async (options) => {
      try {
        await mod.sendNotification(options);
      } catch {
        // No notification daemon for the plugin: try notify-send instead.
        const sent = await api.notifyFallback(options.title, options.body).catch(() => false);
        if (!sent) throw new Error("no notification mechanism available");
      }
    },
  };
}
