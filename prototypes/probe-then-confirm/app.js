// PROTOTYPE — Probe-then-Confirm variants for wayfinder issue #10.
// Question: channel/playlist selection surface (list, quality, select-all default, confirm).

const VARIANTS = [
  { key: "A", name: "Dense sheet" },
  { key: "B", name: "Bulk quality bar" },
  { key: "C", name: "Two-pane" },
];

const QUALITIES = [
  { id: "best", label: "Best available" },
  { id: "1080", label: "1080p" },
  { id: "720", label: "720p" },
  { id: "480", label: "480p" },
  { id: "audio", label: "Audio only" },
];

/** @typedef {{ id: string, title: string, duration: string, sizeHint: string }} Item */

/** @type {Item[]} */
const SOURCE = [
  { id: "i1", title: "01 · Opening titles", duration: "4:12", sizeHint: "~180 MB" },
  { id: "i2", title: "02 · Cold open", duration: "8:04", sizeHint: "~310 MB" },
  { id: "i3", title: "03 · Interview A", duration: "14:22", sizeHint: "~520 MB" },
  { id: "i4", title: "04 · B-roll reel", duration: "6:40", sizeHint: "~240 MB" },
  { id: "i5", title: "05 · Panel discussion", duration: "22:11", sizeHint: "~780 MB" },
  { id: "i6", title: "06 · Q&A", duration: "11:03", sizeHint: "~400 MB" },
  { id: "i7", title: "07 · Credits + outtakes", duration: "3:28", sizeHint: "~95 MB" },
  { id: "i8", title: "08 · Bonus short", duration: "1:50", sizeHint: "~60 MB" },
];

/**
 * @typedef {{
 *   selected: Record<string, boolean>,
 *   qualityById: Record<string, string>,
 *   bulkQuality: string,
 *   overrides: Record<string, boolean>,
 *   focusId: string,
 *   paneQuality: string,
 * }} State
 */

/** @type {State} */
let state = {
  selected: Object.fromEntries(SOURCE.map((i) => [i.id, true])),
  qualityById: Object.fromEntries(SOURCE.map((i) => [i.id, "1080"])),
  bulkQuality: "1080",
  overrides: {},
  focusId: SOURCE[0].id,
  paneQuality: "1080",
};

let toastTimer = null;

function escapeHtml(s) {
  return String(s)
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;");
}

function getVariant() {
  const v = new URLSearchParams(location.search).get("variant") ?? "A";
  return VARIANTS.some((x) => x.key === v) ? v : "A";
}

function setVariant(key) {
  const url = new URL(location.href);
  url.searchParams.set("variant", key);
  history.replaceState(null, "", url);
  render();
}

function cycle(dir) {
  const i = VARIANTS.findIndex((v) => v.key === getVariant());
  setVariant(VARIANTS[(i + dir + VARIANTS.length) % VARIANTS.length].key);
}

function selectedCount() {
  return SOURCE.filter((i) => state.selected[i.id]).length;
}

function selectAll(on) {
  state.selected = Object.fromEntries(SOURCE.map((i) => [i.id, on]));
  render();
}

function toggle(id) {
  state.selected[id] = !state.selected[id];
  render();
}

function setRowQuality(id, q) {
  state.qualityById[id] = q;
  state.overrides[id] = true;
  render();
}

function applyBulk() {
  for (const item of SOURCE) {
    if (state.selected[item.id] && !state.overrides[item.id]) {
      state.qualityById[item.id] = state.bulkQuality;
    }
  }
  render();
}

function clearOverride(id) {
  delete state.overrides[id];
  state.qualityById[id] = state.bulkQuality;
  render();
}

function qualityOptions(selectedId) {
  return QUALITIES.map(
    (q) =>
      `<option value="${q.id}" ${q.id === selectedId ? "selected" : ""}>${q.label}</option>`,
  ).join("");
}

function qualityLabel(id) {
  return QUALITIES.find((q) => q.id === id)?.label ?? id;
}

function showToast(msg) {
  const el = document.getElementById("toast");
  el.textContent = msg;
  el.classList.add("show");
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => el.classList.remove("show"), 2200);
}

function confirmEnqueue() {
  const picks = SOURCE.filter((i) => state.selected[i.id]).map((i) => ({
    id: i.id,
    title: i.title,
    quality:
      getVariant() === "C"
        ? state.paneQuality
        : getVariant() === "B"
          ? state.overrides[i.id]
            ? state.qualityById[i.id]
            : state.bulkQuality
          : state.qualityById[i.id],
  }));
  console.log("[prototype confirm]", { variant: getVariant(), picks });
  showToast(
    picks.length
      ? `Would enqueue ${picks.length} job${picks.length === 1 ? "" : "s"}`
      : "Nothing selected",
  );
}

function topbar(variantLabel) {
  return `<header class="topbar">
    <div>
      <h1>Winter playlist</h1>
      <div class="sub">YouTube · Probe-then-Confirm · ${escapeHtml(variantLabel)} · ${SOURCE.length} items</div>
    </div>
    <div class="sub mono">${selectedCount()} / ${SOURCE.length} selected</div>
  </header>
  <div id="toast" class="toast" role="status"></div>`;
}

function footer(extra = "") {
  const n = selectedCount();
  return `<footer class="footer">
    <div class="count mono">${n} selected · all on by default</div>
    <div class="footer-actions">
      ${extra}
      <button type="button" class="ghost" data-act="select-all">Select all</button>
      <button type="button" class="ghost" data-act="select-none">Select none</button>
      <button type="button" class="primary" data-act="confirm" ${n ? "" : "disabled"}>Confirm download</button>
    </div>
  </footer>`;
}

function renderA() {
  const rows = SOURCE.map((item) => {
    const on = state.selected[item.id];
    return `<li class="row a-row ${on ? "" : "unchecked"}">
      <label class="row-check">
        <input type="checkbox" data-act="toggle" data-id="${item.id}" ${on ? "checked" : ""} />
        <span>
          <div class="row-title">${escapeHtml(item.title)}</div>
          <div class="row-meta mono">${item.duration} · ${item.sizeHint}</div>
        </span>
      </label>
      <select data-act="row-quality" data-id="${item.id}" ${on ? "" : "disabled"} aria-label="Quality for ${escapeHtml(item.title)}">
        ${qualityOptions(state.qualityById[item.id])}
      </select>
    </li>`;
  }).join("");

  return `<div class="shell has-footer">
    ${topbar("A — Dense sheet")}
    <ul class="list">${rows}</ul>
    ${footer()}
  </div>`;
}

function renderB() {
  const rows = SOURCE.map((item) => {
    const on = state.selected[item.id];
    const overridden = !!state.overrides[item.id];
    const q = overridden ? state.qualityById[item.id] : state.bulkQuality;
    return `<li class="row b-row ${on ? "" : "unchecked"}">
      <label class="row-check">
        <input type="checkbox" data-act="toggle" data-id="${item.id}" ${on ? "checked" : ""} />
        <span>
          <div class="row-title">${escapeHtml(item.title)}</div>
          <div class="row-meta mono">${item.duration} · ${item.sizeHint} · ${qualityLabel(q)}${overridden ? " (override)" : ""}</div>
        </span>
      </label>
      <div class="b-override">
        ${
          overridden
            ? `<select data-act="row-quality" data-id="${item.id}">${qualityOptions(state.qualityById[item.id])}</select>
               <button type="button" class="ghost" data-act="clear-override" data-id="${item.id}">Use bulk</button>`
            : `<button type="button" class="ghost" data-act="start-override" data-id="${item.id}" ${on ? "" : "disabled"}>Override</button>`
        }
      </div>
    </li>`;
  }).join("");

  return `<div class="shell has-footer">
    ${topbar("B — Bulk quality bar")}
    <div class="b-bar">
      <label>
        Quality for selected
        <select data-act="bulk-quality">${qualityOptions(state.bulkQuality)}</select>
      </label>
      <button type="button" data-act="apply-bulk">Apply to selected</button>
      <span class="row-meta">Overrides stay put until cleared</span>
    </div>
    <ul class="list">${rows}</ul>
    ${footer()}
  </div>`;
}

function renderC() {
  const n = selectedCount();
  const rows = SOURCE.map((item) => {
    const on = state.selected[item.id];
    const focus = state.focusId === item.id ? "is-focus" : "";
    return `<li class="row c-row ${on ? "" : "unchecked"} ${focus}" data-act="focus" data-id="${item.id}">
      <label class="row-check" onclick="event.stopPropagation()">
        <input type="checkbox" data-act="toggle" data-id="${item.id}" ${on ? "checked" : ""} />
        <span>
          <div class="row-title">${escapeHtml(item.title)}</div>
          <div class="row-meta mono">${item.duration} · ${item.sizeHint}</div>
        </span>
      </label>
    </li>`;
  }).join("");

  const qualityBtns = QUALITIES.map(
    (q) =>
      `<button type="button" class="${state.paneQuality === q.id ? "is-on" : ""}" data-act="pane-quality" data-id="${q.id}">${q.label}</button>`,
  ).join("");

  return `<div class="shell">
    ${topbar("C — Two-pane")}
    <div class="c-layout">
      <div class="c-left">
        <div class="b-bar" style="border-bottom:1px solid var(--gray-400)">
          <button type="button" class="ghost" data-act="select-all">Select all</button>
          <button type="button" class="ghost" data-act="select-none">Select none</button>
        </div>
        <ul class="list">${rows}</ul>
      </div>
      <aside class="c-right">
        <h2>Enqueue</h2>
        <dl class="c-summary">
          <dt>Items</dt>
          <dd>${n} of ${SOURCE.length}</dd>
          <dt>Quality for all selected</dt>
          <dd>${escapeHtml(qualityLabel(state.paneQuality))}</dd>
          <dt>Destination</dt>
          <dd>Library Root / Winter playlist/</dd>
        </dl>
        <div class="c-quality-grid">${qualityBtns}</div>
        <button type="button" class="primary" data-act="confirm" ${n ? "" : "disabled"}>Confirm download</button>
        <p class="row-meta">One quality applies to the whole selection. Per-item quality lives in A/B.</p>
      </aside>
    </div>
  </div>`;
}

function bind(root) {
  root.querySelectorAll("[data-act]").forEach((el) => {
    const act = el.getAttribute("data-act");
    const id = el.getAttribute("data-id");

    if (el.tagName === "SELECT") {
      el.addEventListener("change", () => {
        if (act === "row-quality") setRowQuality(id, el.value);
        if (act === "bulk-quality") {
          state.bulkQuality = el.value;
          applyBulk();
        }
      });
      return;
    }

    if (el.tagName === "INPUT" && el.type === "checkbox") {
      el.addEventListener("change", () => toggle(id));
      return;
    }

    el.addEventListener("click", (e) => {
      e.stopPropagation();
      if (act === "toggle") toggle(id);
      if (act === "select-all") selectAll(true);
      if (act === "select-none") selectAll(false);
      if (act === "confirm") confirmEnqueue();
      if (act === "apply-bulk") {
        for (const item of SOURCE) {
          if (state.selected[item.id] && !state.overrides[item.id]) {
            state.qualityById[item.id] = state.bulkQuality;
          }
        }
        render();
      }
      if (act === "start-override") {
        state.overrides[id] = true;
        state.qualityById[id] = state.bulkQuality;
        render();
      }
      if (act === "clear-override") clearOverride(id);
      if (act === "focus") {
        state.focusId = id;
        render();
      }
      if (act === "pane-quality") {
        state.paneQuality = id;
        render();
      }
    });
  });
}

function renderSwitcher() {
  const current = VARIANTS.find((v) => v.key === getVariant());
  const theme = document.documentElement.getAttribute("data-theme") || "light";
  const bar = document.getElementById("prototype-switcher");
  bar.innerHTML = `
    <button type="button" data-nav="-1" aria-label="Previous variant">←</button>
    <div class="label">${current.key} — ${current.name}</div>
    <button type="button" data-nav="1" aria-label="Next variant">→</button>
    <div class="theme-toggle">
      <button type="button" data-theme-toggle>${theme === "dark" ? "Light" : "Dark"}</button>
    </div>
  `;
  bar.querySelectorAll("[data-nav]").forEach((btn) => {
    btn.addEventListener("click", () => cycle(Number(btn.getAttribute("data-nav"))));
  });
  bar.querySelector("[data-theme-toggle]").addEventListener("click", () => {
    document.documentElement.setAttribute(
      "data-theme",
      theme === "dark" ? "light" : "dark",
    );
    renderSwitcher();
  });
}

function render() {
  const app = document.getElementById("app");
  const v = getVariant();
  app.innerHTML = v === "B" ? renderB() : v === "C" ? renderC() : renderA();
  bind(app);
  renderSwitcher();
  console.log("[prototype state]", {
    variant: v,
    selectedCount: selectedCount(),
    state: structuredClone(state),
  });
}

document.addEventListener("keydown", (e) => {
  const t = e.target;
  if (
    t &&
    (t.tagName === "INPUT" ||
      t.tagName === "TEXTAREA" ||
      t.tagName === "SELECT" ||
      t.isContentEditable)
  ) {
    return;
  }
  if (e.key === "ArrowLeft") {
    e.preventDefault();
    cycle(-1);
  }
  if (e.key === "ArrowRight") {
    e.preventDefault();
    cycle(1);
  }
});

render();
