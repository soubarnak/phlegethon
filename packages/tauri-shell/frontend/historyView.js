/**
 * History view — archive-backed list of completed (optionally failed) media.
 *
 * Reads from the Download Archive (single source of truth).  Actions:
 *   - Reveal in folder (uses reveal_output)
 *   - Open file (uses open_output)
 *   - Re-download (triggers probe for the URL)
 *   - Remove-from-History (removes archive entry only; files stay on disk)
 */

/**
 * @param {() => (cmd: string, args?: object) => Promise<any>} getInvoke
 * @param {object} options
 * @param {(url: string) => void} [options.onReDownload]
 */
export function createHistoryApi(getInvoke, options = {}) {
  const { onReDownload } = options;

  async function list() {
    return getInvoke()("list_history");
  }

  async function listByOutcome(outcome) {
    return getInvoke()("list_history_by_outcome", { outcome });
  }

  async function removeEntry(mediaId) {
    return getInvoke()("remove_history_entry", { mediaId });
  }

  async function count() {
    return getInvoke()("history_len");
  }

  return { list, listByOutcome, removeEntry, count, onReDownload };
}

/**
 * Render history entries into a container.
 *
 * @param {HTMLElement} container
 * @param {import("./historyView.js").HistoryApi} api
 * @param {Array} entries — list of ArchiveEntry objects
 * @param {object} [opts]
 * @param {string} [opts.filter] — "all" | "Completed" | "Failed"
 * @param {() => (cmd: string, args?: object) => Promise<any>} [opts.getInvoke]
 * @param {() => void} [opts.onEntriesChanged] — called after remove so caller can reload
 */
export function renderHistory(container, api, entries, opts = {}) {
  const filter = opts.filter || "all";
  const getInvoke = opts.getInvoke;
  const onEntriesChanged = opts.onEntriesChanged;

  const filtered =
    filter === "all" ? entries : entries.filter((e) => e.outcome === filter);

  if (filtered.length === 0) {
    container.innerHTML = `<div class="queue-empty"><span>${
      filter === "all"
        ? "No completed downloads yet."
        : filter === "Failed"
          ? "No failed downloads."
          : "No completed downloads."
    }</span></div>`;
    return;
  }

  container.innerHTML = filtered
    .map(
      (entry) => `
    <div class="history-row" data-media-id="${escapeHtml(entry.media_id)}" data-outcome="${escapeHtml(entry.outcome)}" data-url="${escapeHtml(entry.url)}" data-destination="${escapeHtml(entry.destination)}">
      <div class="history-row-main">
        <span class="history-title">${escapeHtml(entry.title)}</span>
        <span class="history-meta">
          ${entry.outcome === "Completed" ? '<span class="badge badge-success">Completed</span>' : '<span class="badge badge-error">Failed</span>'}
          &nbsp;·&nbsp; ${formatTimestamp(entry.completed_at_unix_secs)}
        </span>
      </div>
      <div class="history-row-actions">
        <button class="btn btn-sm history-reveal" type="button" title="Reveal in folder">Reveal</button>
        <button class="btn btn-sm history-open" type="button" title="Open file">Open</button>
        <button class="btn btn-sm history-redownload" type="button" title="Re-download">Re-download</button>
        <button class="btn btn-sm btn-danger history-remove" type="button" title="Remove from History">Remove</button>
      </div>
    </div>`
    )
    .join("");

  container.querySelectorAll(".history-reveal").forEach((btn) => {
    const row = btn.closest(".history-row");
    btn.addEventListener("click", () => {
      const destination = row.dataset.destination;
      if (destination && getInvoke) {
        getInvoke()("reveal_output", { path: destination }).catch((err) =>
          console.warn("Reveal failed:", err)
        );
      }
    });
  });

  container.querySelectorAll(".history-open").forEach((btn) => {
    const row = btn.closest(".history-row");
    btn.addEventListener("click", () => {
      const destination = row.dataset.destination;
      if (destination && getInvoke) {
        getInvoke()("open_output", { path: destination }).catch((err) =>
          console.warn("Open failed:", err)
        );
      }
    });
  });

  container.querySelectorAll(".history-redownload").forEach((btn) => {
    const row = btn.closest(".history-row");
    btn.addEventListener("click", () => {
      const url = row.dataset.url;
      if (url && api.onReDownload) {
        api.onReDownload(url);
      }
    });
  });

  container.querySelectorAll(".history-remove").forEach((btn) => {
    const row = btn.closest(".history-row");
    btn.addEventListener("click", async () => {
      const mediaId = row.dataset.mediaId;
      try {
        await api.removeEntry(mediaId);
        if (onEntriesChanged) {
          onEntriesChanged();
        } else {
          row.remove();
          if (container.children.length === 0) {
            renderHistory(
              container,
              api,
              entries.filter((e) => e.media_id !== mediaId),
              opts
            );
          }
        }
      } catch (err) {
        console.warn("Remove from History failed:", err);
      }
    });
  });
}

/**
 * Format a Unix timestamp as a human-readable date string.
 * @param {number} unixSecs
 */
function formatTimestamp(unixSecs) {
  const d = new Date(unixSecs * 1000);
  const now = new Date();
  const diffMs = now - d;

  if (diffMs < 60_000) return "Just now";
  if (diffMs < 3600_000) {
    const mins = Math.floor(diffMs / 60_000);
    return `${mins}m ago`;
  }
  if (d.toDateString() === now.toDateString()) {
    return d.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
  }
  const yesterday = new Date(now);
  yesterday.setDate(yesterday.getDate() - 1);
  if (d.toDateString() === yesterday.toDateString()) {
    return "Yesterday " + d.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
  }
  return d.toLocaleDateString(undefined, {
    month: "short",
    day: "numeric",
    year: d.getFullYear() !== now.getFullYear() ? "numeric" : undefined,
  });
}

function escapeHtml(str) {
  if (str == null) return "";
  const div = document.createElement("div");
  div.appendChild(document.createTextNode(String(str)));
  return div.innerHTML;
}
