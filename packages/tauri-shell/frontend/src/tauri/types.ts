/**
 * TypeScript mirrors of the Download Core / Tauri command serde shapes.
 *
 * Types that cross the IPC boundary and have a Rust owner are generated from the serde types
 * (`scripts/gen-ts-types.sh`, ts-rs) into `./generated` and re-exported here: do not hand-write
 * them again. The rest are hand-written; keep field names snake_case to match Rust serialization.
 */
import type { JobSnapshot } from "./generated/JobSnapshot";

export type { ActiveJobMatch } from "./generated/ActiveJobMatch";
export type { CompanionIntentEvent as CompanionIntent } from "./generated/CompanionIntentEvent";
export type { CompanionResult } from "./generated/CompanionResult";
export type { EnqueueResult } from "./generated/EnqueueResult";
export type { EventSummary as CoreEvent } from "./generated/EventSummary";
export type { ExtractionTier } from "./generated/ExtractionTier";
export type { JobSnapshot } from "./generated/JobSnapshot";
export type { MediaFetchKind } from "./generated/MediaFetchKind";
export type { MediaItem } from "./generated/MediaItem";
export type { ProbeDestinationPreview } from "./generated/ProbeDestinationPreview";
export type { ProbeResult } from "./generated/ProbeResult";
export type { Quality } from "./generated/Quality";

export type JobStatus = JobSnapshot["status"];

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

export type SidecarChannel = "stable" | "nightly";

export interface SidecarUpdateSettings {
  check_on_launch: boolean;
  channel: SidecarChannel;
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
