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
});
