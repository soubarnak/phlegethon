/**
 * Opt-in clipboard watcher — fills the URL input, never auto-enqueues.
 */
export interface ClipboardWatcherOptions {
  readClipboardText: () => Promise<string>;
  splitUrls: (text: string) => Promise<string[]>;
  onUrlsDetected: (urls: string[]) => void;
  intervalMs?: number;
  /** Polling runs only while this returns true. Defaults to a visible, focused window. */
  isActive?: () => boolean;
  /** True for text the app put on the clipboard itself, which is never offered back. */
  isOwnWrite?: (text: string) => boolean;
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

function defaultIsActive(): boolean {
  return typeof document === "undefined" || (!document.hidden && document.hasFocus());
}

export function createClipboardWatcher({
  readClipboardText,
  splitUrls,
  onUrlsDetected,
  intervalMs = 1500,
  isActive = defaultIsActive,
  isOwnWrite = () => false,
  setInterval: setIntervalFn = (fn, ms) => window.setInterval(fn, ms),
  clearInterval: clearIntervalFn = (id) => window.clearInterval(id),
}: ClipboardWatcherOptions): ClipboardWatcher {
  let timer: number | null = null;
  let lastSeen: string | null = null;

  async function tick(): Promise<void> {
    try {
      if (!isActive()) return;
      const text = await readClipboardText();
      if (!text?.trim()) return;
      // Cheap prefilter: only text that can hold a URL goes over IPC to the splitter.
      if (!text.includes("://") || isOwnWrite(text)) return;

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
