import { describe, expect, it, vi } from "vitest";
import { createClipboardWatcher } from "./clipboardWatcher";

function make(readValues: string[], urlsFor: (t: string) => string[]) {
  const detected: string[][] = [];
  let i = 0;
  let scheduled: (() => void) | null = null;
  const watcher = createClipboardWatcher({
    readClipboardText: async () => readValues[Math.min(i++, readValues.length - 1)],
    splitUrls: async (t) => urlsFor(t),
    onUrlsDetected: (u) => detected.push(u),
    isActive: () => true,
    setInterval: (fn) => {
      scheduled = fn;
      return 1;
    },
    clearInterval: () => {
      scheduled = null;
    },
  });
  return { watcher, detected, isScheduled: () => scheduled != null };
}

describe("clipboard watcher", () => {
  it("reports new URLs once and ignores unchanged clipboard contents", async () => {
    const { watcher, detected } = make(["https://a", "https://a", "https://b"], (t) => [t]);
    await watcher.tick();
    await watcher.tick();
    await watcher.tick();
    expect(detected).toEqual([["https://a"], ["https://b"]]);
  });
  it("ignores empty clipboards and read errors", async () => {
    const detected = vi.fn();
    const watcher = createClipboardWatcher({
      readClipboardText: async () => {
        throw new Error("denied");
      },
      splitUrls: async () => ["x"],
      onUrlsDetected: detected,
      isActive: () => true,
    });
    await watcher.tick();
    expect(detected).not.toHaveBeenCalled();
  });
  it("start/stop toggles the timer", () => {
    const { watcher, isScheduled } = make(["x"], () => []);
    watcher.start();
    expect(watcher.isRunning()).toBe(true);
    expect(isScheduled()).toBe(true);
    watcher.stop();
    expect(watcher.isRunning()).toBe(false);
  });

  it("does not send text without a scheme to the splitter (M-61)", async () => {
    const splitUrls = vi.fn(async (t: string) => [t]);
    const detected = vi.fn();
    const watcher = createClipboardWatcher({
      readClipboardText: async () => "just some words, no links",
      splitUrls,
      onUrlsDetected: detected,
    });
    await watcher.tick();
    expect(splitUrls).not.toHaveBeenCalled();
    expect(detected).not.toHaveBeenCalled();
  });
  it("does not read the clipboard while the window is hidden or unfocused (M-61)", async () => {
    let active = false;
    const read = vi.fn(async () => "https://a");
    const detected = vi.fn();
    const watcher = createClipboardWatcher({
      readClipboardText: read,
      splitUrls: async (t) => [t],
      onUrlsDetected: detected,
      isActive: () => active,
    });
    await watcher.tick();
    expect(read).not.toHaveBeenCalled();
    active = true;
    await watcher.tick();
    expect(detected).toHaveBeenCalledWith(["https://a"]);
  });
  it("does not offer text the app copied itself (M-61)", async () => {
    const detected = vi.fn();
    const watcher = createClipboardWatcher({
      readClipboardText: async () => "https://own.example/file",
      splitUrls: async (t) => [t],
      onUrlsDetected: detected,
      isOwnWrite: (text) => text === "https://own.example/file",
    });
    await watcher.tick();
    expect(detected).not.toHaveBeenCalled();
  });
});
