import { describe, expect, it } from "vitest";
import samples from "./contract/samples.json";
import type {
  ActiveJobMatch,
  ArchiveEntry,
  CompanionIntent,
  CompanionResult,
  CoreEvent,
  EnqueueResult,
  ExpectedHash,
  JobSnapshot,
  MediaItem,
  ProbeDestinationPreview,
  ProbeResult,
  Quality,
} from "./types";

/**
 * M-71: `contract/samples.json` is also checked by the Rust tests (`ipc_contract.rs` in
 * download-core and companion-protocol, `tests` in the shell crate) against the real serde output.
 * The types come from ts-rs (`scripts/gen-ts-types.sh`). Here every sample must have exactly the
 * fields of its generated type, so renaming or adding a field on either side fails a test.
 *
 * `Shape<T>` makes the key lists below exhaustive at compile time: a field missing from a list, or
 * an extra one, is a type error after the generated types change.
 */
type Shape<T> = { [K in keyof T]-?: true };
type ByTag<T extends { type: string }> = { [E in T as E["type"]]: Shape<E> };

const shape = <T,>(s: Shape<T>): string[] => Object.keys(s).sort();

const JOB_SNAPSHOT: Shape<JobSnapshot> = {
  job_id: true, url: true, destination: true, status: true, bytes_downloaded: true, total_bytes: true,
  title: true, path: true, is_extractor: true, extraction_tier: true, last_error: true, queue_index: true,
};
const ACTIVE_JOB_MATCH: Shape<ActiveJobMatch> = { job_id: true, url: true, status: true, title: true };
const ARCHIVE_ENTRY: Shape<ArchiveEntry> = {
  media_id: true, title: true, url: true, completed_at_unix_secs: true, destination: true, job_id: true,
  outcome: true, size_bytes: true,
};
const EXPECTED_HASH: Shape<ExpectedHash> = { algorithm: true, digest: true };
const PROBE_DESTINATION_PREVIEW: Shape<ProbeDestinationPreview> = { path_display: true };
const QUALITY: Shape<Quality> = { id: true, label: true, size_bytes: true, container: true };
const MEDIA_ITEM: Shape<MediaItem> = {
  id: true, title: true, url: true, qualities: true, archived: true, uploader: true, upload_date: true,
  fetch_kind: true, qualities_are_presets: true, referer: true,
};
const PROBE_RESULT: Shape<ProbeResult> = {
  url: true, items: true, source_name: true, tier: true, extractor_key: true, warnings: true,
};
const EVENT_SUMMARY: ByTag<CoreEvent> = {
  JobProgress: { type: true, job_id: true, bytes_downloaded: true, total_bytes: true },
  JobCompleted: { type: true, job_id: true, bytes_written: true, path: true },
  JobFailed: { type: true, job_id: true, error: true },
  JobPaused: { type: true, job_id: true },
  JobResumed: { type: true, job_id: true },
  JobRetried: { type: true, job_id: true },
  JobDiscarded: { type: true, job_id: true },
  JobMirrorFailover: { type: true, job_id: true, previous_url: true, next_url: true, error: true },
};
const ENQUEUE_RESULT: ByTag<EnqueueResult> = {
  enqueue_direct: { type: true, job_id: true, destination: true },
  probe_then_confirm: { type: true, url: true },
};
const COMPANION_RESULT: ByTag<CompanionResult> = ENQUEUE_RESULT;
const COMPANION_INTENT: Shape<CompanionIntent> = { url: true, result: true };

const keys = (value: object) => Object.keys(value).sort();

function expectTagged(list: Array<{ type: string }>, shapes: Record<string, Shape<unknown>>) {
  expect(list.map((s) => s.type).sort()).toEqual(Object.keys(shapes).sort());
  for (const sample of list) expect(keys(sample)).toEqual(keys(shapes[sample.type]));
}

describe("IPC contract samples (M-71)", () => {
  it("JobSnapshot", () => {
    expect(keys(samples.JobSnapshot)).toEqual(shape(JOB_SNAPSHOT));
    expect(["pending", "downloading", "paused", "failed", "completed"]).toContain(samples.JobSnapshot.status);
  });

  it("ActiveJobMatch includes the probing status that is not a job status", () => {
    for (const match of samples.ActiveJobMatch) expect(keys(match)).toEqual(shape(ACTIVE_JOB_MATCH));
    expect(samples.ActiveJobMatch.map((m) => m.status)).toContain("probing");
  });

  it("ArchiveEntry covers both outcomes and a missing size", () => {
    for (const entry of samples.ArchiveEntry) expect(keys(entry)).toEqual(shape(ARCHIVE_ENTRY));
    expect(samples.ArchiveEntry.map((e) => e.outcome).sort()).toEqual(["Completed", "Failed"]);
    expect(samples.ArchiveEntry.some((e) => e.size_bytes === null)).toBe(true);
  });

  it("ExpectedHash", () => {
    expect(keys(samples.ExpectedHash)).toEqual(shape(EXPECTED_HASH));
  });

  it("probe types", () => {
    expect(keys(samples.ProbeDestinationPreview)).toEqual(shape(PROBE_DESTINATION_PREVIEW));
    expect(keys(samples.ProbeResult)).toEqual(shape(PROBE_RESULT));
    const item = samples.ProbeResult.items[0];
    expect(keys(item)).toEqual(shape(MEDIA_ITEM));
    expect(keys(item.qualities[0])).toEqual(shape(QUALITY));
  });

  it("enums are the snake_case strings the frontend compares against", () => {
    expect(samples.ExtractionTier).toEqual(["dedicated", "generic_extractor", "page_sniff", "direct_file"]);
    expect(samples.MediaFetchKind).toEqual(["extractor", "direct_http"]);
  });

  it("every core-event variant", () => {
    expectTagged(samples.EventSummary, EVENT_SUMMARY);
  });

  it("EnqueueResult and CompanionResult share one tag casing", () => {
    expectTagged(samples.EnqueueResult, ENQUEUE_RESULT);
    expectTagged(samples.CompanionResult, COMPANION_RESULT);
    expect(samples.EnqueueResult).toEqual(samples.CompanionResult);
  });

  it("CompanionIntent carries the shell's result", () => {
    for (const intent of samples.CompanionIntentEvent) {
      expect(keys(intent)).toEqual(shape(COMPANION_INTENT));
      expect(Object.keys(COMPANION_RESULT)).toContain(intent.result.type);
    }
  });
});
