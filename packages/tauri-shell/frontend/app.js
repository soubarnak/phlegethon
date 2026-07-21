/* ------------------------------------------------------------------ */
/*  Phlegethon — Frontend App Logic                                    */
/*  Dense row queue + dense Probe-then-Confirm sheet (variant A)       */
/*  Communicates with Download Core via Tauri commands                 */
/* ------------------------------------------------------------------ */

import { buildDiscardConfirmCopy } from "./discardConfirm.js";
import { displayProgressPercent } from "./progressPercent.js";
import { createClipboardWatcher } from "./clipboardWatcher.js";
import { initLocale, t } from "./i18n.js";
import { focusFirst, trapFocus } from "./focusTrap.js";
import {
  initNotificationActivation,
  loadDesktopNotificationsEnabled,
  maybeNotifyJob,
  saveDesktopNotificationsEnabled,
} from "./notifications.js";
import {
  createUrlInputApi,
  processUrlInput,
  resolveDuplicateUrl,
} from "./urlInput.js";
import {
  QUEUE_FILTER_ALL,
  QUEUE_SORT_QUEUE,
  applyQueueView,
} from "./queueView.js";
import {
  createHistoryApi,
  renderHistory,
} from "./historyView.js";

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
  /** Per-probe media extras override (pre-filled from settings). */
  probeMediaExtras: null,
  mediaExtrasCapabilities: null,
  clipboardWatcherEnabled: false,
  desktopNotificationsEnabled: true,
  queueSearch: "",
  queueFilter: QUEUE_FILTER_ALL,
  queueSort: QUEUE_SORT_QUEUE,
  /** Cleanup for probe sheet focus trap. */
  releaseProbeFocusTrap: null,
};

const urlInputApi = createUrlInputApi(getInvoke);

// ---- DOM refs ----
const $ = (id) => document.getElementById(id);
const urlInput = $("urlInput");
const enqueueBtn = $("enqueueBtn");
const queueList = $("queueList");
const queueEmpty = $("queueEmpty");
const queueEmptyMessage = $("queueEmptyMessage");
const queueSearch = $("queueSearch");
const queueFilter = $("queueFilter");
const queueSort = $("queueSort");
const probeOverlay = $("probeOverlay");
const probeSheet = probeOverlay?.querySelector(".probe-sheet");
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
const activeJobLimitInput = $("activeJobLimitInput");
const activeJobLimitApply = $("activeJobLimitApply");
const audioOnlyModeInput = $("audioOnlyModeInput");
const audioOnlyModeApply = $("audioOnlyModeApply");
const probeMediaExtras = $("probeMediaExtras");
const probeMediaExtrasNote = $("probeMediaExtrasNote");
const mediaExtrasFfmpegNote = $("mediaExtrasFfmpegNote");
const writeSubsDefault = $("writeSubsDefault");
const writeThumbnailDefault = $("writeThumbnailDefault");
const embedSubsDefault = $("embedSubsDefault");
const remuxDefault = $("remuxDefault");
const mediaExtrasApply = $("mediaExtrasApply");
const cookiesFromBrowserInput = $("cookiesFromBrowserInput");
const cookieFileInput = $("cookieFileInput");
const cookieChannelApply = $("cookieChannelApply");
const proxyUrlInput = $("proxyUrlInput");
const proxySettingsApply = $("proxySettingsApply");
const checkSidecarUpdatesOnLaunchInput = $("checkSidecarUpdatesOnLaunchInput");
const checkSidecarUpdatesBtn = $("checkSidecarUpdatesBtn");
const applySidecarUpdatesBtn = $("applySidecarUpdatesBtn");
const toolchainPinInput = $("toolchainPinInput");
const pinToolchainBtn = $("pinToolchainBtn");
const rollbackToolchainBtn = $("rollbackToolchainBtn");
const pluginPinInput = $("pluginPinInput");
const pinPluginBtn = $("pinPluginBtn");
const rollbackPluginBtn = $("rollbackPluginBtn");
const sidecarUpdateStatus = $("sidecarUpdateStatus");
const filenameTemplateInput = $("filenameTemplateInput");
const filenameTemplateApply = $("filenameTemplateApply");
const discardConfirmOverlay = $("discardConfirmOverlay");
const discardConfirmTitle = $("discardConfirmTitle");
const discardConfirmBody = $("discardConfirmBody");
const discardConfirmCancel = $("discardConfirmCancel");
const discardConfirmOk = $("discardConfirmOk");
const duplicateConfirmOverlay = $("duplicateConfirmOverlay");
const duplicateConfirmTitle = $("duplicateConfirmTitle");
const duplicateConfirmBody = $("duplicateConfirmBody");
const duplicateConfirmCancel = $("duplicateConfirmCancel");
const duplicateConfirmFocus = $("duplicateConfirmFocus");
const duplicateConfirmEnqueue = $("duplicateConfirmEnqueue");
const clipboardWatcherInput = $("clipboardWatcherInput");
const desktopNotificationsInput = $("desktopNotificationsInput");

/** @type {{ resolve: (choice: "enqueue" | "focus" | "cancel") => void } | null} */
let duplicateConfirmWaiter = null;

initLocale();

function applyStaticLabels() {
  urlInput.placeholder = t("url.placeholder");
  urlInput.setAttribute("aria-label", t("url.placeholder"));
  enqueueBtn.textContent = t("url.enqueue");
  saveToBtn.textContent = t("url.saveTo");
  saveToBtn.setAttribute("aria-label", t("url.saveTo"));
  queueSearch.placeholder = t("queue.search.placeholder");
  queueSearch.setAttribute("aria-label", t("queue.search.label"));
  queueFilter.setAttribute("aria-label", t("queue.filter.label"));
  queueSort.setAttribute("aria-label", t("queue.sort.label"));
  const filterOptions = [
    ["all", "queue.filter.all"],
    ["queued", "queue.filter.queued"],
    ["downloading", "queue.filter.downloading"],
    ["paused", "queue.filter.paused"],
    ["completed", "queue.filter.completed"],
    ["failed", "queue.filter.failed"],
  ];
  for (const [value, key] of filterOptions) {
    const option = queueFilter.querySelector(`option[value="${value}"]`);
    if (option) option.textContent = t(key);
  }
  const sortOptions = [
    ["queue", "queue.sort.queue"],
    ["title", "queue.sort.title"],
    ["status", "queue.sort.status"],
    ["size", "queue.sort.size"],
  ];
  for (const [value, key] of sortOptions) {
    const option = queueSort.querySelector(`option[value="${value}"]`);
    if (option) option.textContent = t(key);
  }
  probeSelectAll.textContent = t("probe.selectAll");
  probeDeselectAll.textContent = t("probe.deselectAll");
  probeCancel.textContent = t("probe.cancel");
  probeConfirm.textContent = t("probe.confirm");
  probeSaveToBtn.textContent = t("probe.saveTo");
  probeSaveToBtn.setAttribute("aria-label", t("probe.saveTo"));
  probeClose.setAttribute("aria-label", t("probe.close"));
  probeSaveToPath.textContent = t("probe.libraryRootDefault");
}

applyStaticLabels();

/** @type {{ resolve: (ok: boolean) => void } | null} */
let discardConfirmWaiter = null;

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
async function enqueueOneUrl(url) {
  const args = { url };
  if (state.saveToDestination) {
    args.destination = state.saveToDestination;
  }
  const result = await getInvoke()("enqueue_url", args);
  handleEnqueueResult(url, result);
}

async function enqueueUrlInput(text) {
  if (!text || !text.trim()) {
    showError("Paste or type a URL first.");
    urlInput.focus();
    return;
  }

  try {
    const { enqueued, total } = await processUrlInput(
      {
        splitUrls: (value) => urlInputApi.splitUrls(value),
        resolveDuplicate: (url) =>
          resolveDuplicateUrl(
            {
              findActiveJobForUrl: (value) =>
                urlInputApi.findActiveJobForUrl(value),
              confirmDuplicate: confirmDuplicate,
              focusExisting: focusExistingJob,
            },
            url,
          ),
        enqueueOne: enqueueOneUrl,
      },
      text.trim(),
    );

    if (total === 0) {
      showError("No valid HTTP(S) URLs found.");
      return;
    }

    if (enqueued === 0) {
      return;
    }

    urlInput.value = "";
  } catch (err) {
    showError(`Enqueue failed: ${err}`);
  }
}

async function enqueueUrl(url) {
  await enqueueUrlInput(url);
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

enqueueBtn.addEventListener("click", () => enqueueUrlInput(urlInput.value));
urlInput.addEventListener("keydown", (e) => {
  if (e.key === "Enter" && !e.shiftKey) {
    e.preventDefault();
    enqueueUrlInput(urlInput.value);
  }
});

function closeDuplicateConfirm(choice) {
  if (!duplicateConfirmOverlay.hidden) {
    duplicateConfirmOverlay.hidden = true;
  }
  const waiter = duplicateConfirmWaiter;
  duplicateConfirmWaiter = null;
  if (waiter) {
    waiter.resolve(choice);
  }
}

function confirmDuplicate(match) {
  duplicateConfirmTitle.textContent = "URL already active";
  duplicateConfirmBody.textContent = `This URL is already ${match.status}:\n${match.url}`;
  duplicateConfirmOverlay.hidden = false;
  duplicateConfirmFocus.focus();

  return new Promise((resolve) => {
    duplicateConfirmWaiter = { resolve };
  });
}

duplicateConfirmCancel.addEventListener("click", () =>
  closeDuplicateConfirm("cancel"),
);
duplicateConfirmFocus.addEventListener("click", () =>
  closeDuplicateConfirm("focus"),
);
duplicateConfirmEnqueue.addEventListener("click", () =>
  closeDuplicateConfirm("enqueue"),
);
duplicateConfirmOverlay.addEventListener("click", (e) => {
  if (e.target === duplicateConfirmOverlay) {
    closeDuplicateConfirm("cancel");
  }
});

async function focusExistingJob(match) {
  if (match.job_id) {
    focusJobRow(match.job_id);
    return;
  }

  showError("URL is already being probed.");
}

// ---- Paste handler (Ctrl+V anywhere) ----
document.addEventListener("paste", (e) => {
  const text = e.clipboardData?.getData("text") || "";
  if (!text.trim()) return;

  e.preventDefault();
  urlInput.value = text;
  void enqueueUrlInput(text);
});

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
  if (text.trim()) {
    urlInput.value = text;
    void enqueueUrlInput(text);
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
const MEDIA_EXTRA_FIELDS = [
  { key: "write_subs", label: "Write subs", write: true },
  { key: "embed_subs", label: "Embed subs", write: false },
  { key: "write_thumbnail", label: "Write thumbnail", write: true },
  { key: "embed_thumbnail", label: "Embed thumbnail", write: false },
  { key: "write_metadata", label: "Write metadata", write: true },
  { key: "embed_metadata", label: "Embed metadata", write: false },
  { key: "write_chapters", label: "Write chapters", write: true },
  { key: "embed_chapters", label: "Embed chapters", write: false },
  { key: "remux", label: "Remux to mp4", write: false },
];

async function loadMediaExtrasCapabilities() {
  try {
    state.mediaExtrasCapabilities = await getInvoke()(
      "get_media_extras_capabilities",
    );
  } catch {
    state.mediaExtrasCapabilities = {
      ffmpeg_available: false,
      embed_disabled_reason: "ffmpeg not found",
    };
  }
  return state.mediaExtrasCapabilities;
}

function applyEmbedAvailability(root, caps) {
  const embedAllowed = Boolean(caps?.ffmpeg_available);
  const note = caps?.embed_disabled_reason;
  const noteEl = root.querySelector("[data-embed-note]");
  if (noteEl) {
    if (!embedAllowed && note) {
      noteEl.textContent = note;
      noteEl.hidden = false;
    } else {
      noteEl.hidden = true;
    }
  }
  root.querySelectorAll("[data-embed-only]").forEach((input) => {
    input.disabled = !embedAllowed;
    if (!embedAllowed) input.checked = false;
  });
}

function renderMediaExtrasPanel(container, options, caps, noteEl) {
  container.innerHTML = "";
  const wrap = document.createElement("div");
  wrap.className = "probe-media-grid";
  if (noteEl) {
    noteEl.dataset.embedNote = "1";
    noteEl.className = "probe-media-note";
  }

  for (const field of MEDIA_EXTRA_FIELDS) {
    const label = document.createElement("label");
    label.className = "settings-checkbox";
    const input = document.createElement("input");
    input.type = "checkbox";
    input.dataset.mediaKey = field.key;
    if (!field.write) input.dataset.embedOnly = "1";
    input.checked = Boolean(options?.[field.key]);
    label.appendChild(input);
    label.appendChild(document.createTextNode(` ${field.label}`));
    wrap.appendChild(label);
  }

  container.appendChild(wrap);
  applyEmbedAvailability(container, caps);
  if (noteEl && !caps?.ffmpeg_available && caps?.embed_disabled_reason) {
    noteEl.textContent = caps.embed_disabled_reason;
    noteEl.hidden = false;
  } else if (noteEl) {
    noteEl.hidden = true;
  }
}

function readMediaExtrasFromRoot(root) {
  const options = {};
  root.querySelectorAll("[data-media-key]").forEach((input) => {
    options[input.dataset.mediaKey] = input.checked;
  });
  return options;
}

async function startProbe(url) {
  try {
    openProbeSheet();
    probeTitle.textContent = t("probe.probingTitle");
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
    await showProbeResults(url, result);
  } catch (err) {
    closeProbe();
    showError(`Probe failed: ${err}`);
  }
}

async function showProbeResults(url, result) {
  probeTitle.textContent = result.source_name || t("probe.results");
  probeUrlEl.textContent = url;
  probeItems.innerHTML = "";

  const caps = await loadMediaExtrasCapabilities();
  let defaults = {};
  try {
    defaults = await getInvoke()("get_default_media_extras");
  } catch {
    defaults = {};
  }
  state.probeMediaExtras = { ...defaults };
  renderMediaExtrasPanel(probeMediaExtras, state.probeMediaExtras, caps, probeMediaExtrasNote);

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

  for (let idx = 0; idx < result.items.length; idx++) {
    const item = result.items[idx];
    const dur = item.duration ? formatDuration(item.duration) : "";
    const row = document.createElement("div");
    row.className = "probe-item";
    if (item.archived) {
      row.classList.add("probe-item-archived");
    }
    row.dataset.itemIndex = idx;

    const checkbox = document.createElement("input");
    checkbox.type = "checkbox";
    checkbox.className = "probe-item-checkbox";
    checkbox.checked = !item.archived;
    checkbox.dataset.itemIndex = idx;
    checkbox.setAttribute(
      "aria-label",
      t("probe.selectItem", { title: item.title }),
    );

    const info = document.createElement("div");
    info.className = "probe-item-info";

    const title = document.createElement("div");
    title.className = "probe-item-title";
    title.textContent = item.title;
    info.appendChild(title);

    if (item.archived) {
      const badge = document.createElement("div");
      badge.className = "probe-item-archived-badge";
      badge.textContent = t("probe.downloaded");
      info.appendChild(badge);
    }

    if (dur) {
      const duration = document.createElement("div");
      duration.className = "probe-item-duration";
      duration.textContent = dur;
      info.appendChild(duration);
    }

    const qualitiesWrap = document.createElement("div");
    qualitiesWrap.className = "probe-item-qualities";
    qualitiesWrap.setAttribute("role", "radiogroup");
    qualitiesWrap.setAttribute(
      "aria-label",
      t("probe.qualityFor", { title: item.title }),
    );

    const defaultQuality = await resolveDefaultQuality(item.qualities);
    row.dataset.defaultQuality = defaultQuality || "";

    item.qualities.forEach((q) => {
      const chip = document.createElement("button");
      chip.type = "button";
      chip.className = "quality-chip";
      chip.setAttribute("role", "radio");
      chip.textContent = q.label || q.id;
      chip.dataset.qualityId = q.id;
      chip.dataset.itemIndex = idx;
      chip.setAttribute(
        "aria-label",
        t("probe.qualityFor", { title: item.title }) + `: ${q.label || q.id}`,
      );

      if (q.id === defaultQuality) {
        chip.classList.add("selected");
        chip.setAttribute("aria-checked", "true");
      } else {
        chip.setAttribute("aria-checked", "false");
      }

      chip.addEventListener("click", () => {
        qualitiesWrap.querySelectorAll(".quality-chip").forEach((c) => {
          c.classList.remove("selected");
          c.setAttribute("aria-checked", "false");
        });
        chip.classList.add("selected");
        chip.setAttribute("aria-checked", "true");
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
        selectDefaultQualityForRow(qualitiesWrap, row.dataset.defaultQuality);
      } else {
        qualitiesWrap.querySelectorAll(".quality-chip").forEach((c) => {
          c.classList.remove("selected");
        });
      }
      updateProbeSaveToHint();
    });

    probeItems.appendChild(row);
  }

  updateProbeSaveToHint();
  focusFirst(probeSheet);
}

function selectDefaultQualityForRow(qualitiesWrap, defaultQuality) {
  const chips = qualitiesWrap.querySelectorAll(".quality-chip");
  if (chips.length === 0) return;

  chips.forEach((c) => {
    c.classList.remove("selected");
    c.setAttribute("aria-checked", "false");
  });
  const target =
    qualitiesWrap.querySelector(`[data-quality-id="${defaultQuality}"]`) ||
    chips[0];
  target.classList.add("selected");
  target.setAttribute("aria-checked", "true");
}

async function setAllProbeItemsSelected(selected) {
  for (const row of probeItems.querySelectorAll(".probe-item")) {
    const checkbox = row.querySelector(".probe-item-checkbox");
    const qualitiesWrap = row.querySelector(".probe-item-qualities");
    const itemIndex = parseInt(checkbox.dataset.itemIndex, 10);
    const item = state.pendingProbe?.items[itemIndex];
    if (!checkbox || !qualitiesWrap || !item) continue;

    checkbox.checked = selected;
    if (selected) {
      const defaultQuality =
        row.dataset.defaultQuality ||
        (await resolveDefaultQuality(item.qualities));
      row.dataset.defaultQuality = defaultQuality || "";
      selectDefaultQualityForRow(qualitiesWrap, defaultQuality);
    } else {
      qualitiesWrap.querySelectorAll(".quality-chip").forEach((c) => {
        c.classList.remove("selected");
        c.setAttribute("aria-checked", "false");
      });
    }
  }
  updateProbeSaveToHint();
}

probeSelectAll.addEventListener("click", () => {
  void setAllProbeItemsSelected(true);
});
probeDeselectAll.addEventListener("click", () => {
  void setAllProbeItemsSelected(false);
});

async function resolveDefaultQuality(qualities) {
  if (!qualities || qualities.length === 0) return null;
  try {
    const selected = await getInvoke()("select_default_quality", { qualities });
    return selected || qualities[0].id;
  } catch {
    return qualities[0].id;
  }
}

function formatDuration(seconds) {
  const m = Math.floor(seconds / 60);
  const s = Math.floor(seconds % 60);
  return `${m}:${s.toString().padStart(2, "0")}`;
}

/** Read checked rows with an already-selected quality chip (sync). */
function collectSelectedItemsFromDom() {
  const selectedItems = [];
  if (!state.pendingProbe) return selectedItems;

  probeItems.querySelectorAll(".probe-item").forEach((row) => {
    const checkbox = row.querySelector(".probe-item-checkbox");
    if (!checkbox?.checked) return;

    const itemIndex = parseInt(checkbox.dataset.itemIndex, 10);
    const item = state.pendingProbe.items[itemIndex];
    const selectedChip = row.querySelector(".quality-chip.selected");

    if (item && selectedChip) {
      selectedItems.push({
        item_id: item.id,
        quality_id: selectedChip.dataset.qualityId,
      });
    }
  });

  return selectedItems;
}

/** Collect probe selection, filling in Core default qualities when needed. */
async function collectProbeSelectionItems() {
  const selectedItems = [];
  if (!state.pendingProbe) return selectedItems;

  for (const row of probeItems.querySelectorAll(".probe-item")) {
    const checkbox = row.querySelector(".probe-item-checkbox");
    if (!checkbox?.checked) continue;

    const itemIndex = parseInt(checkbox.dataset.itemIndex, 10);
    const item = state.pendingProbe.items[itemIndex];
    let selectedChip = row.querySelector(".quality-chip.selected");

    if (item && !selectedChip && item.qualities?.length) {
      const qualitiesWrap = row.querySelector(".probe-item-qualities");
      const defaultQuality =
        row.dataset.defaultQuality ||
        (await resolveDefaultQuality(item.qualities));
      selectDefaultQualityForRow(qualitiesWrap, defaultQuality);
      selectedChip = row.querySelector(".quality-chip.selected");
    }

    if (item && selectedChip) {
      selectedItems.push({
        item_id: item.id,
        quality_id: selectedChip.dataset.qualityId,
      });
    }
  }

  return selectedItems;
}

// Confirm probe selection
probeConfirm.addEventListener("click", async () => {
  if (!state.pendingProbe) return;

  const selectedItems = await collectProbeSelectionItems();

  if (selectedItems.length === 0) {
    showError("Select at least one item.");
    return;
  }

  try {
    const selection = {
      url: state.pendingProbe.url,
      items: selectedItems,
      media_extras: readMediaExtrasFromRoot(probeOverlay),
    };

    const invoke = getInvoke();
    const args = { selection };
    if (state.probeSaveToDestination) {
      args.destination = state.probeSaveToDestination;
    }
    const jobIds = await invoke("confirm_probe_selection", args);
    closeProbe();

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
function openProbeSheet() {
  probeOverlay.classList.add("open");
  state.releaseProbeFocusTrap?.();
  if (probeSheet) {
    state.releaseProbeFocusTrap = trapFocus(probeSheet, { onEscape: closeProbe });
  }
}

function closeProbe() {
  probeOverlay.classList.remove("open");
  state.releaseProbeFocusTrap?.();
  state.releaseProbeFocusTrap = null;
  state.pendingProbe = null;
  state.probeSaveToDestination = null;
  updateProbeSaveToHint();
  urlInput.focus();
}

probeCancel.addEventListener("click", closeProbe);
probeClose.addEventListener("click", closeProbe);
probeOverlay.addEventListener("click", (e) => {
  if (e.target === probeOverlay) closeProbe();
});

document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    if (!duplicateConfirmOverlay.hidden) {
      closeDuplicateConfirm("cancel");
      return;
    }
    if (!discardConfirmOverlay.hidden) {
      closeDiscardConfirm(false);
      return;
    }
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

function visibleJobs() {
  return applyQueueView(state.jobs, {
    search: state.queueSearch,
    stateFilter: state.queueFilter,
    sort: state.queueSort,
  });
}

function coreIndexOfJob(jobId) {
  return state.jobs.findIndex((j) => j.job_id === jobId);
}

function renderQueue() {
  const viewJobs = visibleJobs();
  const hasJobs = state.jobs.length > 0;

  if (viewJobs.length === 0) {
    queueEmpty.style.display = "flex";
    queueEmptyMessage.textContent = hasJobs
      ? t("queue.emptyFiltered")
      : t("queue.emptyWithHint");
    queueList.innerHTML = "";
    return;
  }

  queueEmpty.style.display = "none";

  let html = "";
  viewJobs.forEach((job) => {
    const coreIndex = coreIndexOfJob(job.job_id);
    const pct = displayProgressPercent(job.bytes_downloaded, job.total_bytes);
    const hasTotal = pct != null;

    const statusClass = `status-${job.status}`;
    const statusLabel =
      job.status.charAt(0).toUpperCase() + job.status.slice(1);
    const sizeLabel = hasTotal ? formatBytes(job.total_bytes) : "";
    const progressWidth = pct ?? 0;
    const indeterminate =
      !hasTotal &&
      (job.status === "pending" || job.status === "downloading");
    const locationPath =
      job.status === "completed" && job.path
        ? job.path
        : job.destination || "";
    let locationLabel = "";
    if (job.status === "completed" && job.path) {
      locationLabel = job.destination
        ? `${job.path} · Save to ${job.destination}`
        : job.path;
    } else if (job.destination) {
      locationLabel = `Save to ${job.destination}`;
    }

    html += `
      <div class="queue-row ${state.selectedJobId === job.job_id ? "selected" : ""}" data-job-id="${job.job_id}" role="listitem">
        <div class="row-info">
          <div class="row-title" title="${escapeHtml(job.title)}">${escapeHtml(job.title)}</div>
          <div class="row-url" title="${escapeHtml(job.url)}">${escapeHtml(job.url)}</div>
          ${
            locationLabel
              ? `<div class="row-location" title="${escapeHtml(locationPath)}">${escapeHtml(locationLabel)}</div>`
              : ""
          }
        </div>
        <div class="row-status ${statusClass}" aria-label="${escapeHtml(statusLabel)}">${statusLabel}</div>
        <div class="row-progress">
          <div class="progress-bar${indeterminate ? " indeterminate" : ""}" role="progressbar"${
            hasTotal
              ? ` aria-valuenow="${progressWidth}" aria-valuemin="0" aria-valuemax="100"`
              : ` aria-valuetext="${formatBytes(job.bytes_downloaded)}"`
          }>
            <div class="progress-fill" style="${indeterminate ? "" : `width:${progressWidth}%`}"></div>
          </div>
        </div>
        <div class="row-meta">${formatBytes(job.bytes_downloaded)}${sizeLabel ? ` / ${sizeLabel}` : ""}${hasTotal ? ` (${pct}%)` : ""}</div>
        <div class="row-actions">
          ${coreIndex > 0 ? `<button class="btn-icon btn-sm" data-action="reorder-up" data-job-id="${job.job_id}" aria-label="${t("queue.action.moveUp")}" title="${t("queue.action.moveUp")}">↑</button>` : ""}
          ${coreIndex >= 0 && coreIndex < state.jobs.length - 1 ? `<button class="btn-icon btn-sm" data-action="reorder-down" data-job-id="${job.job_id}" aria-label="${t("queue.action.moveDown")}" title="${t("queue.action.moveDown")}">↓</button>` : ""}
          ${job.status === "pending" || job.status === "downloading" ? `<button class="btn-icon btn-sm" data-action="pause" data-job-id="${job.job_id}" aria-label="${t("queue.action.pause")}" title="${t("queue.action.pause")}">⏸</button>` : ""}
          ${job.status === "paused" ? `<button class="btn-icon btn-sm" data-action="resume" data-job-id="${job.job_id}" aria-label="${t("queue.action.resume")}" title="${t("queue.action.resume")}">▶</button>` : ""}
          ${job.status === "failed" ? `<button class="btn-icon btn-sm" data-action="retry" data-job-id="${job.job_id}" aria-label="${t("queue.action.retry")}" title="${t("queue.action.retry")}">↻</button>` : ""}
          ${job.status === "completed" && job.path ? `<button class="btn-icon btn-sm" data-action="reveal" data-job-id="${job.job_id}" aria-label="${t("queue.action.reveal")}" title="${t("queue.action.reveal")}">${t("queue.action.reveal")}</button>` : ""}
          ${job.status === "completed" && job.path ? `<button class="btn-icon btn-sm" data-action="open" data-job-id="${job.job_id}" aria-label="${t("queue.action.open")}" title="${t("queue.action.open")}">${t("queue.action.open")}</button>` : ""}
          <button class="btn-icon btn-sm btn-danger" data-action="discard" data-job-id="${job.job_id}" aria-label="${t("queue.action.discard")}" title="${t("queue.action.discard")}">✕</button>
        </div>
      </div>
    `;
  });

  queueList.innerHTML = html;
  queueList.setAttribute("role", "list");
  queueList.setAttribute("aria-label", t("queue.title"));

  // Attach action handlers
  queueList.querySelectorAll("[data-action]").forEach((btn) => {
    btn.addEventListener("click", (e) => {
      e.stopPropagation();
      const action = btn.dataset.action;
      const jobId = btn.dataset.jobId;
      if (action === "reorder-up" || action === "reorder-down") {
        handleReorder(action, jobId);
        return;
      }
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
function closeDiscardConfirm(confirmed) {
  if (!discardConfirmOverlay.hidden) {
    discardConfirmOverlay.hidden = true;
  }
  const waiter = discardConfirmWaiter;
  discardConfirmWaiter = null;
  if (waiter) {
    waiter.resolve(Boolean(confirmed));
  }
}

/**
 * Always-confirm Discard. Copy differs for Completed vs unfinished.
 * @param {{ status: string, destination?: string }} job
 * @returns {Promise<boolean>}
 */
function confirmDiscard(job) {
  const copy = buildDiscardConfirmCopy(job.status, job.destination || "");
  discardConfirmTitle.textContent = copy.title;
  discardConfirmBody.textContent = copy.body;
  discardConfirmOk.textContent = copy.confirmLabel;
  discardConfirmCancel.textContent = copy.cancelLabel;
  discardConfirmOverlay.hidden = false;
  discardConfirmOk.focus();

  return new Promise((resolve) => {
    discardConfirmWaiter = { resolve };
  });
}

discardConfirmCancel.addEventListener("click", () => closeDiscardConfirm(false));
discardConfirmOk.addEventListener("click", () => closeDiscardConfirm(true));
discardConfirmOverlay.addEventListener("click", (e) => {
  if (e.target === discardConfirmOverlay) {
    closeDiscardConfirm(false);
  }
});

async function handleReorder(action, jobId) {
  const fromIndex = coreIndexOfJob(jobId);
  if (fromIndex < 0) return;
  const toIndex = action === "reorder-up" ? fromIndex - 1 : fromIndex + 1;
  if (toIndex < 0 || toIndex >= state.jobs.length) return;
  try {
    await getInvoke()("reorder_job", { fromIndex, toIndex });
    await refreshJobs();
  } catch (err) {
    showError(`Reorder failed: ${err}`);
  }
}

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
      case "discard": {
        const job = state.jobs.find((j) => j.job_id === jobId);
        if (!job) return;
        const confirmed = await confirmDiscard(job);
        if (!confirmed) return;
        await invoke("discard_job", { jobId });
        state.jobs = state.jobs.filter((j) => j.job_id !== jobId);
        if (state.selectedJobId === jobId) {
          state.selectedJobId = null;
        }
        renderQueue();
        return;
      }
      case "reveal": {
        const revealJob = state.jobs.find((j) => j.job_id === jobId);
        if (!revealJob?.path) return;
        await invoke("reveal_output", { path: revealJob.path });
        return;
      }
      case "open": {
        const openJob = state.jobs.find((j) => j.job_id === jobId);
        if (!openJob?.path) return;
        await invoke("open_output", { path: openJob.path });
        return;
      }
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
    // Preserve Completed final paths across snapshot refresh.
    const pathById = new Map(
      state.jobs.filter((j) => j.path).map((j) => [j.job_id, j.path]),
    );
    state.jobs = jobs.map((job) => {
      const path = pathById.get(job.job_id);
      return path ? { ...job, path } : job;
    });
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
  if (evt.type === "JobDiscarded") {
    state.jobs = state.jobs.filter((j) => j.job_id !== evt.job_id);
    if (state.selectedJobId === evt.job_id) {
      state.selectedJobId = null;
    }
    renderQueue();
    return;
  }

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
      if (evt.path) {
        job.path = evt.path;
      }
      void notifyJobOutcome(
        job.job_id,
        job.title || "Download",
        "Download completed",
        evt.path || "",
      );
      break;
    case "JobFailed":
      job.status = "failed";
      void notifyJobOutcome(
        job.job_id,
        job.title || "Download",
        "Download failed",
        evt.error || "Unknown error",
      );
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

async function notifyJobOutcome(jobId, title, headline, detail) {
  try {
    await maybeNotifyJob({
      invoke: getInvoke(),
      settingsEnabled: state.desktopNotificationsEnabled,
      jobId,
      title,
      headline,
      detail,
    });
  } catch {
    /* silent */
  }
}

function focusJobRow(jobId) {
  state.selectedJobId = jobId;
  renderQueue();
  const row = queueList.querySelector(`[data-job-id="${jobId}"]`);
  row?.scrollIntoView({ block: "nearest" });
  row?.querySelector(".row-actions button")?.focus();
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
      : t("probe.libraryRootDefault");

  // Sync hint when probe sheet closed or still loading.
  if (!state.pendingProbe) {
    probeSaveToPath.textContent = fallbackHint();
    return;
  }

  const selectedItems = collectSelectedItemsFromDom();
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
        const multiNote =
          selectedItems.length > 1
            ? " (each item under its own subfolder)"
            : "";
        probeSaveToPath.textContent = `${preview.path_display}${multiNote}`;
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
    const limit = await invoke("get_active_job_limit");
    const audioOnly = await invoke("get_audio_only_mode");
    const mediaDefaults = await invoke("get_default_media_extras");
    const cookieChannel = await invoke("get_cookie_channel");
    const proxySettings = await invoke("get_proxy_settings");
    const sidecarSettings = await invoke("get_sidecar_update_settings");
    const filenameTemplate = await invoke("get_filename_template");
    const caps = await loadMediaExtrasCapabilities();
    state.pendingLibraryRoot = root;
    libraryRootPath.value = root || "";
    activeJobLimitInput.value = String(limit);
    audioOnlyModeInput.checked = Boolean(audioOnly);
    writeSubsDefault.checked = Boolean(mediaDefaults.write_subs);
    writeThumbnailDefault.checked = Boolean(mediaDefaults.write_thumbnail);
    embedSubsDefault.checked = Boolean(mediaDefaults.embed_subs);
    remuxDefault.checked = Boolean(mediaDefaults.remux);
    cookiesFromBrowserInput.value = cookieChannel.cookies_from_browser || "";
    cookieFileInput.value = cookieChannel.cookie_file || "";
    proxyUrlInput.value = proxySettings.url || "";
    checkSidecarUpdatesOnLaunchInput.checked = Boolean(sidecarSettings.check_on_launch);
    toolchainPinInput.value = sidecarSettings.toolchain_pin || "";
    pluginPinInput.value = sidecarSettings.plugin_pin || "";
    sidecarUpdateStatus.textContent = "";
    filenameTemplateInput.value = filenameTemplate || "";
    const watcherPref = localStorage.getItem("phlegethon-clipboard-watcher") === "1";
    clipboardWatcherInput.checked = watcherPref;
    state.desktopNotificationsEnabled = loadDesktopNotificationsEnabled();
    desktopNotificationsInput.checked = state.desktopNotificationsEnabled;
    applyEmbedAvailability(settingsOverlay, caps);
    if (!caps.ffmpeg_available && caps.embed_disabled_reason) {
      mediaExtrasFfmpegNote.textContent = caps.embed_disabled_reason;
      mediaExtrasFfmpegNote.hidden = false;
    } else {
      mediaExtrasFfmpegNote.hidden = true;
    }
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

activeJobLimitApply.addEventListener("click", async () => {
  const limit = Number.parseInt(activeJobLimitInput.value, 10);
  if (!Number.isFinite(limit) || limit < 1) {
    showError("Active Job Limit must be at least 1.");
    return;
  }
  try {
    await getInvoke()("set_active_job_limit", { limit });
    closeSettings();
  } catch (err) {
    showError(`Apply Active Job Limit failed: ${err}`);
  }
});

audioOnlyModeApply.addEventListener("click", async () => {
  try {
    await getInvoke()("set_audio_only_mode", {
      enabled: audioOnlyModeInput.checked,
    });
    closeSettings();
  } catch (err) {
    showError(`Apply audio-only mode failed: ${err}`);
  }
});

mediaExtrasApply.addEventListener("click", async () => {
  try {
    await getInvoke()("set_default_media_extras", {
      options: {
        write_subs: writeSubsDefault.checked,
        embed_subs: embedSubsDefault.checked,
        write_chapters: false,
        embed_chapters: false,
        write_thumbnail: writeThumbnailDefault.checked,
        embed_thumbnail: false,
        write_metadata: false,
        embed_metadata: false,
        remux: remuxDefault.checked,
      },
    });
    closeSettings();
  } catch (err) {
    showError(`Apply media defaults failed: ${err}`);
  }
});

cookieChannelApply.addEventListener("click", async () => {
  try {
    const browser = cookiesFromBrowserInput.value.trim();
    const cookieFile = cookieFileInput.value.trim();
    await getInvoke()("set_cookie_channel", {
      channel: {
        cookies_from_browser: browser || null,
        cookie_file: cookieFile || null,
      },
    });
    closeSettings();
  } catch (err) {
    showError(`Apply cookie channel failed: ${err}`);
  }
});

proxySettingsApply.addEventListener("click", async () => {
  try {
    const url = proxyUrlInput.value.trim();
    await getInvoke()("set_proxy_settings", {
      settings: {
        url: url || null,
      },
    });
    closeSettings();
  } catch (err) {
    showError(`Apply proxy settings failed: ${err}`);
  }
});

async function persistSidecarUpdateSettings() {
  await getInvoke()("set_sidecar_update_settings", {
    settings: {
      check_on_launch: checkSidecarUpdatesOnLaunchInput.checked,
      toolchain_pin: toolchainPinInput.value.trim() || null,
      plugin_pin: pluginPinInput.value.trim() || null,
    },
  });
}

checkSidecarUpdatesOnLaunchInput.addEventListener("change", async () => {
  try {
    await persistSidecarUpdateSettings();
  } catch (err) {
    showError(`Save sidecar settings failed: ${err}`);
  }
});

function formatSidecarStatus(result) {
  const tc = result.toolchain;
  const pl = result.plugin;
  return [
    `Toolchain ${tc.installed_version || "none"} → ${tc.available_version || "none"}${tc.update_available ? " (update)" : ""}`,
    `Plugin ${pl.installed_version || "none"} → ${pl.available_version || "none"}${pl.update_available ? " (update)" : ""}`,
  ].join("; ");
}

checkSidecarUpdatesBtn.addEventListener("click", async () => {
  try {
    await persistSidecarUpdateSettings();
    const result = await getInvoke()("check_sidecar_updates");
    sidecarUpdateStatus.textContent = formatSidecarStatus(result);
  } catch (err) {
    showError(`Check sidecar updates failed: ${err}`);
  }
});

applySidecarUpdatesBtn.addEventListener("click", async () => {
  try {
    await persistSidecarUpdateSettings();
    const result = await getInvoke()("apply_sidecar_updates");
    sidecarUpdateStatus.textContent = `Apply: ${result.outcome}`;
  } catch (err) {
    showError(`Apply sidecar updates failed: ${err}`);
  }
});

pinToolchainBtn.addEventListener("click", async () => {
  try {
    const version = toolchainPinInput.value.trim();
    if (!version) {
      showError("Enter a toolchain version to pin");
      return;
    }
    await getInvoke()("pin_sidecar_version", { component: "toolchain", version });
    sidecarUpdateStatus.textContent = `Pinned toolchain to ${version}`;
  } catch (err) {
    showError(`Pin toolchain failed: ${err}`);
  }
});

rollbackToolchainBtn.addEventListener("click", async () => {
  try {
    await getInvoke()("rollback_sidecar", { component: "toolchain" });
    const result = await getInvoke()("check_sidecar_updates");
    sidecarUpdateStatus.textContent = `Rolled back toolchain. ${formatSidecarStatus(result)}`;
  } catch (err) {
    showError(`Rollback toolchain failed: ${err}`);
  }
});

pinPluginBtn.addEventListener("click", async () => {
  try {
    const version = pluginPinInput.value.trim();
    if (!version) {
      showError("Enter a plugin version to pin");
      return;
    }
    await getInvoke()("pin_sidecar_version", { component: "plugin", version });
    sidecarUpdateStatus.textContent = `Pinned plugin to ${version}`;
  } catch (err) {
    showError(`Pin plugin failed: ${err}`);
  }
});

rollbackPluginBtn.addEventListener("click", async () => {
  try {
    await getInvoke()("rollback_sidecar", { component: "plugin" });
    const result = await getInvoke()("check_sidecar_updates");
    sidecarUpdateStatus.textContent = `Rolled back plugin. ${formatSidecarStatus(result)}`;
  } catch (err) {
    showError(`Rollback plugin failed: ${err}`);
  }
});

filenameTemplateApply.addEventListener("click", async () => {
  try {
    const template = filenameTemplateInput.value.trim();
    if (!template) {
      showError("Filename template cannot be empty");
      return;
    }
    await getInvoke()("set_filename_template", { template });
    closeSettings();
  } catch (err) {
    showError(`Apply filename template failed: ${err}`);
  }
});

queueSearch?.addEventListener("input", () => {
  state.queueSearch = queueSearch.value;
  renderQueue();
});

queueFilter?.addEventListener("change", () => {
  state.queueFilter = queueFilter.value;
  renderQueue();
});

queueSort?.addEventListener("change", () => {
  state.queueSort = queueSort.value;
  renderQueue();
});

const clipboardWatcher = createClipboardWatcher({
  readClipboardText: async () => {
    if (!navigator.clipboard?.readText) {
      throw new Error("clipboard unavailable");
    }
    return navigator.clipboard.readText();
  },
  splitUrls: (text) => urlInputApi.splitUrls(text),
  onUrlsDetected(urls) {
    urlInput.value = urls.join("\n");
    showError(
      urls.length === 1
        ? "URL detected in clipboard — review and press Enqueue."
        : `${urls.length} URLs detected in clipboard — review and press Enqueue.`,
    );
  },
});

function applyClipboardWatcherPreference(enabled) {
  state.clipboardWatcherEnabled = enabled;
  localStorage.setItem(
    "phlegethon-clipboard-watcher",
    enabled ? "1" : "0",
  );
  if (enabled) {
    clipboardWatcher.start();
  } else {
    clipboardWatcher.stop();
  }
}

clipboardWatcherInput?.addEventListener("change", () => {
  applyClipboardWatcherPreference(clipboardWatcherInput.checked);
});

desktopNotificationsInput?.addEventListener("change", () => {
  state.desktopNotificationsEnabled = desktopNotificationsInput.checked;
  saveDesktopNotificationsEnabled(state.desktopNotificationsEnabled);
});

// ---- Tab switching ----
const tabBar = document.getElementById("tabBar");
const queueTab = document.getElementById("queueTab");
const historyTab = document.getElementById("historyTab");

tabBar?.addEventListener("click", (e) => {
  const btn = e.target.closest(".tab-btn");
  if (!btn) return;
  const tab = btn.dataset.tab;

  tabBar.querySelectorAll(".tab-btn").forEach((b) => b.classList.remove("tab-active"));
  btn.classList.add("tab-active");

  queueTab.hidden = tab !== "queue";
  historyTab.hidden = tab !== "history";

  if (tab === "history") {
    void loadAndRenderHistory();
  }
});

// ---- History integration ----
const historyList = document.getElementById("historyList");
const historyFilter = document.getElementById("historyFilter");
const historyEmpty = document.getElementById("historyEmpty");
const historyApi = createHistoryApi(getInvoke, {
  onReDownload(url) {
    // Switch to queue tab, fill URL input, enqueue
    document.querySelector('[data-tab="queue"]').click();
    urlInput.value = url;
    void enqueueUrlInput(url);
  },
});

async function loadAndRenderHistory() {
  try {
    const filter = historyFilter?.value || "all";
    const entries = await (filter === "all"
      ? historyApi.list()
      : historyApi.listByOutcome(filter));
    renderHistory(historyList, historyApi, entries, {
      filter,
      getInvoke,
      onEntriesChanged: () => void loadAndRenderHistory(),
    });
    if (historyEmpty) {
      historyEmpty.hidden = entries.length > 0;
    }
  } catch (err) {
    console.warn("Failed to load history:", err);
    historyList.innerHTML = `<div class="queue-empty"><span>Failed to load history: ${err}</span></div>`;
  }
}

historyFilter?.addEventListener("change", () => {
  void loadAndRenderHistory();
});

// Refresh history when events suggest a job completed or failed
// Hook into handleCoreEvent to trigger history refresh
const _origHandleCoreEvent = handleCoreEvent;
handleCoreEvent = function (evt) {
  _origHandleCoreEvent(evt);
  // Refresh history view if it's visible and a job completed or failed
  if (
    !historyTab.hidden &&
    (evt.type === "JobCompleted" || evt.type === "JobFailed")
  ) {
    void loadAndRenderHistory();
  }
};

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
  state.desktopNotificationsEnabled = loadDesktopNotificationsEnabled();
  if (desktopNotificationsInput) {
    desktopNotificationsInput.checked = state.desktopNotificationsEnabled;
  }
  applyClipboardWatcherPreference(
    localStorage.getItem("phlegethon-clipboard-watcher") === "1",
  );
  await initNotificationActivation({
    invoke: getInvoke(),
    onJobFocus: focusJobRow,
  });
  await refreshJobs();
  await initCompanionListener();
});

async function initCompanionListener() {
  const listen = window.__TAURI__?.event?.listen;
  if (typeof listen !== "function") return;

  await listen("companion-intent", (event) => {
    const { url, result } = event.payload ?? {};
    if (!result) return;
    const mapped = mapCompanionEnqueueResult(result);
    if (!mapped) return;
    handleEnqueueResult(url ?? mapped.url ?? "", mapped);
  });
}

function mapCompanionEnqueueResult(result) {
  if (result.type === "enqueue_direct") {
    return {
      type: "EnqueueDirect",
      job_id: result.job_id,
      destination: result.destination,
    };
  }
  if (result.type === "probe_then_confirm") {
    return {
      type: "ProbeThenConfirm",
      url: result.url,
    };
  }
  return null;
}

window.addEventListener("error", (e) => {
  showError(`JS error: ${e.message}`);
});
window.addEventListener("unhandledrejection", (e) => {
  showError(`Promise error: ${e.reason}`);
});
