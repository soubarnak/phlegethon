/**
 * TypeScript mirrors of the Download Core / Tauri command serde shapes.
 * Keep field names snake_case to match Rust serialization.
 */

export type JobStatus = "pending" | "downloading" | "paused" | "failed" | "completed";

export type ExtractionTier = "dedicated" | "generic_extractor" | "page_sniff" | "direct_file";

export type MediaFetchKind = "extractor" | "direct_http";

export interface JobSnapshot {
  job_id: string;
  url: string;
  destination: string;
  status: JobStatus;
  bytes_downloaded: number;
  total_bytes: number | null;
  title: string;
  path: string | null;
  is_extractor: boolean;
  extraction_tier: ExtractionTier | null;
  last_error: string | null;
  queue_index: number;
}

export interface ActiveJobMatch {
  job_id: string;
  url: string;
  status: string;
  title: string;
}

export type EnqueueResult =
  | { type: "EnqueueDirect"; job_id: string; destination: string }
  | { type: "ProbeThenConfirm"; url: string };

export interface Quality {
  id: string;
  label: string;
  size_bytes: number | null;
  container: string | null;
}

export interface MediaItem {
  id: string;
  title: string;
  url: string;
  qualities: Quality[];
  archived: boolean;
  uploader: string | null;
  upload_date: string | null;
  fetch_kind: MediaFetchKind;
  qualities_are_presets: boolean;
  referer: string | null;
}

export interface ProbeResult {
  url: string;
  items: MediaItem[];
  source_name: string | null;
  tier: ExtractionTier;
  extractor_key: string | null;
  warnings: string[];
}

export interface ItemSelection {
  item_id: string;
  quality_id: string;
}

export interface MediaExtrasOptions {
  write_subs: boolean;
  embed_subs: boolean;
  write_chapters: boolean;
  embed_chapters: boolean;
  write_thumbnail: boolean;
  embed_thumbnail: boolean;
  write_metadata: boolean;
  embed_metadata: boolean;
  remux: boolean;
}

export const EMPTY_MEDIA_EXTRAS: MediaExtrasOptions = {
  write_subs: false,
  embed_subs: false,
  write_chapters: false,
  embed_chapters: false,
  write_thumbnail: false,
  embed_thumbnail: false,
  write_metadata: false,
  embed_metadata: false,
  remux: false,
};

export interface MediaExtrasCapabilities {
  ffmpeg_available: boolean;
  embed_disabled_reason: string | null;
}

export interface ProbeSelection {
  url: string;
  items: ItemSelection[];
  media_extras: MediaExtrasOptions | null;
}

export interface ProbeDestinationPreview {
  path_display: string;
}

export interface ProbeTierSettings {
  allow_generic: boolean;
  allow_page_sniff: boolean;
  allow_direct_fallback: boolean;
  probe_timeout_secs: number;
}

export interface DefaultQualityPolicy {
  max_height: number;
  prefer_mp4: boolean;
}

export interface CookieChannel {
  cookies_from_browser: string | null;
  cookie_file: string | null;
}

export interface ProxySettings {
  url: string | null;
}

export interface SidecarUpdateSettings {
  check_on_launch: boolean;
  toolchain_pin: string | null;
  plugin_pin: string | null;
}

export type SidecarComponent = "toolchain" | "plugin";

export interface SidecarComponentStatus {
  component: SidecarComponent;
  installed_version: string | null;
  available_version: string | null;
  pinned_version: string | null;
  update_available: boolean;
  /** Why the available version is unknown (lookup failed); absent when it worked. */
  lookup_error?: string | null;
}

export interface SidecarUpdateCheckResult {
  toolchain: SidecarComponentStatus;
  plugin: SidecarComponentStatus;
}

export type SidecarApplyOutcome =
  | "applied"
  | "deferred_until_idle"
  | "deferred_until_next_launch"
  | "nothing_to_apply"
  | "blocked_mid_transfer";

export interface SidecarApplyResult {
  outcome: SidecarApplyOutcome;
  applied: SidecarComponent[];
}

export type ArchiveOutcome = "Completed" | "Failed";

export interface ArchiveEntry {
  media_id: string;
  title: string;
  url: string;
  completed_at_unix_secs: number;
  destination: string;
  job_id: string;
  outcome: ArchiveOutcome;
}

export interface ToolInfo {
  path: string;
  version: string | null;
}

export type JsRuntimeKind = "deno" | "node" | "quick_js" | "bun";

export interface JsRuntime {
  kind: JsRuntimeKind;
  path: string;
  version: string | null;
  supported: boolean;
}

export type Libc = "glibc" | "musl" | "unknown";

export interface PlatformInfo {
  os: string;
  arch: string;
  libc: Libc;
}

export type EjsStatus = "bundled" | "python_package" | "missing" | "unknown";

export type YtDlpSource = "sidecar_manifest" | "env_var" | "path" | "python_module";

export type YtDlpLauncher =
  | { kind: "binary"; path: string }
  | { kind: "python_module"; python: string };

export interface ToolchainStatus {
  ytdlp: ToolInfo | null;
  ytdlp_source: YtDlpSource | null;
  ytdlp_launcher: YtDlpLauncher | null;
  ffmpeg: ToolInfo | null;
  ffprobe: ToolInfo | null;
  js_runtime: JsRuntime | null;
  ejs: EjsStatus;
  platform: PlatformInfo;
  sidecar_root: string | null;
  plugin_dirs: string[];
  remote_components_enabled: boolean;
  impersonation_available: boolean;
  issues: string[];
}

export type CoreEvent =
  | { type: "JobProgress"; job_id: string; bytes_downloaded: number; total_bytes: number | null }
  | { type: "JobCompleted"; job_id: string; bytes_written: number; path: string }
  | { type: "JobFailed"; job_id: string; error: string }
  | { type: "JobPaused"; job_id: string }
  | { type: "JobResumed"; job_id: string }
  | { type: "JobRetried"; job_id: string }
  | { type: "JobDiscarded"; job_id: string }
  | { type: "JobsReordered"; from_index: number; to_index: number }
  | { type: "JobMirrorFailover"; job_id: string; previous_url: string; next_url: string; error: string };

/** What the shell did with a Chrome Companion hand-off (`CompanionResult` in companion-protocol). */
export type CompanionResult =
  | { type: "enqueue_direct"; job_id: string; destination: string }
  | { type: "probe_then_confirm"; url: string };

/** Payload of the `companion-intent` event. The shell has already enqueued a direct URL. */
export interface CompanionIntent {
  url: string;
  result: CompanionResult;
}

/** Which mechanism the Rust side used for a folder choice. `manual` = ask for a typed path. */
export type PickerMethod = "portal" | "zenity" | "kdialog" | "manual";

export interface PickFolderResult {
  path: string | null;
  method: PickerMethod;
}

export interface PlatformCapabilities {
  folder_picker: PickerMethod;
  portal: boolean;
  wayland: boolean;
  notify_send: boolean;
}
