/**
 * Helpers to stand up a fake Tauri IPC for component tests.
 * `mockIPC` intercepts `invoke`; `mockWindows` lets event listeners attach.
 */
import { mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import { vi } from "vitest";
import type { JobSnapshot, MediaItem, ProbeResult, Quality, ToolchainStatus } from "@/tauri/types";

export type CommandHandler = (args: Record<string, unknown>) => unknown | Promise<unknown>;

export interface FakeIpc {
  /** Every invoke, in order. */
  calls: Array<{ cmd: string; args: Record<string, unknown> }>;
  /** Replace or add a handler. */
  on(cmd: string, handler: CommandHandler): void;
  /** Calls to one command. */
  callsTo(cmd: string): Array<Record<string, unknown>>;
}

export function installFakeIpc(initial: Record<string, CommandHandler> = {}): FakeIpc {
  mockWindows("main");
  const handlers = new Map<string, CommandHandler>(Object.entries(initial));
  const calls: FakeIpc["calls"] = [];
  mockIPC(async (cmd, payload) => {
    const args = (payload ?? {}) as Record<string, unknown>;
    calls.push({ cmd, args });
    // Tauri event plumbing used by `listen` goes through plugin:event|listen.
    if (cmd.startsWith("plugin:event|")) {
      return 1;
    }
    const handler = handlers.get(cmd);
    if (!handler) {
      throw new Error(`fake ipc: no handler for command '${cmd}'`);
    }
    return handler(args);
  });
  return {
    calls,
    on(cmd, handler) {
      handlers.set(cmd, handler);
    },
    callsTo(cmd) {
      return calls.filter((c) => c.cmd === cmd).map((c) => c.args);
    },
  };
}

export function quality(id: string, label = id, size: number | null = null): Quality {
  return { id, label, size_bytes: size, container: id === "best" ? null : "mp4" };
}

export function mediaItem(overrides: Partial<MediaItem> & { id: string }): MediaItem {
  return {
    title: overrides.id,
    url: `https://example.com/${overrides.id}`,
    qualities: [quality("1080p"), quality("720p"), quality("best")],
    archived: false,
    uploader: null,
    upload_date: null,
    fetch_kind: "extractor",
    qualities_are_presets: false,
    referer: null,
    ...overrides,
  };
}

export function probeResult(items: MediaItem[], overrides: Partial<ProbeResult> = {}): ProbeResult {
  return {
    url: "https://www.youtube.com/playlist?list=PL1",
    items,
    source_name: items.length > 1 ? "Playlist" : null,
    tier: "dedicated",
    extractor_key: "YoutubeTab",
    warnings: [],
    ...overrides,
  };
}

export function job(overrides: Partial<JobSnapshot> & { job_id: string }): JobSnapshot {
  return {
    url: `https://example.com/${overrides.job_id}.mp4`,
    destination: `/home/u/Downloads/${overrides.job_id}`,
    status: "pending",
    bytes_downloaded: 0,
    total_bytes: null,
    title: overrides.job_id,
    path: null,
    is_extractor: false,
    extraction_tier: null,
    last_error: null,
    queue_index: 0,
    ...overrides,
  };
}

export function toolchainStatus(overrides: Partial<ToolchainStatus> = {}): ToolchainStatus {
  return {
    ytdlp: { path: "/usr/bin/yt-dlp", version: "2026.08.19" },
    ytdlp_source: "path",
    ytdlp_launcher: { kind: "binary", path: "/usr/bin/yt-dlp" },
    ffmpeg: { path: "/usr/bin/ffmpeg", version: "7.1" },
    ffprobe: { path: "/usr/bin/ffprobe", version: "7.1" },
    js_runtime: { kind: "node", path: "/usr/bin/node", version: "22.22.0", supported: true },
    ejs: "python_package",
    platform: { os: "linux", arch: "x86_64", libc: "glibc" },
    sidecar_root: "/home/u/.local/share/phlegethon",
    plugin_dirs: [],
    remote_components_enabled: false,
    impersonation_available: true,
    issues: [],
    ...overrides,
  };
}

export const noop = vi.fn();
