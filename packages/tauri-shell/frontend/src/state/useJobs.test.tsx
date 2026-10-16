import { act, renderHook, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { installFakeIpc, job } from "@/test/mockApi";
import { PROGRESS_FLUSH_MS, useJobs } from "./useJobs";

function setup(jobs = [job({ job_id: "a", status: "downloading", total_bytes: 1000 })]) {
  let current = jobs;
  const ipc = installFakeIpc({
    list_jobs: () => current,
    process_pending: () => undefined,
  });
  return { ipc, setJobs: (next: typeof jobs) => (current = next) };
}

/** Wait until the hook's `listen` subscription is attached. */
async function settle() {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

const progress = (job_id: string, bytes_downloaded: number) => ({
  type: "JobProgress",
  job_id,
  bytes_downloaded,
  total_bytes: 1000,
});

describe("useJobs", () => {
  it("loads the snapshot and clears loading", async () => {
    setup();
    const { result } = renderHook(() => useJobs());
    expect(result.current.loading).toBe(true);
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.jobs.map((j) => j.job_id)).toEqual(["a"]);
    expect(result.current.error).toBeNull();
  });

  it("reports a failed list_jobs as an error, not an empty queue", async () => {
    const ipc = installFakeIpc({ list_jobs: () => Promise.reject("core unavailable") });
    const { result } = renderHook(() => useJobs());
    await waitFor(() => expect(result.current.error).toBe("core unavailable"));
    expect(result.current.loading).toBe(false);
    expect(ipc.callsTo("list_jobs").length).toBeGreaterThan(0);
  });

  it("coalesces a burst of progress events into one update with the latest bytes", async () => {
    const { ipc } = setup();
    let renders = 0;
    const { result } = renderHook(() => {
      renders += 1;
      return useJobs();
    });
    await waitFor(() => expect(result.current.loading).toBe(false));
    await settle();
    const rendersBefore = renders;

    await act(async () => {
      for (let bytes = 1; bytes <= 200; bytes++) await ipc.emit("core-event", progress("a", bytes));
    });
    expect(result.current.jobs[0].bytes_downloaded).toBe(0);

    await waitFor(() => expect(result.current.jobs[0].bytes_downloaded).toBe(200), { timeout: PROGRESS_FLUSH_MS * 10 });
    expect(renders - rendersBefore).toBeLessThanOrEqual(3);
    expect(result.current.events).toEqual([]);
  });

  it("keeps the same row object for jobs a refresh did not change", async () => {
    const { setJobs } = setup([job({ job_id: "a", status: "paused" }), job({ job_id: "b", status: "paused", queue_index: 1 })]);
    const { result } = renderHook(() => useJobs());
    await waitFor(() => expect(result.current.jobs).toHaveLength(2));
    const [a, b] = result.current.jobs;

    setJobs([job({ job_id: "a", status: "paused" }), job({ job_id: "b", status: "failed", queue_index: 1 })]);
    await act(() => result.current.refresh());
    expect(result.current.jobs[0]).toBe(a);
    expect(result.current.jobs[1]).not.toBe(b);
  });

  it("queues every completion and failure instead of keeping only the last", async () => {
    const { ipc } = setup([job({ job_id: "a" }), job({ job_id: "b", queue_index: 1 })]);
    const { result } = renderHook(() => useJobs());
    await waitFor(() => expect(result.current.loading).toBe(false));
    await settle();

    await act(async () => {
      await ipc.emit("core-event", { type: "JobCompleted", job_id: "a", bytes_written: 1, path: "/a" });
      await ipc.emit("core-event", { type: "JobFailed", job_id: "b", error: "boom" });
    });

    expect(result.current.events.map((e) => [e.event.type, e.event.job_id])).toEqual([
      ["JobCompleted", "a"],
      ["JobFailed", "b"],
    ]);
    const ids = result.current.events.map((e) => e.id);
    expect(new Set(ids).size).toBe(2);

    act(() => result.current.dismissEvents(ids[0]));
    expect(result.current.events.map((e) => e.id)).toEqual([ids[1]]);
    act(() => result.current.dismissEvents(ids[1]));
    expect(result.current.events).toEqual([]);
  });
});
