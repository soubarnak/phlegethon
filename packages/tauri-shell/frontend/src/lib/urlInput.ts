/** URL input helpers backed by Download Core. */
import type { ActiveJobMatch } from "@/tauri/types";

export type DuplicateResolution = "enqueue" | "focus" | "cancel";

export interface DuplicateHandlers {
  findActiveJobForUrl: (url: string) => Promise<ActiveJobMatch | null>;
  confirmDuplicate: (match: ActiveJobMatch) => Promise<DuplicateResolution>;
  focusExisting: (match: ActiveJobMatch) => void | Promise<void>;
}

export async function resolveDuplicateUrl(
  { findActiveJobForUrl, confirmDuplicate, focusExisting }: DuplicateHandlers,
  url: string,
): Promise<DuplicateResolution> {
  const match = await findActiveJobForUrl(url);
  if (!match) {
    return "enqueue";
  }
  const resolution = await confirmDuplicate(match);
  if (resolution === "focus") {
    await focusExisting(match);
  }
  return resolution;
}

export interface ProcessUrlHandlers {
  splitUrls: (text: string) => Promise<string[]>;
  resolveDuplicate: (url: string) => Promise<DuplicateResolution>;
  enqueueOne: (url: string) => Promise<void>;
}

export interface ProcessUrlResult {
  enqueued: number;
  total: number;
  skipped: string[];
}

/** Split pasted text into URLs, resolve duplicates one by one, enqueue the rest. */
export async function processUrlInput(
  { splitUrls, resolveDuplicate, enqueueOne }: ProcessUrlHandlers,
  text: string,
): Promise<ProcessUrlResult> {
  const urls = await splitUrls(text);
  if (urls.length === 0) {
    return { enqueued: 0, total: 0, skipped: [] };
  }

  let enqueued = 0;
  const skipped: string[] = [];
  for (const url of urls) {
    const resolution = await resolveDuplicate(url);
    if (resolution === "enqueue") {
      await enqueueOne(url);
      enqueued++;
    } else {
      skipped.push(url);
    }
  }

  return { enqueued, total: urls.length, skipped };
}

/** Pull plain text / URI lists out of a drop or paste `DataTransfer`. */
export function textFromDataTransfer(dt: Pick<DataTransfer, "getData" | "types"> | null): string {
  if (!dt) return "";
  const types = Array.from(dt.types ?? []);
  const uriList = types.includes("text/uri-list") ? dt.getData("text/uri-list") : "";
  const plain = types.includes("text/plain") || types.includes("text") ? dt.getData("text/plain") || dt.getData("text") : "";
  const combined = [uriList, plain]
    .filter(Boolean)
    .join("\n")
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter((line) => line && !line.startsWith("#"));
  return Array.from(new Set(combined)).join("\n");
}
