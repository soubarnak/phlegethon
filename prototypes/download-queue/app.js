// PROTOTYPE — throwaway download queue variants for wayfinder issue #9.
// Question answered: queue look/feel (progress, Paused/Failed/Retry, batch grouping).

const VARIANTS = [
  { key: "A", name: "Dense rows" },
  { key: "B", name: "Batch tree" },
  { key: "C", name: "Split inspector" },
];

/** @typedef {"queued"|"downloading"|"paused"|"failed"|"done"} JobState */

/**
 * @typedef {{
 *   id: string,
 *   title: string,
 *   site: string,
 *   bytesDone: number,
 *   bytesTotal: number,
 *   speedBps: number,
 *   state: JobState,
 *   batchId?: string,
 * }} Job
 */

/**
 * @typedef {{
 *   id: string,
 *   title: string,
 *   collapsed?: boolean,
 * }} Batch
 */

/** @type {Batch[]} */
let batches = [
  {
    id: "batch-yt",
    title: "YouTube · Winter playlist",
    collapsed: false,
  },
];

/** @type {Job[]} */
let jobs = [
  {
    id: "j1",
    title: "Opening titles — 1080p",
    site: "YouTube",
    bytesDone: 412_000_000,
    bytesTotal: 780_000_000,
    speedBps: 4_200_000,
    state: "downloading",
    batchId: "batch-yt",
  },
  {
    id: "j2",
    title: "Interview cut — 720p",
    site: "YouTube",
    bytesDone: 0,
    bytesTotal: 310_000_000,
    speedBps: 0,
    state: "queued",
    batchId: "batch-yt",
  },
  {
    id: "j3",
    title: "Credits roll — audio-only",
    site: "YouTube",
    bytesDone: 18_000_000,
    bytesTotal: 42_000_000,
    speedBps: 0,
    state: "paused",
    batchId: "batch-yt",
  },
  {
    id: "j4",
    title: "luxuretv_clip_8841.mp4",
    site: "luxuretv",
    bytesDone: 96_000_000,
    bytesTotal: 510_000_000,
    speedBps: 0,
    state: "failed",
  },
  {
    id: "j5",
    title: "xhamster_sample_a1b2 — 1080p",
    site: "xHamster",
    bytesDone: 1_240_000_000,
    bytesTotal: 1_240_000_000,
    speedBps: 0,
    state: "done",
  },
  {
    id: "j6",
    title: "firmware-util-2.4.1.zip",
    site: "HTTP",
    bytesDone: 64_000_000,
    bytesTotal: 210_000_000,
    speedBps: 2_100_000,
    state: "downloading",
  },
];

let selectedId = "j1";
let tick = null;

function fmtBytes(n) {
  if (n < 1024) return `${n} B`;
  const u = ["KB", "MB", "GB", "TB"];
  let v = n;
  let i = -1;
  do {
    v /= 1024;
    i++;
  } while (v >= 1024 && i < u.length - 1);
  return `${v.toFixed(v >= 10 || i === 0 ? 0 : 1)} ${u[i]}`;
}

function fmtSpeed(bps) {
  if (!bps) return "—";
  return `${fmtBytes(bps)}/s`;
}

function pct(job) {
  if (!job.bytesTotal) return 0;
  return Math.min(100, Math.round((100 * job.bytesDone) / job.bytesTotal));
}

function eta(job) {
  if (job.state !== "downloading" || !job.speedBps) return "—";
  const left = Math.max(0, job.bytesTotal - job.bytesDone);
  const s = Math.round(left / job.speedBps);
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m ${s % 60}s`;
  return `${Math.floor(s / 3600)}h ${Math.floor((s % 3600) / 60)}m`;
}

function stateLabel(state) {
  return (
    {
      queued: "Queued",
      downloading: "Downloading",
      paused: "Paused",
      failed: "Failed",
      done: "Done",
    }[state] ?? state
  );
}

function stateClass(state) {
  if (state === "failed") return "state-failed";
  if (state === "paused") return "state-paused";
  if (state === "done") return "state-done";
  return "";
}

function fillClass(state) {
  if (state === "done") return "done";
  if (state === "paused") return "paused";
  if (state === "failed") return "failed";
  return "";
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
  const next = VARIANTS[(i + dir + VARIANTS.length) % VARIANTS.length];
  setVariant(next.key);
}

function mutateJob(id, fn) {
  jobs = jobs.map((j) => (j.id === id ? fn({ ...j }) : j));
  render();
}

function pause(id) {
  mutateJob(id, (j) => {
    if (j.state === "downloading") {
      j.state = "paused";
      j.speedBps = 0;
    }
    return j;
  });
}

function resume(id) {
  mutateJob(id, (j) => {
    if (j.state === "paused" || j.state === "queued") {
      j.state = "downloading";
      j.speedBps = j.speedBps || 2_500_000;
    }
    return j;
  });
}

function retry(id) {
  mutateJob(id, (j) => {
    if (j.state === "failed") {
      j.state = "downloading";
      j.speedBps = 1_800_000;
    }
    return j;
  });
}

function pauseBatch(batchId) {
  jobs = jobs.map((j) => {
    if (j.batchId !== batchId) return j;
    if (j.state === "downloading") return { ...j, state: "paused", speedBps: 0 };
    return j;
  });
  render();
}

function resumeBatch(batchId) {
  jobs = jobs.map((j) => {
    if (j.batchId !== batchId) return j;
    if (j.state === "paused" || j.state === "queued") {
      return { ...j, state: "downloading", speedBps: j.speedBps || 2_000_000 };
    }
    return j;
  });
  render();
}

function toggleCollapse(batchId) {
  batches = batches.map((b) =>
    b.id === batchId ? { ...b, collapsed: !b.collapsed } : b,
  );
  render();
}

function actionButtons(job, { compact = false } = {}) {
  const parts = [];
  if (job.state === "downloading") {
    parts.push(
      `<button type="button" data-act="pause" data-id="${job.id}">Pause</button>`,
    );
  }
  if (job.state === "paused" || job.state === "queued") {
    parts.push(
      `<button type="button" data-act="resume" data-id="${job.id}">${job.state === "queued" ? "Start" : "Resume"}</button>`,
    );
  }
  if (job.state === "failed") {
    parts.push(
      `<button type="button" class="danger" data-act="retry" data-id="${job.id}">Retry</button>`,
    );
  }
  if (!compact && job.state === "done") {
    parts.push(`<button type="button" disabled>Open</button>`);
  }
  return parts.join("");
}

function metaLine(job) {
  return `<span class="${stateClass(job.state)}">${stateLabel(job.state)}</span>
    <span class="mono">${fmtBytes(job.bytesDone)} / ${fmtBytes(job.bytesTotal)}</span>
    <span class="mono">${fmtSpeed(job.speedBps)}</span>
    <span class="mono">ETA ${eta(job)}</span>
    <span>${job.site}</span>`;
}

function track(job, kind = "row") {
  const wrap =
    kind === "big" ? "c-big-track" : kind === "cell" ? "b-child-track" : "a-track";
  const fill = kind === "big" ? "c-big-fill" : "a-fill";
  return `<div class="${wrap}" role="progressbar" aria-valuenow="${pct(job)}" aria-valuemin="0" aria-valuemax="100">
    <div class="${fill} ${fillClass(job.state)}" style="width:${pct(job)}%"></div>
  </div>`;
}

function renderTopbar(subtitle) {
  const active = jobs.filter((j) => j.state === "downloading").length;
  const failed = jobs.filter((j) => j.state === "failed").length;
  return `<header class="topbar">
    <div>
      <h1>Phlegethon</h1>
      <div class="meta">${subtitle} · ${active} active · ${failed} failed</div>
    </div>
    <div class="toolbar">
      <button type="button" class="primary">Add URL</button>
      <button type="button">Pause all</button>
    </div>
  </header>`;
}

function renderA() {
  const batch = batches[0];
  const inBatch = jobs.filter((j) => j.batchId === batch.id);
  const lone = jobs.filter((j) => !j.batchId);

  const row = (job, batchChild) => `
    <li class="a-row ${batchChild ? "is-batch-child" : ""}" data-id="${job.id}">
      <div class="a-title">${escapeHtml(job.title)}</div>
      <div class="a-actions">${actionButtons(job)}</div>
      <div class="a-sub">${metaLine(job)}</div>
      ${job.state === "done" ? "" : track(job, "row")}
    </li>`;

  return `<div class="shell">
    ${renderTopbar("A — Dense rows")}
    <ul class="a-list">
      <li class="a-batch-head">
        <span>${escapeHtml(batch.title)} · ${inBatch.length} items</span>
        <span class="mono">${inBatch.filter((j) => j.state === "done").length}/${inBatch.length} done</span>
      </li>
      ${inBatch.map((j) => row(j, true)).join("")}
      ${lone.map((j) => row(j, false)).join("")}
    </ul>
  </div>`;
}

function renderB() {
  const batch = batches[0];
  const inBatch = jobs.filter((j) => j.batchId === batch.id);
  const lone = jobs.filter((j) => !j.batchId);
  const done = inBatch.filter((j) => j.state === "done").length;
  const groupPct = Math.round(
    inBatch.reduce((s, j) => s + pct(j), 0) / Math.max(1, inBatch.length),
  );

  return `<div class="shell">
    ${renderTopbar("B — Batch tree")}
    <div class="b-layout">
      <div class="b-group">
        <div class="b-group-head" data-act="toggle-batch" data-id="${batch.id}">
          <span class="b-chevron">${batch.collapsed ? "▸" : "▾"}</span>
          <div>
            <div class="b-group-title">${escapeHtml(batch.title)}</div>
            <div class="b-group-meta mono">${done}/${inBatch.length} done · ~${groupPct}%</div>
          </div>
          <div class="b-group-actions" onclick="event.stopPropagation()">
            <button type="button" data-act="pause-batch" data-id="${batch.id}">Pause group</button>
            <button type="button" data-act="resume-batch" data-id="${batch.id}">Resume group</button>
          </div>
        </div>
        ${
          batch.collapsed
            ? ""
            : `<ul class="b-children">
          ${inBatch
            .map(
              (job) => `<li class="b-child">
              <div>
                <div>${escapeHtml(job.title)}</div>
                <div class="a-sub" style="margin-top:4px">${metaLine(job)}</div>
              </div>
              ${job.state === "done" ? `<span class="state-done mono">${pct(job)}%</span>` : track(job, "cell")}
              <div>${actionButtons(job, { compact: true })}</div>
            </li>`,
            )
            .join("")}
        </ul>`
        }
      </div>
      <ul class="b-ungrouped">
        ${lone
          .map(
            (job) => `<li>
            <div>
              <div>${escapeHtml(job.title)}</div>
              <div class="a-sub" style="margin-top:4px">${metaLine(job)}</div>
            </div>
            ${job.state === "done" ? `<span class="state-done mono">${pct(job)}%</span>` : track(job, "cell")}
            <div>${actionButtons(job, { compact: true })}</div>
          </li>`,
          )
          .join("")}
      </ul>
    </div>
  </div>`;
}

function renderC() {
  const job = jobs.find((j) => j.id === selectedId) ?? jobs[0];
  selectedId = job.id;
  const batch = job.batchId
    ? batches.find((b) => b.id === job.batchId)
    : null;

  return `<div class="shell">
    ${renderTopbar("C — Split inspector")}
    <div class="c-layout">
      <ul class="c-strip">
        ${jobs
          .map((j) => {
            const sel = j.id === selectedId ? "is-selected" : "";
            return `<li>
              <button type="button" class="c-item ${sel}" data-act="select" data-id="${j.id}">
                <span class="c-dot ${j.state}"></span>
                <span>
                  <div class="c-item-title">${escapeHtml(j.title)}</div>
                  <div class="c-item-meta mono">${stateLabel(j.state)} · ${pct(j)}%</div>
                </span>
              </button>
            </li>`;
          })
          .join("")}
      </ul>
      <section class="c-detail">
        <h2>${escapeHtml(job.title)}</h2>
        ${
          batch
            ? `<div class="c-batch-note">Part of batch <strong>${escapeHtml(batch.title)}</strong> — group actions live on Variant B; here the selected job is the focus.</div>`
            : ""
        }
        <div class="c-detail-meta">
          <span class="${stateClass(job.state)}">${stateLabel(job.state)}</span>
          <span class="mono">${fmtBytes(job.bytesDone)} / ${fmtBytes(job.bytesTotal)} (${pct(job)}%)</span>
          <span class="mono">${fmtSpeed(job.speedBps)}</span>
          <span class="mono">ETA ${eta(job)}</span>
          <span>${escapeHtml(job.site)}</span>
        </div>
        ${track(job, "big")}
        <div class="c-detail-actions">${actionButtons(job)}</div>
      </section>
    </div>
  </div>`;
}

function escapeHtml(s) {
  return String(s)
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;");
}

function bindActions(root) {
  root.querySelectorAll("[data-act]").forEach((el) => {
    el.addEventListener("click", (e) => {
      e.stopPropagation();
      const act = el.getAttribute("data-act");
      const id = el.getAttribute("data-id");
      if (act === "pause") pause(id);
      if (act === "resume") resume(id);
      if (act === "retry") retry(id);
      if (act === "select") {
        selectedId = id;
        render();
      }
      if (act === "toggle-batch") toggleCollapse(id);
      if (act === "pause-batch") pauseBatch(id);
      if (act === "resume-batch") resumeBatch(id);
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
    const next = theme === "dark" ? "light" : "dark";
    document.documentElement.setAttribute("data-theme", next);
    renderSwitcher();
  });
}

function render() {
  const app = document.getElementById("app");
  const v = getVariant();
  app.innerHTML = v === "B" ? renderB() : v === "C" ? renderC() : renderA();
  bindActions(app);
  renderSwitcher();
  // Surface full relevant state after every action / variant switch.
  console.log("[prototype state]", {
    variant: v,
    selectedId,
    batches: structuredClone(batches),
    jobs: structuredClone(jobs),
  });
}

function simulate() {
  jobs = jobs.map((j) => {
    if (j.state !== "downloading") return j;
    const next = Math.min(j.bytesTotal, j.bytesDone + j.speedBps * 0.4);
    if (next >= j.bytesTotal) {
      return { ...j, bytesDone: j.bytesTotal, speedBps: 0, state: "done" };
    }
    return { ...j, bytesDone: next };
  });
  render();
}

document.addEventListener("keydown", (e) => {
  const t = e.target;
  if (
    t &&
    (t.tagName === "INPUT" ||
      t.tagName === "TEXTAREA" ||
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
tick = setInterval(simulate, 400);
