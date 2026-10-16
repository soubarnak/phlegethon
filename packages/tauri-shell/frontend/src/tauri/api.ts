/**
 * Typed wrappers over the Tauri commands registered in src-tauri/src/lib.rs.
 * Argument keys are camelCase; Tauri maps them to the Rust snake_case params.
 */
import { invoke } from "@tauri-apps/api/core";
import type {
  ActiveJobMatch,
  ArchiveEntry,
  ArchiveOutcome,
  CompanionIntent,
  CookieChannel,
  CoreEvent,
  DefaultQualityPolicy,
  EnqueueResult,
  JobSnapshot,
  MediaExtrasCapabilities,
  MediaExtrasOptions,
  ProbeDestinationPreview,
  ProbeResult,
  PickFolderResult,
  PlatformCapabilities,
  ProbeSelection,
  ProbeTierSettings,
  ProxySettings,
  Quality,
  SidecarApplyResult,
  SidecarComponent,
  SidecarUpdateCheckResult,
  SidecarUpdateSettings,
  ToolchainStatus,
} from "./types";

export const api = {
  // ---- URL input / enqueue
  splitUrls: (text: string) => invoke<string[]>("split_urls_cmd", { text }),
  normalizeUrl: (url: string) => invoke<string | null>("normalize_url_cmd", { url }),
  findActiveJobForUrl: (url: string) =>
    invoke<ActiveJobMatch | null>("find_active_job_for_url", { url }),
  enqueueUrl: (url: string, destination: string | null, forceDirect = false) =>
    invoke<EnqueueResult>("enqueue_url", { url, destination, forceDirect }),

  // ---- Probe-then-Confirm
  probeUrl: (url: string) => invoke<ProbeResult>("probe_url", { url }),
  cancelProbe: (url: string) => invoke<boolean>("cancel_probe", { url }),
  clearPendingProbe: (url: string) => invoke<void>("clear_pending_probe", { url }),
  enrichProbeItemQualities: (probeUrl: string, itemId: string) =>
    invoke<Quality[]>("enrich_probe_item_qualities", { probeUrl, itemId }),
  previewProbeDestination: (selection: ProbeSelection, destination: string | null) =>
    invoke<ProbeDestinationPreview>("preview_probe_destination", { selection, destination }),
  confirmProbeSelection: (selection: ProbeSelection, destination: string | null) =>
    invoke<string[]>("confirm_probe_selection", { selection, destination }),
  selectDefaultQuality: (qualities: Quality[]) =>
    invoke<string | null>("select_default_quality", { qualities }),

  // ---- Queue
  listJobs: () => invoke<JobSnapshot[]>("list_jobs"),
  processPending: () => invoke<void>("process_pending"),
  /** Companion intents that arrived before mount. Call once on mount, after subscribing to `companion-intent` (M-69). */
  takePendingIntents: () => invoke<CompanionIntent[]>("take_pending_intents"),
  pauseJob: (jobId: string) => invoke<void>("pause_job", { jobId }),
  resumeJob: (jobId: string) => invoke<void>("resume_job", { jobId }),
  retryJob: (jobId: string) => invoke<void>("retry_job", { jobId }),
  discardJob: (jobId: string) => invoke<void>("discard_job", { jobId }),
  reorderJob: (fromIndex: number, toIndex: number) =>
    invoke<void>("reorder_job", { fromIndex, toIndex }),
  revealOutput: (path: string) => invoke<void>("reveal_output", { path }),
  openOutput: (path: string) => invoke<void>("open_output", { path }),
  addJobMirror: (jobId: string, url: string) => invoke<void>("add_job_mirror", { jobId, url }),
  jobMirrors: (jobId: string) => invoke<string[]>("job_mirrors", { jobId }),
  setJobBandwidthCap: (jobId: string, cap: number | null) =>
    invoke<void>("set_job_bandwidth_cap", { jobId, cap }),
  getJobBandwidthCap: (jobId: string) => invoke<number | null>("get_job_bandwidth_cap", { jobId }),
  setJobExpectedHash: (jobId: string, algorithm: string, digest: string) =>
    invoke<void>("set_job_expected_hash", { jobId, algorithm, digest }),

  // ---- Window / files
  pickFolder: (title?: string) => invoke<PickFolderResult>("pick_folder", { title: title ?? null }),
  getPlatformCapabilities: () => invoke<PlatformCapabilities>("get_platform_capabilities"),
  notifyFallback: (summary: string, body: string) =>
    invoke<boolean>("notify_fallback", { summary, body }),
  isMainWindowFocused: () => invoke<boolean>("is_main_window_focused"),
  focusMainWindow: () => invoke<void>("focus_main_window"),

  // ---- Settings
  getLibraryRoot: () => invoke<string | null>("get_library_root"),
  setLibraryRoot: (path: string) => invoke<string>("set_library_root", { path }),
  getActiveJobLimit: () => invoke<number>("get_active_job_limit"),
  setActiveJobLimit: (limit: number) => invoke<void>("set_active_job_limit", { limit }),
  getAudioOnlyMode: () => invoke<boolean>("get_audio_only_mode"),
  setAudioOnlyMode: (enabled: boolean) => invoke<void>("set_audio_only_mode", { enabled }),
  getDefaultQualityPolicy: () => invoke<DefaultQualityPolicy>("get_default_quality_policy"),
  setDefaultQualityPolicy: (policy: DefaultQualityPolicy) =>
    invoke<void>("set_default_quality_policy", { policy }),
  getDefaultMediaExtras: () => invoke<MediaExtrasOptions>("get_default_media_extras"),
  setDefaultMediaExtras: (options: MediaExtrasOptions) =>
    invoke<void>("set_default_media_extras", { options }),
  getMediaExtrasCapabilities: () => invoke<MediaExtrasCapabilities>("get_media_extras_capabilities"),
  getCookieChannel: () => invoke<CookieChannel>("get_cookie_channel"),
  setCookieChannel: (channel: CookieChannel) => invoke<void>("set_cookie_channel", { channel }),
  getProxySettings: () => invoke<ProxySettings>("get_proxy_settings"),
  setProxySettings: (settings: ProxySettings) => invoke<void>("set_proxy_settings", { settings }),
  getFilenameTemplate: () => invoke<string>("get_filename_template"),
  setFilenameTemplate: (template: string) => invoke<void>("set_filename_template", { template }),
  previewFilenameTemplate: (template: string) =>
    invoke<string>("preview_filename_template", { template }),
  getGlobalBandwidthCap: () => invoke<number | null>("get_global_bandwidth_cap"),
  setGlobalBandwidthCap: (cap: number | null) => invoke<void>("set_global_bandwidth_cap", { cap }),
  getProbeTierSettings: () => invoke<ProbeTierSettings>("get_probe_tier_settings"),
  setProbeTierSettings: (settings: ProbeTierSettings) =>
    invoke<void>("set_probe_tier_settings", { settings }),

  // ---- Toolchain / sidecar updates
  getToolchainStatus: (force = false) => invoke<ToolchainStatus>("get_toolchain_status", { force }),
  installToolchainLatest: () => invoke<SidecarApplyResult>("install_toolchain_latest"),
  getSidecarUpdateSettings: () => invoke<SidecarUpdateSettings>("get_sidecar_update_settings"),
  setSidecarUpdateSettings: (settings: SidecarUpdateSettings) =>
    invoke<void>("set_sidecar_update_settings", { settings }),
  checkSidecarUpdates: () => invoke<SidecarUpdateCheckResult>("check_sidecar_updates"),
  applySidecarUpdates: () => invoke<SidecarApplyResult>("apply_sidecar_updates"),
  pinSidecarVersion: (component: SidecarComponent, version: string) =>
    invoke<void>("pin_sidecar_version", { component, version }),
  unpinSidecarVersion: (component: SidecarComponent) =>
    invoke<void>("unpin_sidecar_version", { component }),
  rollbackSidecar: (component: SidecarComponent) => invoke<void>("rollback_sidecar", { component }),

  // ---- History
  listHistory: () => invoke<ArchiveEntry[]>("list_history"),
  listHistoryByOutcome: (outcome: ArchiveOutcome) =>
    invoke<ArchiveEntry[]>("list_history_by_outcome", { outcome }),
  removeHistoryEntry: (mediaId: string) => invoke<void>("remove_history_entry", { mediaId }),
  historyLen: () => invoke<number>("history_len"),
  clearHistory: (outcome: ArchiveOutcome | null) => invoke<number>("clear_history", { outcome }),
};

export type Api = typeof api;
