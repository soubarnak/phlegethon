import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  Content,
  Header,
  HeaderGlobalAction,
  HeaderGlobalBar,
  HeaderName,
  InlineLoading,
  InlineNotification,
  Tab,
  TabList,
  TabPanel,
  TabPanels,
  Tabs,
  Tag,
} from "@carbon/react";
import { Asleep, Light, Tools } from "@carbon/icons-react";
import { api } from "@/tauri/api";
import { onCompanionIntent } from "@/tauri/events";
import { notificationPort } from "@/tauri/platform";
import type { CompanionIntent, JobSnapshot, ToolchainStatus } from "@/tauri/types";
import { buildBatchDiscardCopy, buildDiscardConfirmCopy, type DiscardConfirmCopy } from "@/lib/discardConfirm";
import { maybeNotify, loadDesktopNotificationsEnabled, saveDesktopNotificationsEnabled } from "@/lib/notifications";
import { resolveDuplicateUrl, processUrlInput, type DuplicateResolution } from "@/lib/urlInput";
import { errorToString } from "@/lib/errors";
import { t } from "@/lib/i18n";
import { writeClipboardText } from "@/tauri/platform";
import { useJobs, type JobRow } from "@/state/useJobs";
import { useBooleanPreference } from "@/state/usePreference";
import { useTheme } from "@/state/useTheme";
import { ToastProvider, useToasts } from "@/state/useToasts";
import { UrlBar } from "@/components/UrlBar";
import { QueueTable, type QueueActions } from "@/components/QueueTable";
import { ProbeModal } from "@/components/ProbeModal";
import { DiscardModal } from "@/components/DiscardModal";
import { DuplicateUrlModal } from "@/components/DuplicateUrlModal";
import { HistoryTable } from "@/components/HistoryTable";
import { SettingsPage } from "@/components/SettingsPage";
import { PromptModal } from "@/components/PromptModal";
import { ExpectedHashModal, type ExpectedHashRequest } from "@/components/ExpectedHashModal";
import { MirrorsModal, type MirrorsRequest } from "@/components/MirrorsModal";
import type { ActiveJobMatch } from "@/tauri/types";

const TAB_QUEUE = 0;
const TAB_SETTINGS = 2;

export default function App() {
  return (
    <ToastProvider>
      <Shell />
    </ToastProvider>
  );
}

function Shell() {
  const toasts = useToasts();
  const [theme, toggleTheme] = useTheme();
  const { jobs, loading, error: jobsError, refresh, kick, events, dismissEvents } = useJobs();
  const [tab, setTab] = useState(TAB_QUEUE);
  const [destination, setDestination] = useState<string | null>(null);
  const [clipboardWatch, setClipboardWatch] = useBooleanPreference("phlegethon-clipboard-watcher", false);
  const [notificationsEnabled, setNotificationsEnabled] = useState(loadDesktopNotificationsEnabled);
  // URLs awaiting Probe-then-Confirm; the head is shown, the rest wait their turn.
  const [probeQueue, setProbeQueue] = useState<string[]>([]);
  const probeUrl = probeQueue[0] ?? null;
  // Count of addUrls runs queued or in progress; they run one at a time (M-64).
  const [pendingAdds, setPendingAdds] = useState(0);
  const busy = pendingAdds > 0;
  const addChain = useRef<Promise<unknown>>(Promise.resolve());
  const [historyKey, setHistoryKey] = useState(0);
  const [toolchain, setToolchain] = useState<ToolchainStatus | null>(null);
  const [duplicate, setDuplicate] = useState<{ match: ActiveJobMatch; resolve: (r: DuplicateResolution) => void } | null>(null);
  const [discardIds, setDiscardIds] = useState<string[] | null>(null);
  const [prompt, setPrompt] = useState<PromptRequest | null>(null);
  const [hashRequest, setHashRequest] = useState<ExpectedHashRequest | null>(null);
  const [mirrorsRequest, setMirrorsRequest] = useState<MirrorsRequest | null>(null);
  const jobsRef = useRef<JobRow[]>([]);
  jobsRef.current = jobs;

  // ---- toolchain health tag in the header
  useEffect(() => {
    api.getToolchainStatus(false).then(setToolchain).catch(() => setToolchain(null));
  }, []);

  // ---- history reloads and notifications on terminal events
  // Every queued event is handled exactly once; toggling notifications re-runs the
  // effect but finds the queue already dismissed (M-65).
  useEffect(() => {
    if (events.length === 0) return;
    setHistoryKey((k) => k + 1);
    for (const { event } of events) {
      const job = jobsRef.current.find((j) => j.job_id === event.job_id);
      const title = job?.title ?? event.job_id;
      void (async () => {
        const port = await notificationPort();
        await maybeNotify({
          port,
          isMainWindowFocused: () => api.isMainWindowFocused(),
          settingsEnabled: notificationsEnabled,
          headline: event.type === "JobCompleted" ? t("notify.completed") : t("notify.failed"),
          body: event.type === "JobCompleted" ? title : `${title}: ${event.error}`,
        });
      })();
    }
    dismissEvents(events[events.length - 1].id);
  }, [events, dismissEvents, notificationsEnabled]);

  // ---- enqueue flow
  const queueProbe = useCallback((url: string) => {
    setProbeQueue((q) => (q.includes(url) ? q : [...q, url]));
  }, []);

  const enqueueOne = useCallback(
    async (url: string, forceDirect = false) => {
      const result = await api.enqueueUrl(url, destination, forceDirect);
      if (result.type === "probe_then_confirm") {
        queueProbe(result.url);
      } else {
        await refresh();
        await kick();
      }
    },
    [destination, refresh, kick, queueProbe],
  );
  const confirmDuplicate = useCallback(
    (match: ActiveJobMatch) =>
      new Promise<DuplicateResolution>((resolve) => {
        setDuplicate({ match, resolve });
      }),
    [],
  );

  const runAddUrls = useCallback(
    async (text: string) => {
      try {
        const outcome = await processUrlInput(
          {
            splitUrls: (value) => api.splitUrls(value),
            resolveDuplicate: (url) =>
              resolveDuplicateUrl(
                {
                  findActiveJobForUrl: (u) => api.findActiveJobForUrl(u),
                  confirmDuplicate,
                  focusExisting: () => setTab(TAB_QUEUE),
                },
                url,
              ),
            enqueueOne: (url) => enqueueOne(url),
          },
          text,
        );
        if (outcome.total === 0) {
          toasts.warning(t("url.noneFound"));
        } else if (outcome.failed.length > 0) {
          const detail = outcome.failed
            .slice(0, 3)
            .map(({ url, error }) => `${url}: ${errorToString(error)}`)
            .join("\n");
          toasts.error(t("url.addFailedSome", { failed: outcome.failed.length, total: outcome.total }), detail);
        } else if (outcome.enqueued > 1) {
          toasts.info(t("url.addedMany", { count: outcome.enqueued }));
        }
      } catch (err) {
        toasts.error(t("url.addFailed"), err);
      }
    },
    [confirmDuplicate, enqueueOne, toasts],
  );

  // One duplicate prompt can be open at a time, so submissions from paste, drop, the
  // clipboard watcher and History wait for the previous one to finish.
  const runAddUrlsRef = useRef(runAddUrls);
  runAddUrlsRef.current = runAddUrls;
  const addUrls = useCallback((text: string) => {
    setPendingAdds((n) => n + 1);
    const run = addChain.current.then(() => runAddUrlsRef.current(text));
    addChain.current = run.catch(() => undefined).finally(() => setPendingAdds((n) => n - 1));
    return run;
  }, []);

  // Companion (browser extension) hand-off. The shell has already acted on the URL:
  // a direct file is enqueued but emits no core-event, and a page waits for a probe.
  // Handle the outcome here; never run the URL through addUrls again.
  const handleCompanionIntent = useCallback(
    (intent: CompanionIntent) => {
      switch (intent.result.type) {
        case "enqueue_direct":
          setTab(TAB_QUEUE);
          void refresh().then(kick);
          break;
        case "probe_then_confirm":
          queueProbe(intent.result.url);
          break;
      }
    },
    [kick, queueProbe, refresh],
  );
  // Subscribe once; the handler is read through a ref so it never re-subscribes (L-55).
  const companionHandler = useRef(handleCompanionIntent);
  companionHandler.current = handleCompanionIntent;

  useEffect(() => {
    let off: (() => void) | null = null;
    let cancelled = false;
    onCompanionIntent((intent) => companionHandler.current(intent))
      .then(async (fn) => {
        if (cancelled) {
          fn();
          return;
        }
        off = fn;
        // The shell holds intents that arrived while the webview was loading and emits
        // live ones only after this call. Subscribe first, so nothing falls in between (M-69).
        const pending = await api.takePendingIntents();
        if (!cancelled) pending.forEach((intent) => companionHandler.current(intent));
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
      off?.();
    };
  }, []);

  // The shell notes problems it found loading the Library Root (a corrupt queue file
  // moved aside, rows skipped) before the webview existed; tell the user once (H-8).
  const warnRef = useRef(toasts.warning);
  warnRef.current = toasts.warning;
  useEffect(() => {
    api
      .takeLibraryRootWarnings()
      .then((warnings) => warnings.forEach((w) => warnRef.current(t("libraryRoot.warning"), w)))
      .catch(() => undefined);
  }, []);

  // ---- probe lifecycle
  // Remove by URL, not by position: a second close or a late confirm must not skip the next URL (M-63).
  const dropProbe = useCallback((url: string | null) => {
    if (url) setProbeQueue((q) => q.filter((u) => u !== url));
  }, []);

  const onProbeConfirmed = useCallback(
    async (url: string, jobIds: string[]) => {
      toasts.success(t("probe.queued", { count: jobIds.length }));
      dropProbe(url);
      setTab(TAB_QUEUE);
      await refresh();
      await kick();
    },
    [dropProbe, refresh, kick, toasts],
  );

  const tryPlain = useCallback(
    async (url: string) => {
      dropProbe(url);
      try {
        await enqueueOne(url, true);
        toasts.info(t("probe.plainQueued"));
      } catch (err) {
        toasts.error(t("probe.plainFailed"), err);
      }
    },
    [dropProbe, enqueueOne, toasts],
  );

  const openCookieSettings = useCallback(
    (url: string) => {
      dropProbe(url);
      setTab(TAB_SETTINGS);
      window.setTimeout(() => document.getElementById("settings-cookies")?.scrollIntoView({ behavior: "smooth" }), 50);
    },
    [dropProbe],
  );

  // ---- queue actions
  const guard = useCallback(
    async (label: string, action: () => Promise<unknown>) => {
      try {
        await action();
        await refresh();
      } catch (err) {
        toasts.error(label, err);
      }
    },
    [refresh, toasts],
  );

  const actions = useMemo<QueueActions>(
    () => ({
      pause: (id) => void guard(t("queue.pauseFailed"), () => api.pauseJob(id)),
      resume: (id) =>
        void guard(t("queue.resumeFailed"), async () => {
          await api.resumeJob(id);
          await kick();
        }),
      retry: (id) =>
        void guard(t("queue.retryFailed"), async () => {
          await api.retryJob(id);
          await kick();
        }),
      pauseMany: (ids) => void guard(t("queue.pauseFailed"), () => Promise.all(ids.map((id) => api.pauseJob(id)))),
      resumeMany: (ids) =>
        void guard(t("queue.resumeFailed"), async () => {
          await Promise.all(ids.map((id) => api.resumeJob(id)));
          await kick();
        }),
      retryMany: (ids) =>
        void guard(t("queue.retryFailed"), async () => {
          await Promise.all(ids.map((id) => api.retryJob(id)));
          await kick();
        }),
      discard: (ids) => {
        if (ids.some((id) => jobsRef.current.some((j) => j.job_id === id))) setDiscardIds(ids);
      },
      moveUp: (job) =>
        void guard(t("queue.reorderFailed"), () => api.reorderJob(job.queue_index, Math.max(0, job.queue_index - 1))),
      moveDown: (job) =>
        void guard(t("queue.reorderFailed"), () =>
          api.reorderJob(job.queue_index, Math.min(jobsRef.current.length - 1, job.queue_index + 1)),
        ),
      reveal: (job: JobSnapshot) => void guard(t("history.revealFailed"), () => api.revealOutput(job.path ?? job.destination)),
      open: (job: JobSnapshot) => void guard(t("history.openFailed"), () => api.openOutput(job.path ?? "")),
      copyUrl: (job: JobSnapshot) =>
        void writeClipboardText(job.url).then(
          () => toasts.info(t("queue.urlCopied")),
          (err) => toasts.error(t("queue.copyFailed"), err),
        ),
      addMirror: (job: JobSnapshot) =>
        setPrompt({
          title: t("queue.mirror.title"),
          label: t("queue.mirror.label"),
          placeholder: "https://mirror.example.com/file.iso",
          confirmLabel: t("queue.mirror.add"),
          validate: (v) => (/^https?:\/\//i.test(v.trim()) ? null : t("queue.mirror.invalid")),
          onSubmit: (v) =>
            guard(t("queue.mirror.failed"), async () => {
              await api.addJobMirror(job.job_id, v.trim());
              toasts.success(t("queue.mirror.added"));
            }),
        }),
      setExpectedHash: (job: JobSnapshot) => setHashRequest({ jobId: job.job_id, title: job.title }),
      viewMirrors: (job: JobSnapshot) => setMirrorsRequest({ jobId: job.job_id, title: job.title }),
      setJobCap: (job: JobSnapshot) =>
        setPrompt({
          title: t("queue.cap.title"),
          label: t("queue.cap.label"),
          placeholder: "0",
          confirmLabel: t("settings.apply"),
          validate: (v) => (/^\d+$/.test(v.trim()) ? null : t("queue.cap.invalid")),
          onSubmit: (v) =>
            guard(t("queue.cap.failed"), async () => {
              const kb = Number(v.trim());
              await api.setJobBandwidthCap(job.job_id, kb > 0 ? kb * 1024 : null);
              toasts.success(kb > 0 ? t("queue.cap.set", { kb }) : t("queue.cap.cleared"));
            }),
        }),
    }),
    [guard, kick, toasts],
  );

  // Built from the live jobs on every render, so a job that completes while the dialog is
  // open no longer shows the warning about deleting files (L-60).
  const discardCopy = useMemo<DiscardConfirmCopy | null>(() => {
    if (!discardIds) return null;
    const selected = discardIds
      .map((id) => jobs.find((j) => j.job_id === id))
      .filter((j): j is JobRow => !!j);
    if (selected.length === 0) return null;
    return selected.length === 1
      ? buildDiscardConfirmCopy(selected[0].status, selected[0].destination)
      : buildBatchDiscardCopy(selected.map((j) => j.status));
  }, [discardIds, jobs]);

  const confirmDiscard = useCallback(async () => {
    if (!discardIds) return;
    const ids = discardIds;
    setDiscardIds(null);
    await guard(t("queue.discardFailed"), () => Promise.all(ids.map((id) => api.discardJob(id))));
  }, [discardIds, guard]);

  const header = (
    <Header aria-label="Phlegethon">
      <HeaderName href="#" prefix="">
        Phlegethon
      </HeaderName>
      <HeaderGlobalBar>
        {toolchain && (
          <Tag
            size="sm"
            type={toolchain.issues.length === 0 ? "green" : toolchain.ytdlp ? "warm-gray" : "red"}
            title={toolchain.issues.join("\n") || t("toolchain.healthy")}
            style={{ alignSelf: "center", marginRight: "0.5rem" }}
          >
            {toolchain.ytdlp ? `yt-dlp ${toolchain.ytdlp.version ?? ""}`.trim() : t("toolchain.noYtdlp")}
          </Tag>
        )}
        <HeaderGlobalAction
          aria-label={t("toolchain.title")}
          tooltipAlignment="end"
          onClick={() => setTab(TAB_SETTINGS)}
        >
          <Tools size={20} />
        </HeaderGlobalAction>
        <HeaderGlobalAction
          aria-label={theme === "g10" ? t("theme.dark") : t("theme.light")}
          tooltipAlignment="end"
          onClick={toggleTheme}
        >
          {theme === "g10" ? <Asleep size={20} /> : <Light size={20} />}
        </HeaderGlobalAction>
      </HeaderGlobalBar>
    </Header>
  );

  return (
    <div className="ph-app">
      {header}
      <Content id="main-content" className="ph-main" style={{ padding: 0 }}>
        <div className="ph-content">
          <UrlBar
            onSubmit={addUrls}
            destination={destination}
            onDestinationChange={setDestination}
            clipboardWatch={clipboardWatch}
            onClipboardWatchChange={setClipboardWatch}
            busy={busy}
          />
          <Tabs selectedIndex={tab} onChange={({ selectedIndex }) => setTab(selectedIndex)}>
            <TabList aria-label={t("app.sections")} contained>
              <Tab>{t("queue.title")}</Tab>
              <Tab>{t("history.title")}</Tab>
              <Tab>{t("settings.title")}</Tab>
            </TabList>
            <TabPanels>
              <TabPanel>
                <div className="ph-panel">
                  {jobsError && (
                    <InlineNotification
                      kind="error"
                      lowContrast
                      hideCloseButton
                      title={t("queue.loadFailed")}
                      subtitle={jobsError}
                      data-testid="queue-error"
                    />
                  )}
                  {loading && jobs.length === 0 ? (
                    <InlineLoading description={t("queue.loading")} data-testid="queue-loading" />
                  ) : (
                    !(jobsError && jobs.length === 0) && (
                      <QueueTable jobs={jobs} actions={actions} hasAnyJobs={jobs.length > 0} />
                    )
                  )}
                </div>
              </TabPanel>
              <TabPanel>
                <HistoryTable
                  refreshKey={historyKey}
                  onRedownload={(url) => {
                    setTab(TAB_QUEUE);
                    void addUrls(url);
                  }}
                />
              </TabPanel>
              <TabPanel>
                <SettingsPage
                  notificationsEnabled={notificationsEnabled}
                  onNotificationsChange={(enabled) => {
                    setNotificationsEnabled(enabled);
                    saveDesktopNotificationsEnabled(enabled);
                  }}
                  onToolchainStatus={setToolchain}
                />
              </TabPanel>
            </TabPanels>
          </Tabs>
        </div>
      </Content>

      <ProbeModal
        url={probeUrl}
        initialDestination={destination}
        onClose={() => dropProbe(probeUrl)}
        onConfirmed={(ids) => probeUrl && void onProbeConfirmed(probeUrl, ids)}
        onTryPlain={(url) => void tryPlain(url)}
        onOpenSettings={() => probeUrl && openCookieSettings(probeUrl)}
        onError={(title, err) => toasts.error(title, errorToString(err))}
      />
      <DiscardModal copy={discardCopy} onConfirm={() => void confirmDiscard()} onCancel={() => setDiscardIds(null)} />
      <DuplicateUrlModal
        match={duplicate?.match ?? null}
        onResolve={(resolution) => {
          duplicate?.resolve(resolution);
          setDuplicate(null);
        }}
      />
      <PromptModal request={prompt} onClose={() => setPrompt(null)} />
      <ExpectedHashModal request={hashRequest} onClose={() => setHashRequest(null)} />
      <MirrorsModal
        request={mirrorsRequest}
        currentUrl={jobs.find((j) => j.job_id === mirrorsRequest?.jobId)?.url ?? null}
        onClose={() => setMirrorsRequest(null)}
      />
    </div>
  );
}

export interface PromptRequest {
  title: string;
  label: string;
  placeholder?: string;
  confirmLabel: string;
  validate: (value: string) => string | null;
  onSubmit: (value: string) => void | Promise<void>;
}
