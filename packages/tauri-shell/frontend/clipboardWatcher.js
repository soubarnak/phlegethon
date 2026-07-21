/**
 * Opt-in clipboard watcher — fills input, never auto-enqueues (#57).
 *
 * @param {object} opts
 * @param {() => Promise<string>} opts.readClipboardText
 * @param {(text: string) => Promise<string[]>} opts.splitUrls
 * @param {(urls: string[]) => void} opts.onUrlsDetected
 * @param {number} [opts.intervalMs]
 */
export function createClipboardWatcher({
  readClipboardText,
  splitUrls,
  onUrlsDetected,
  intervalMs = 1500,
}) {
  /** @type {number | null} */
  let timer = null;
  /** @type {string | null} */
  let lastSeen = null;

  async function tick() {
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
      timer = window.setInterval(() => {
        void tick();
      }, intervalMs);
    },
    stop() {
      if (timer == null) return;
      window.clearInterval(timer);
      timer = null;
      lastSeen = null;
    },
    isRunning() {
      return timer != null;
    },
  };
}
