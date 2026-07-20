/* ------------------------------------------------------------------ */
/*  Phlegethon — Frontend App Logic                                    */
/*  Dense row queue + dense Probe-then-Confirm sheet (variant A)       */
/*  Communicates with Download Core via Tauri commands                 */
/* ------------------------------------------------------------------ */

// Resolve Tauri APIs lazily — withGlobalTauri must be enabled in tauri.conf.json.
function getInvoke() {
  const invoke = window.__TAURI__?.core?.invoke;
  if (typeof invoke !== "function") {
    throw new Error(
      "Tauri API unavailable (window.__TAURI__). Open the desktop app window, not a plain browser tab.",
    );
  }
  return invoke;
}

// ---- State ----
const state = {
  jobs: [], // array of job objects from Rust
  pendingProbe: null, // { url, items, sourceName } — shown in probe sheet
  theme: "light",
  selectedJobId: null,
  /** Absolute Job Destination override for quick URL enqueue (null = Library Root default). */
  saveToDestination: null,
  /** Absolute Job Destination override for Probe-then-Confirm (null = Library Root default). */
  probeSaveToDestination: null,
  /** Monotonic id so stale preview_probe_destination responses are ignored. */
  probePreviewRequestId: 0,
  /** Pending Library Root path chosen in settings (applied via Core on Apply). */
  pendingLibraryRoot: null,
};

// ---- DOM refs ----
const $ = (id) => document.getElementById(id);
const urlInput = $("urlInput");
const enqueueBtn = $("enqueueBtn");
const queueList = $("queueList");
const queueEmpty = $("queueEmpty");
const probeOverlay = $("probeOverlay");
const probeTitle = $("probeTitle");
const probeUrlEl = $("probeUrl");
const probeItems = $("probeItems");
const probeConfirm = $("probeConfirm");
const probeCancel = $("probeCancel");
const probeClose = $("probeClose");
const probeSelectAll = $("probeSelectAll");
const probeDeselectAll = $("probeDeselectAll");
const themeToggle = $("themeToggle");
const dragOverlay = $("dragOverlay");
const saveToBtn = $("saveToBtn");
const saveToHint = $("saveToHint");
const saveToPath = $("saveToPath");
const saveToClear = $("saveToClear");
const probeSaveToBtn = $("probeSaveToBtn");
const probeSaveToPath = $("probeSaveToPath");
const settingsBtn = $("settingsBtn");
const settingsOverlay = $("settingsOverlay");
const settingsClose = $("settingsClose");
const libraryRootPath = $("libraryRootPath");
const libraryRootPick = $("libraryRootPick");
const libraryRootApply = $("libraryRootApply");

// ---- Theme ----
function setTheme(theme) {
  state.theme = theme;
  document.documentElement.setAttribute("data-theme", theme);
  themeToggle.textContent = theme === "dark" ? "Light" : "Dark";
  localStorage.setItem("phlegethon-theme", theme);
}

themeToggle.addEventListener("click", () => {
  setTheme(state.theme === "light" ? "dark" : "light");
});

// Restore saved theme
const savedTheme = localStorage.getItem("phlegethon-theme");
if (savedTheme) setTheme(savedTheme);

// ---- URL Enqueue ----
async function enqueueUrl(url) {
  if (!url || !url.trim()) {
    showError("Paste or type a URL first.");
    urlInput.focus();
    return;
  }
  const trimmed = url.trim();

  try {
    const invoke = getInvoke();
    const args = { url: trimmed };
    if (state.saveToDestination) {
      args.destination = state.saveToDestination;
    }
    const result = await invoke("enqueue_url", args);
    handleEnqueueResult(trimmed, result);
  } catch (err) {
    showError(`Enqueue failed: ${err}`);
  }
}

function handleEnqueueResult(url, result) {
  if (result.type === "EnqueueDirect") {
    addJobToState(result.job_id, url, result.destination);
    renderQueue();
    getInvoke()("process_pending").catch(() => {});
  } else if (result.type === "ProbeThenConfirm") {
    startProbe(url);
  } else {
    showError(`Unexpected enqueue result: ${JSON.stringify(result)}`);
  }
}

enqueueBtn.addEventListener("click", () => enqueueUrl(urlInput.value));
urlInput.addEventListener("keydown", (e) => {
  if (e.key === "Enter") {
    e.preventDefault();
    enqueueUrl(urlInput.value);
  }
});

// ---- Paste handler (Ctrl+V anywhere) ----
document.addEventListener("paste", (e) => {
  const text = e.clipboardData?.getData("text") || "";
  if (text && isValidUrl(text)) {
    e.preventDefault();
    urlInput.value = text;
    enqueueUrl(text);
  }
});

function isValidUrl(str) {
  try {
    new URL(str);
    return str.startsWith("http://") || str.startsWith("https://");
  } catch {
    return false;
  }
}

// ---- Drag & Drop ----
document.addEventListener("dragenter", (e) => {
  e.preventDefault();
  dragOverlay.classList.add("show");
});

document.addEventListener("dragover", (e) => {
  e.preventDefault();
  dragOverlay.classList.add("show");
});

document.addEventListener("dragleave", (e) => {
  e.preventDefault();
  if (
    e.relatedTarget === null ||
    e.relatedTarget === document.documentElement
  ) {
    dragOverlay.classList.remove("show");
  }
});

document.addEventListener("drop", (e) => {
  e.preventDefault();
  dragOverlay.classList.remove("show");

  const text = e.dataTransfer?.getData("text") || "";
  if (text && isValidUrl(text)) {
    enqueueUrl(text);
    return;
  }

  // Check for dropped files containing URLs
  const files = e.dataTransfer?.files;
  if (files && files.length > 0) {
    // Could read file contents — for now, just notify
    showError("File drop not yet supported. Paste URLs instead.");
  }
});

// ---- Probe ----
async function startProbe(url) {
  try {
    probeOverlay.classList.add("open");
    probeTitle.textContent = "Probing…";
    probeUrlEl.textContent = url;
    probeItems.innerHTML =
      '<div class="queue-empty" style="padding:2rem"><span>Analyzing URL and loading qualities…</span></div>';
    probeConfirm.disabled = true;
    probeSelectAll.disabled = true;
    probeDeselectAll.disabled = true;
    // Carry quick-URL Save to… into the probe sheet when present.
    if (state.saveToDestination && !state.probeSaveToDestination) {
      state.probeSaveToDestination = state.saveToDestination;
    }
    updateProbeSaveToHint();

    const result = await getInvoke()("probe_url", { url });
    showProbeResults(url, result);
  } catch (err) {
    probeOverlay.classList.remove("open");
    showError(`Probe failed: ${err}`);
  }
}

function showProbeResults(url, result) {
  probeTitle.textContent = result.source_name || "Probe Results";
  probeUrlEl.textContent = url;
  probeItems.innerHTML = "";

  if (!result.items || result.items.length === 0) {
    probeItems.innerHTML =
      '<div class="queue-empty" style="padding:2rem"><span>No items found.</span></div>';
    probeSelectAll.disabled = true;
    probeDeselectAll.disabled = true;
    return;
  }

  state.pendingProbe = {
    url,
    items: result.items,
    sourceName: result.source_name,
  };
  probeConfirm.disabled = false;
  probeSelectAll.disabled = false;
  probeDeselectAll.disabled = false;

  result.items.forEach((item, idx) => {
    // Format duration
    const dur = item.duration ? formatDuration(item.duration) : "";
    const row = document.createElement("div");
    row.className = "probe-item";
    row.dataset.itemIndex = idx;

    const checkbox = document.createElement("input");
    checkbox.type = "checkbox";
    checkbox.className = "probe-item-checkbox";
    checkbox.checked = true;
    checkbox.dataset.itemIndex = idx;

    const info = document.createElement("div");
    info.className = "probe-item-info";

    const title = document.createElement("div");
    title.className = "probe-item-title";
    title.textContent = item.title;
    info.appendChild(title);

    if (dur) {
      const duration = document.createElement("div");
      duration.className = "probe-item-duration";
      duration.textContent = dur;
      info.appendChild(duration);
    }

    const qualitiesWrap = document.createElement("div");
    qualitiesWrap.className = "probe-item-qualities";

    // Default quality selection
    const defaultQuality = findDefaultQuality(item.qualities);

    item.qualities.forEach((q) => {
      const chip = document.createElement("span");
      chip.className = "quality-chip";
      chip.textContent = q.label || q.id;
      chip.dataset.qualityId = q.id;
      chip.dataset.itemIndex = idx;

      if (q.id === defaultQuality) {
        chip.classList.add("selected");
      }

      chip.addEventListener("click", () => {
        // Deselect all qualities in this item, select this one
        qualitiesWrap.querySelectorAll(".quality-chip").forEach((c) => {
          c.classList.remove("selected");
        });
        chip.classList.add("selected");
        checkbox.checked = true;
        updateProbeSaveToHint();
      });

      qualitiesWrap.appendChild(chip);
    });

    row.appendChild(checkbox);
    row.appendChild(info);
    row.appendChild(qualitiesWrap);

    checkbox.addEventListener("change", () => {
      if (checkbox.checked) {
        selectDefaultQualityForRow(qualitiesWrap, defaultQuality);
      } else {
        qualitiesWrap.querySelectorAll(".quality-chip").forEach((c) => {
          c.classList.remove("selected");
        });
      }
      updateProbeSaveToHint();
    });

    probeItems.appendChild(row);
  });

  updateProbeSaveToHint();
}

function selectDefaultQualityForRow(qualitiesWrap, defaultQuality) {
  const chips = qualitiesWrap.querySelectorAll(".quality-chip");
  if (chips.length === 0) return;

  chips.forEach((c) => c.classList.remove("selected"));
  const target =
    qualitiesWrap.querySelector(`[data-quality-id="${defaultQuality}"]`) ||
    chips[0];
  target.classList.add("selected");
}

function setAllProbeItemsSelected(selected) {
  probeItems.querySelectorAll(".probe-item").forEach((row) => {
    const checkbox = row.querySelector(".probe-item-checkbox");
    const qualitiesWrap = row.querySelector(".probe-item-qualities");
    const itemIndex = parseInt(checkbox.dataset.itemIndex, 10);
    const item = state.pendingProbe?.items[itemIndex];
    if (!checkbox || !qualitiesWrap || !item) return;

    checkbox.checked = selected;
    if (selected) {
      selectDefaultQualityForRow(
        qualitiesWrap,
        findDefaultQuality(item.qualities),
      );
    } else {
      qualitiesWrap.querySelectorAll(".quality-chip").forEach((c) => {
        c.classList.remove("selected");
      });
    }
  });
  updateProbeSaveToHint();
}

probeSelectAll.addEventListener("click", () => setAllProbeItemsSelected(true));
probeDeselectAll.addEventListener("click", () =>
  setAllProbeItemsSelected(false),
);

function findDefaultQuality(qualities) {
  if (!qualities || qualities.length === 0) return null;
  // Prefer 720p or 1080p
  for (const prefix of ["1080p", "720p", "480p", "360p"]) {
    const found = qualities.find((q) => q.id.startsWith(prefix));
    if (found) return found.id;
  }
  return qualities[0].id;
}

function formatDuration(seconds) {
  const m = Math.floor(seconds / 60);
  const s = Math.floor(seconds % 60);
  return `${m}:${s.toString().padStart(2, "0")}`;
}

/** Collect currently checked probe items + qualities for Core APIs. */
function collectProbeSelectionItems() {
  const selectedItems = [];
  if (!state.pendingProbe) return selectedItems;

  probeItems.querySelectorAll(".probe-item").forEach((row) => {
    const checkbox = row.querySelector(".probe-item-checkbox");
    if (!checkbox?.checked) return;

    const itemIndex = parseInt(checkbox.dataset.itemIndex, 10);
    const item = state.pendingProbe.items[itemIndex];
    let selectedChip = row.querySelector(".quality-chip.selected");

    if (item && !selectedChip && item.qualities?.length) {
      const qualitiesWrap = row.querySelector(".probe-item-qualities");
      selectDefaultQualityForRow(
        qualitiesWrap,
        findDefaultQuality(item.qualities),
      );
      selectedChip = row.querySelector(".quality-chip.selected");
    }

    if (item && selectedChip) {
      selectedItems.push({
        item_id: item.id,
        quality_id: selectedChip.dataset.qualityId,
      });
    }
  });

  return selectedItems;
}

// Confirm probe selection
probeConfirm.addEventListener("click", async () => {
  if (!state.pendingProbe) return;

  const selectedItems = collectProbeSelectionItems();

  if (selectedItems.length === 0) {
    showError("Select at least one item.");
    return;
  }

  try {
    const selection = {
      url: state.pendingProbe.url,
      items: selectedItems,
    };

    const invoke = getInvoke();
    const args = { selection };
    if (state.probeSaveToDestination) {
      args.destination = state.probeSaveToDestination;
    }
    const jobIds = await invoke("confirm_probe_selection", args);
    probeOverlay.classList.remove("open");
    state.pendingProbe = null;
    state.probeSaveToDestination = null;
    updateProbeSaveToHint();

    await refreshJobs();
    jobIds.forEach((id) => {
      const job = state.jobs.find((j) => j.job_id === id);
      if (job) job.status = "downloading";
    });
    renderQueue();
    await runProcessPendingOnce();
  } catch (err) {
    showError(`Confirmation failed: ${err}`);
  }
});

// Close probe sheet
function closeProbe() {
  probeOverlay.classList.remove("open");
  state.pendingProbe = null;
  state.probeSaveToDestination = null;
  updateProbeSaveToHint();
}

probeCancel.addEventListener("click", closeProbe);
probeClose.addEventListener("click", closeProbe);
probeOverlay.addEventListener("click", (e) => {
  if (e.target === probeOverlay) closeProbe();
});

document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    if (!settingsOverlay.hidden) {
      closeSettings();
      return;
    }
    closeProbe();
  }
});

// ---- Queue State Management ----
function addJobToState(jobId, url, destination) {
  // Don't add duplicates
  if (state.jobs.some((j) => j.job_id === jobId)) return;

  state.jobs.unshift({
    job_id: jobId,
    url,
    destination,
    status: "pending",
    bytes_downloaded: 0,
    total_bytes: null,
    title: extractTitleFromUrl(url),
  });
}

function extractTitleFromUrl(url) {
  try {
    const u = new URL(url);
    const path = u.pathname.split("/").filter(Boolean).pop() || u.hostname;
    return decodeURIComponent(path);
  } catch {
    return url;
  }
}

function renderQueue() {
  if (state.jobs.length === 0) {
    queueEmpty.style.display = "flex";
    queueList.innerHTML = "";
    return;
  }

  queueEmpty.style.display = "none";

  let html = "";
  state.jobs.forEach((job) => {
    const hasTotal = job.total_bytes != null && job.total_bytes > 0;
    const pct = hasTotal
      ? Math.round((job.bytes_downloaded / job.total_bytes) * 100)
      : 0;

    const statusClass = `status-${job.status}`;
    const statusLabel =
      job.status.charAt(0).toUpperCase() + job.status.slice(1);
    const sizeLabel = hasTotal ? formatBytes(job.total_bytes) : "";
    const progressWidth = Math.min(pct, 100);
    const indeterminate =
      !hasTotal &&
      (job.status === "pending" || job.status === "downloading");

    html += `
      <div class="queue-row ${state.selectedJobId === job.job_id ? "selected" : ""}" data-job-id="${job.job_id}">
        <div class="row-info">
          <div class="row-title" title="${escapeHtml(job.title)}">${escapeHtml(job.title)}</div>
          <div class="row-url" title="${escapeHtml(job.url)}">${escapeHtml(job.url)}</div>
        </div>
        <div class="row-status ${statusClass}">${statusLabel}</div>
        <div class="row-progress">
          <div class="progress-bar${indeterminate ? " indeterminate" : ""}" role="progressbar"${
            hasTotal
              ? ` aria-valuenow="${progressWidth}" aria-valuemin="0" aria-valuemax="100"`
              : ` aria-valuetext="${formatBytes(job.bytes_downloaded)}"`
          }>
            <div class="progress-fill" style="${indeterminate ? "" : `width:${progressWidth}%`}"></div>
          </div>
        </div>
        <div class="row-meta">${formatBytes(job.bytes_downloaded)}${sizeLabel ? ` / ${sizeLabel}` : ""}</div>
        <div class="row-actions">
          ${job.status === "pending" || job.status === "downloading" ? `<button class="btn-icon btn-sm" data-action="pause" data-job-id="${job.job_id}" title="Pause">⏸</button>` : ""}
          ${job.status === "paused" ? `<button class="btn-icon btn-sm" data-action="resume" data-job-id="${job.job_id}" title="Resume">▶</button>` : ""}
          ${job.status === "failed" ? `<button class="btn-icon btn-sm" data-action="retry" data-job-id="${job.job_id}" title="Retry">↻</button>` : ""}
          <button class="btn-icon btn-sm btn-danger" data-action="discard" data-job-id="${job.job_id}" title="Discard">✕</button>
        </div>
      </div>
    `;
  });

  queueList.innerHTML = html;

  // Attach action handlers
  queueList.querySelectorAll("[data-action]").forEach((btn) => {
    btn.addEventListener("click", (e) => {
      e.stopPropagation();
      const action = btn.dataset.action;
      const jobId = btn.dataset.jobId;
      handleJobAction(action, jobId);
    });
  });

  // Select row
  queueList.querySelectorAll(".queue-row").forEach((row) => {
    row.addEventListener("click", () => {
      state.selectedJobId = row.dataset.jobId;
      renderQueue();
    });
  });
}

function escapeHtml(str) {
  const div = document.createElement("div");
  div.textContent = str;
  return div.innerHTML;
}

function formatBytes(bytes) {
  if (bytes === 0) return "0 B";
  const k = 1024;
  const sizes = ["B", "KB", "MB", "GB"];
  const i = Math.floor(Math.log(bytes) / Math.log(k));
  return parseFloat((bytes / Math.pow(k, i)).toFixed(1)) + " " + sizes[i];
}

// ---- Job Actions ----
async function handleJobAction(action, jobId) {
  try {
    const invoke = getInvoke();
    switch (action) {
      case "pause":
        await invoke("pause_job", { jobId });
        break;
      case "resume":
        await invoke("resume_job", { jobId });
        break;
      case "retry":
        await invoke("retry_job", { jobId });
        break;
      case "discard":
        state.jobs = state.jobs.filter((j) => j.job_id !== jobId);
        renderQueue();
        await invoke("discard_job", { jobId }).catch(() => {});
        return;
    }
    // Refresh state
    await refreshJobs();
    if (action === "resume" || action === "retry") {
      runProcessPendingOnce();
    }
  } catch (err) {
    showError(`${action} failed: ${err}`);
  }
}

async function refreshJobs() {
  try {
    const jobs = await getInvoke()("list_jobs");
    state.jobs = jobs;
    renderQueue();
    runProcessPendingOnce();
  } catch (err) {
    showError(`Failed to load queue: ${err}`);
  }
}

// ---- Process pending loop (matches WinUI ~5 Hz) ----
let processPendingTimer = null;
let processPendingInFlight = false;

function hasPendingJobs() {
  return state.jobs.some((job) => job.status === "pending");
}

async function runProcessPendingOnce() {
  if (processPendingInFlight || !hasPendingJobs()) return;

  processPendingInFlight = true;
  try {
    await getInvoke()("process_pending");
  } catch {
    /* silent */
  } finally {
    processPendingInFlight = false;
  }
}

function startProcessPendingLoop() {
  if (processPendingTimer) return;

  processPendingTimer = setInterval(() => {
    runProcessPendingOnce();
  }, 500);
}

// ---- Event polling (pull-based) ----
async function pollEvents() {
  try {
    const events = await getInvoke()("poll_events");
    events.forEach((evt) => {
      handleCoreEvent(evt);
    });
  } catch {
    /* silent */
  }
  // Poll every 500ms
  setTimeout(pollEvents, 500);
}

function handleCoreEvent(evt) {
  const job = state.jobs.find((j) => j.job_id === evt.job_id);
  if (!job) return;

  switch (evt.type) {
    case "JobProgress":
      job.bytes_downloaded = evt.bytes_downloaded;
      if (evt.total_bytes != null && evt.total_bytes > 0) {
        job.total_bytes = evt.total_bytes;
      }
      job.status = "downloading";
      break;
    case "JobCompleted":
      job.status = "completed";
      job.bytes_downloaded = evt.bytes_written || job.bytes_downloaded;
      break;
    case "JobFailed":
      job.status = "failed";
      break;
    case "JobPaused":
      job.status = "paused";
      break;
    case "JobResumed":
      job.status = "pending";
      break;
    case "JobRetried":
      job.status = "pending";
      break;
  }
  renderQueue();
}

// ---- Error display ----
let errorTimeout = null;

function showError(msg) {
  console.error(msg);
  const existing = document.querySelector(".error-toast");
  if (existing) existing.remove();

  const toast = document.createElement("div");
  toast.className = "error-toast";
  toast.textContent = msg;
  Object.assign(toast.style, {
    position: "fixed",
    bottom: "48px",
    left: "50%",
    transform: "translateX(-50%)",
    background: "var(--error-bg, #fee2e2)",
    color: "var(--error-fg, #991b1b)",
    padding: "8px 16px",
    borderRadius: "var(--radius-md, 8px)",
    fontFamily: "var(--font-sans, sans-serif)",
    fontSize: "var(--text-sm, 14px)",
    zIndex: "9999",
    border: "1px solid var(--border-error, #fca5a5)",
    boxShadow: "var(--shadow-lg, 0 8px 24px rgba(0,0,0,.2))",
    maxWidth: "80%",
    textAlign: "center",
  });
  document.body.appendChild(toast);

  if (errorTimeout) clearTimeout(errorTimeout);
  errorTimeout = setTimeout(() => toast.remove(), 8000);
}

// ---- Save to… (Job Destination override) ----
function updateSaveToHint() {
  if (state.saveToDestination) {
    saveToHint.hidden = false;
    saveToPath.textContent = state.saveToDestination;
  } else {
    saveToHint.hidden = true;
    saveToPath.textContent = "";
  }
}

function updateProbeSaveToHint() {
  if (!probeSaveToPath) return;

  const fallbackHint = () =>
    state.probeSaveToDestination
      ? state.probeSaveToDestination
      : "Library Root default";

  // Sync hint when probe sheet closed or still loading.
  if (!state.pendingProbe) {
    probeSaveToPath.textContent = fallbackHint();
    return;
  }

  const selectedItems = collectProbeSelectionItems();
  if (selectedItems.length === 0) {
    probeSaveToPath.textContent = fallbackHint();
    return;
  }

  const selection = {
    url: state.pendingProbe.url,
    items: selectedItems,
  };
  const args = { selection };
  if (state.probeSaveToDestination) {
    args.destination = state.probeSaveToDestination;
  }

  const requestId = ++state.probePreviewRequestId;
  getInvoke()("preview_probe_destination", args)
    .then((preview) => {
      if (requestId !== state.probePreviewRequestId) return;
      if (preview?.path_display) {
        probeSaveToPath.textContent = preview.path_display;
      }
    })
    .catch(() => {
      if (requestId !== state.probePreviewRequestId) return;
      probeSaveToPath.textContent = fallbackHint();
    });
}

async function pickAbsoluteFolder(title) {
  const invoke = getInvoke();
  const path = await invoke("pick_folder", { title });
  return path || null;
}

saveToBtn.addEventListener("click", async () => {
  try {
    const path = await pickAbsoluteFolder("Save to…");
    if (!path) return;
    state.saveToDestination = path;
    updateSaveToHint();
  } catch (err) {
    showError(`Save to… failed: ${err}`);
  }
});

saveToClear.addEventListener("click", () => {
  state.saveToDestination = null;
  updateSaveToHint();
});

probeSaveToBtn.addEventListener("click", async () => {
  try {
    const path = await pickAbsoluteFolder("Save to…");
    if (!path) return;
    state.probeSaveToDestination = path;
    updateProbeSaveToHint();
  } catch (err) {
    showError(`Save to… failed: ${err}`);
  }
});

// ---- Settings (Library Root) ----
async function openSettings() {
  try {
    const invoke = getInvoke();
    const root = await invoke("get_library_root");
    state.pendingLibraryRoot = root;
    libraryRootPath.value = root || "";
    settingsOverlay.hidden = false;
  } catch (err) {
    showError(`Settings failed: ${err}`);
  }
}

function closeSettings() {
  settingsOverlay.hidden = true;
}

settingsBtn.addEventListener("click", openSettings);
settingsClose.addEventListener("click", closeSettings);
settingsOverlay.addEventListener("click", (e) => {
  if (e.target === settingsOverlay) closeSettings();
});

libraryRootPick.addEventListener("click", async () => {
  try {
    const path = await pickAbsoluteFolder("Choose Library Root");
    if (!path) return;
    state.pendingLibraryRoot = path;
    libraryRootPath.value = path;
  } catch (err) {
    showError(`Choose Library Root failed: ${err}`);
  }
});

libraryRootApply.addEventListener("click", async () => {
  const path = (state.pendingLibraryRoot || libraryRootPath.value || "").trim();
  if (!path) {
    showError("Choose a Library Root folder first.");
    return;
  }
  try {
    const invoke = getInvoke();
    const applied = await invoke("set_library_root", { path });
    libraryRootPath.value = applied;
    state.pendingLibraryRoot = applied;
    closeSettings();
  } catch (err) {
    showError(`Apply Library Root failed: ${err}`);
  }
});

// ---- Init ----
document.addEventListener("DOMContentLoaded", async () => {
  urlInput.focus();
  updateSaveToHint();
  updateProbeSaveToHint();

  try {
    getInvoke();
  } catch (err) {
    showError(String(err.message || err));
    return;
  }

  pollEvents();
  startProcessPendingLoop();
  await refreshJobs();
});

window.addEventListener("error", (e) => {
  showError(`JS error: ${e.message}`);
});
window.addEventListener("unhandledrejection", (e) => {
  showError(`Promise error: ${e.reason}`);
});
