/**
 * Opt-in clipboard watcher — fills the URL input, never auto-enqueues.
 */
export interface ClipboardWatcherOptions {
  readClipboardText: () => Promise<string>;
  splitUrls: (text: string) => Promise<string[]>;
  onUrlsDetected: (urls: string[]) => void;
  intervalMs?: number;
  /** Injectable timer (tests). Defaults to window.setInterval/clearInterval. */
  setInterval?: (fn: () => void, ms: number) => number;
  clearInterval?: (id: number) => void;
}

export interface ClipboardWatcher {
  start(): void;
  stop(): void;
  isRunning(): boolean;
  /** Run one poll immediately (tests / manual refresh). */
  tick(): Promise<void>;
}

export function createClipboardWatcher({
  readClipboardText,
  splitUrls,
  onUrlsDetected,
  intervalMs = 1500,
  setInterval: setIntervalFn = (fn, ms) => window.setInterval(fn, ms),
  clearInterval: clearIntervalFn = (id) => window.clearInterval(id),
}: ClipboardWatcherOptions): ClipboardWatcher {
  let timer: number | null = null;
  let lastSeen: string | null = null;

  async function tick(): Promise<void> {
    try {
      const text = await readClipboardText();
      if (!text?.trim()) return;

      const urls = await splitUrls(text);
      if (urls.length === 0) return;

      const signature = urls.join("\n");
      if (signature === lastSeen) return;

      lastSeen = signature;
      onUrlsDetected(urls);
    } catch {
      /* ignore clipboard access errors */
    }
  }

  return {
    start() {
      if (timer != null) return;
      lastSeen = null;
      timer = setIntervalFn(() => {
        void tick();
      }, intervalMs);
    },
    stop() {
      if (timer == null) return;
      clearIntervalFn(timer);
      timer = null;
      lastSeen = null;
    },
    isRunning() {
      return timer != null;
    },
    tick,
  };
}
