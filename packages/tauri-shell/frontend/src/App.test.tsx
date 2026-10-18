import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import App from "./App";
import { findActiveJobLikeCore, installFakeIpc, job, mediaItem, probeResult, quality, splitUrlsLikeCore, toolchainStatus } from "@/test/mockApi";

const notifier = vi.hoisted(() => ({ sent: [] as Array<{ title: string; body: string }> }));

vi.mock("@/tauri/platform", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/platform")>()),
  notificationPort: async () => ({
    isPermissionGranted: async () => true,
    requestPermission: async () => "granted",
    sendNotification: (options: { title: string; body: string }) => void notifier.sent.push(options),
  }),
}));

const extras = { write_subs: false, embed_subs: false, write_chapters: false, embed_chapters: false, write_thumbnail: false, embed_thumbnail: false, write_metadata: false, embed_metadata: false, remux: false };

function baseHandlers(extra: Record<string, (a: Record<string, unknown>) => unknown> = {}) {
  let jobs = [] as ReturnType<typeof job>[];
  const handlers = {
    list_jobs: () => jobs,
    process_pending: () => undefined,
    take_pending_intents: () => [],
    get_toolchain_status: () => toolchainStatus(),
    split_urls_cmd: (a: Record<string, unknown>) => splitUrlsLikeCore(String(a.text)),
    find_active_job_for_url: (a: Record<string, unknown>) => findActiveJobLikeCore(jobs, String(a.url)),
    get_default_media_extras: () => extras,
    get_media_extras_capabilities: () => ({ ffmpeg_available: true, embed_disabled_reason: null }),
    select_default_quality: (a: Record<string, unknown>) => (a.qualities as Array<{ id: string }>)[0]?.id ?? null,
    preview_probe_destination: () => ({ path_display: "/lib/jobs/…" }),
    clear_pending_probe: () => undefined,
    cancel_probe: () => true,
    list_history: () => [],
    is_main_window_focused: () => false,
    ...extra,
  };
  return { handlers, setJobs: (j: typeof jobs) => (jobs = j) };
}

beforeEach(() => {
  notifier.sent.length = 0;
  document.documentElement.removeAttribute("data-theme");
});

describe("App", () => {
  it("shows the toolchain tag, the empty queue and the three sections", async () => {
    installFakeIpc(baseHandlers().handlers);
    render(<App />);
    expect(await screen.findByTestId("queue-empty")).toBeInTheDocument();
    expect(await screen.findByText(/yt-dlp 2026\.08\.19/)).toBeInTheDocument();
    for (const name of ["Queue", "History", "Settings"]) {
      expect(screen.getByRole("tab", { name })).toBeInTheDocument();
    }
  });

  it("the theme button toggles Carbon g10/g100", async () => {
    installFakeIpc(baseHandlers().handlers);
    render(<App />);
    await screen.findByTestId("queue-empty");
    const before = document.documentElement.dataset.theme;
    await userEvent.setup().click(screen.getByRole("button", { name: /switch to (dark|light) theme/i }));
    expect(document.documentElement.dataset.theme).not.toBe(before);
    expect(["g10", "g100"]).toContain(document.documentElement.dataset.theme);
  });

  it("a plain file URL is queued directly and dispatched", async () => {
    const { handlers, setJobs } = baseHandlers({
      enqueue_url: () => {
        setJobs([job({ job_id: "j1", title: "file.zip", status: "pending", url: "https://example.com/file.zip" })]);
        return { type: "enqueue_direct", job_id: "j1", destination: "/lib/jobs/j1" };
      },
    });
    const ipc = installFakeIpc(handlers);
    render(<App />);
    const user = userEvent.setup();
    await user.type(await screen.findByLabelText("URLs"), "https://example.com/file.zip{Enter}");
    await waitFor(() => expect(ipc.callsTo("enqueue_url")).toHaveLength(1));
    expect(ipc.callsTo("enqueue_url")[0]).toMatchObject({ url: "https://example.com/file.zip", destination: null, forceDirect: false });
    expect(await screen.findByTestId("queue-row-j1")).toBeInTheDocument();
    expect(ipc.callsTo("process_pending").length).toBeGreaterThan(0);
  });

  it("a page URL opens the probe sheet; confirming queues jobs and returns to the queue", async () => {
    const url = "https://www.youtube.com/playlist?list=PL1";
    const items = [mediaItem({ id: "yt-a", title: "First", qualities: [quality("720p")] }), mediaItem({ id: "yt-b", title: "Second", qualities: [quality("720p")] })];
    const { handlers, setJobs } = baseHandlers({
      enqueue_url: () => ({ type: "probe_then_confirm", url }),
      probe_url: () => probeResult(items, { url }),
      confirm_probe_selection: () => {
        setJobs([job({ job_id: "n1", title: "First", queue_index: 0 }), job({ job_id: "n2", title: "Second", queue_index: 1 })]);
        return ["n1", "n2"];
      },
    });
    const ipc = installFakeIpc(handlers);
    render(<App />);
    const user = userEvent.setup();
    await user.type(await screen.findByLabelText("URLs"), `${url}{Enter}`);
    const confirm = await screen.findByTestId("probe-confirm");
    expect(confirm).toHaveTextContent("Download 2 selected");
    await user.click(confirm);
    await waitFor(() => expect(ipc.callsTo("confirm_probe_selection")).toHaveLength(1));
    expect(await screen.findByTestId("queue-row-n1")).toBeInTheDocument();
    expect(screen.queryByTestId("probe-confirm")).not.toBeInTheDocument();
  });

  it("a failed probe offers a plain download that queues the URL as a file", async () => {
    const url = "https://files.example.org/download?id=7";
    const { handlers } = baseHandlers({
      enqueue_url: (a) => (a.forceDirect ? { type: "enqueue_direct", job_id: "p1", destination: "/d" } : { type: "probe_then_confirm", url }),
      probe_url: () => {
        throw "no downloadable media found for the page. yt-dlp: unsupported";
      },
    });
    const ipc = installFakeIpc(handlers);
    render(<App />);
    const user = userEvent.setup();
    await user.type(await screen.findByLabelText("URLs"), `${url}{Enter}`);
    await user.click(await screen.findByRole("button", { name: "Try plain download" }));
    await waitFor(() => expect(ipc.callsTo("enqueue_url").some((c) => c.forceDirect === true)).toBe(true));
  });

  it("discarding asks for confirmation first, then calls discard_job", async () => {
    const { handlers } = baseHandlers({ discard_job: () => undefined });
    handlers.list_jobs = () => [job({ job_id: "d1", title: "Doomed", status: "paused", destination: "/lib/jobs/d1" })];
    const ipc = installFakeIpc(handlers);
    render(<App />);
    const user = userEvent.setup();
    const row = await screen.findByTestId("queue-row-d1");
    await user.click(within(row).getByRole("button", { name: /actions for doomed/i }));
    const menu = document.body.querySelector<HTMLElement>("[role=menu]")!;
    await user.click(within(menu).getByText("Discard"));
    expect(ipc.callsTo("discard_job")).toHaveLength(0);
    const dialog = await screen.findByRole("dialog");
    expect(dialog).toHaveTextContent("/lib/jobs/d1");
    await user.click(within(dialog).getByRole("button", { name: "Discard" }));
    await waitFor(() => expect(ipc.callsTo("discard_job")).toEqual([{ jobId: "d1" }]));
  });

  it("Set expected hash from the row menu sends the digest for that job", async () => {
    const { handlers } = baseHandlers({ set_job_expected_hash: () => undefined });
    handlers.list_jobs = () => [job({ job_id: "h1", title: "Checked file", status: "pending" })];
    const ipc = installFakeIpc(handlers);
    render(<App />);
    const user = userEvent.setup();
    const row = await screen.findByTestId("queue-row-h1");
    await user.click(within(row).getByRole("button", { name: /actions for checked file/i }));
    const menu = document.body.querySelector<HTMLElement>("[role=menu]")!;
    await user.click(within(menu).getByText("Set expected hash…"));
    const dialog = await screen.findByRole("dialog");
    await user.type(within(dialog).getByLabelText("Digest"), "A".repeat(64));
    await user.click(within(dialog).getByRole("button", { name: "Set hash" }));
    await waitFor(() =>
      expect(ipc.callsTo("set_job_expected_hash")).toEqual([{ jobId: "h1", algorithm: "sha-256", digest: "a".repeat(64) }]),
    );
  });

  it("Mirrors from the row menu lists the job's Mirror Set and marks the URL in use", async () => {
    const { handlers } = baseHandlers({
      job_mirrors: () => ["https://one.example.com/f.iso", "https://two.example.com/f.iso"],
    });
    handlers.list_jobs = () => [job({ job_id: "mv", title: "Mirrored file", url: "https://two.example.com/f.iso", status: "downloading" })];
    const ipc = installFakeIpc(handlers);
    render(<App />);
    const user = userEvent.setup();
    const row = await screen.findByTestId("queue-row-mv");
    await user.click(within(row).getByRole("button", { name: /actions for mirrored file/i }));
    const menu = document.body.querySelector<HTMLElement>("[role=menu]")!;
    await user.click(within(menu).getByText("Mirrors…"));
    const list = await within(await screen.findByRole("dialog")).findByTestId("mirror-list");
    expect(ipc.callsTo("job_mirrors")).toEqual([{ jobId: "mv" }]);
    const items = within(list).getAllByRole("listitem");
    expect(items.map((li) => li.textContent)).toEqual(["https://one.example.com/f.iso", "https://two.example.com/f.isoCurrent"]);
  });

  it("History tab lists entries with working remove and clear actions", async () => {
    const entries = [{ media_id: "m1", title: "Saved video", url: "https://x/1", completed_at_unix_secs: 1, destination: "/d/m1", job_id: "j", outcome: "Completed" }];
    const { handlers } = baseHandlers({
      list_history: () => entries,
      remove_history_entry: () => undefined,
      clear_history: () => 1,
      reveal_output: () => undefined,
    });
    const ipc = installFakeIpc(handlers);
    render(<App />);
    const user = userEvent.setup();
    await user.click(await screen.findByRole("tab", { name: "History" }));
    const row = await screen.findByTestId("history-row-m1");
    expect(row).toHaveTextContent("Saved video");
    await user.click(within(row).getByRole("button", { name: /actions for saved video/i }));
    const menu = document.body.querySelector<HTMLElement>("[role=menu]")!;
    await user.click(within(menu).getByText("Reveal in folder"));
    await waitFor(() => expect(ipc.callsTo("reveal_output")).toEqual([{ path: "/d/m1" }]));
    await user.click(screen.getByRole("button", { name: "Clear history" }));
    const dialog = await screen.findByRole("dialog");
    await user.click(within(dialog).getByRole("button", { name: "Clear history" }));
    await waitFor(() => expect(ipc.callsTo("clear_history")).toEqual([{ outcome: null }]));
  });

  it("shows an error, not the empty-queue tile, when the queue cannot be loaded", async () => {
    installFakeIpc(baseHandlers({ list_jobs: () => Promise.reject("core unavailable") }).handlers);
    render(<App />);
    expect(await screen.findByTestId("queue-error")).toHaveTextContent("core unavailable");
    expect(screen.queryByTestId("queue-empty")).not.toBeInTheDocument();
  });

  it("notifies for every job that finishes in the same burst of events", async () => {
    const { handlers } = baseHandlers();
    handlers.list_jobs = () => [job({ job_id: "n1", title: "One" }), job({ job_id: "n2", title: "Two", queue_index: 1 })];
    const ipc = installFakeIpc(handlers);
    render(<App />);
    await screen.findByTestId("queue-row-n1");
    await settle();

    await ipc.emit("core-event", { type: "JobCompleted", job_id: "n1", bytes_written: 1, path: "/n1" });
    await ipc.emit("core-event", { type: "JobFailed", job_id: "n2", error: "boom" });

    await waitFor(() => expect(notifier.sent).toHaveLength(2));
    expect(notifier.sent.map((n) => n.body)).toEqual(["One", "Two: boom"]);
  });

  describe("URL submission", () => {
    const queued = (id: string, url: string) => job({ job_id: id, url, title: id, status: "pending" });

    it("asks before queueing a URL that already has an active job, and Cancel leaves it alone", async () => {
      const { handlers, setJobs } = baseHandlers();
      setJobs([queued("e1", "https://example.com/file.zip")]);
      const ipc = installFakeIpc(handlers);
      render(<App />);
      const user = userEvent.setup();
      await user.type(await screen.findByLabelText("URLs"), "https://example.com/file.zip{Enter}");
      const dialog = await screen.findByRole("dialog");
      expect(dialog).toHaveTextContent("already in the queue");
      await user.click(within(dialog).getByRole("button", { name: "Cancel" }));
      await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
      expect(ipc.callsTo("enqueue_url")).toHaveLength(0);
    });

    it("splits on commas and semicolons like the Core does", async () => {
      const { handlers } = baseHandlers({ enqueue_url: (a) => ({ type: "enqueue_direct", job_id: String(a.url), destination: "/d" }) });
      const ipc = installFakeIpc(handlers);
      render(<App />);
      const user = userEvent.setup();
      await user.type(await screen.findByLabelText("URLs"), "https://a.example/1,https://b.example/2;https://c.example/3{Enter}");
      await waitFor(() => expect(ipc.callsTo("enqueue_url")).toHaveLength(3));
    });

    it("a second submission waits for the open duplicate prompt instead of replacing it (M-64)", async () => {
      const { handlers, setJobs } = baseHandlers();
      setJobs([queued("e1", "https://a.example/one"), queued("e2", "https://b.example/two")]);
      const ipc = installFakeIpc(handlers);
      render(<App />);
      await screen.findByTestId("queue-row-e1");
      const paste = (text: string) => {
        const event = new Event("paste", { bubbles: true, cancelable: true }) as Event & { clipboardData: unknown };
        event.clipboardData = { getData: () => text };
        act(() => void document.body.dispatchEvent(event));
      };
      paste("https://a.example/one");
      const first = await screen.findByRole("dialog");
      expect(first).toHaveTextContent("https://a.example/one");
      paste("https://b.example/two");

      await userEvent.setup().click(within(first).getByRole("button", { name: "Cancel" }));
      const second = await screen.findByRole("dialog");
      expect(second).toHaveTextContent("https://b.example/two");
      await userEvent.setup().click(within(second).getByRole("button", { name: "Add anyway" }));
      await waitFor(() => expect(ipc.callsTo("enqueue_url")).toEqual([expect.objectContaining({ url: "https://b.example/two" })]));
    });

    it("one failing link does not stop the rest, and the toast says how many failed (M-67)", async () => {
      const { handlers } = baseHandlers({
        enqueue_url: (a) => {
          if (a.url === "https://b.example/2") throw "disk full";
          return { type: "enqueue_direct", job_id: String(a.url), destination: "/d" };
        },
      });
      const ipc = installFakeIpc(handlers);
      render(<App />);
      await userEvent.setup().type(await screen.findByLabelText("URLs"), "https://a.example/1 https://b.example/2 https://c.example/3{Enter}");
      await waitFor(() => expect(ipc.callsTo("enqueue_url")).toHaveLength(3));
      expect(await screen.findByText("1 of 3 links could not be added")).toBeInTheDocument();
      expect(screen.getByText(/https:\/\/b\.example\/2: disk full/)).toBeInTheDocument();
    });
  });

  describe("probe queue", () => {
    it("Cancel is disabled while confirming, so the next URL in the queue is not skipped (M-63)", async () => {
      const urlA = "https://a.example/watch?v=1";
      const urlB = "https://b.example/watch?v=2";
      let release: (ids: string[]) => void = () => undefined;
      const { handlers } = baseHandlers({
        enqueue_url: (a) => ({ type: "probe_then_confirm", url: String(a.url) }),
        probe_url: (a) => probeResult([mediaItem({ id: `m-${a.url}`, qualities: [quality("720p")] })], { url: String(a.url) }),
        confirm_probe_selection: () => new Promise<string[]>((resolve) => (release = resolve)),
      });
      const ipc = installFakeIpc(handlers);
      render(<App />);
      const user = userEvent.setup();
      await user.type(await screen.findByLabelText("URLs"), `${urlA} ${urlB}{Enter}`);
      await user.click(await screen.findByTestId("probe-confirm"));
      const cancel = screen.getByRole("button", { name: "Cancel" });
      expect(cancel).toBeDisabled();
      await user.click(cancel);
      expect(ipc.callsTo("clear_pending_probe")).toHaveLength(0);

      release(["job-a"]);
      await waitFor(() => expect(ipc.callsTo("probe_url").map((c) => c.url)).toEqual([urlA, urlB]));
      expect(await screen.findByTestId("probe-confirm")).toBeInTheDocument();
    });
  });

  it("the discard dialog follows the job: no file-deletion warning once it has completed (L-60)", async () => {
    let jobs = [job({ job_id: "d1", title: "Racing", status: "downloading", destination: "/lib/jobs/d1" })];
    const { handlers } = baseHandlers({ discard_job: () => undefined });
    handlers.list_jobs = () => jobs;
    const ipc = installFakeIpc(handlers);
    render(<App />);
    const user = userEvent.setup();
    const row = await screen.findByTestId("queue-row-d1");
    await user.click(within(row).getByRole("button", { name: /actions for racing/i }));
    await user.click(within(document.body.querySelector<HTMLElement>("[role=menu]")!).getByText("Discard"));
    expect(await screen.findByRole("dialog")).toHaveTextContent("Files under the Job Destination will be deleted");

    jobs = [job({ job_id: "d1", title: "Racing", status: "completed", destination: "/lib/jobs/d1" })];
    await ipc.emit("core-event", { type: "JobCompleted", job_id: "d1", bytes_written: 1, path: "/lib/jobs/d1/f" });
    await waitFor(() => expect(screen.getByRole("dialog")).toHaveTextContent("Files on disk will not be deleted"));
  });

  describe("Chrome Companion hand-off", () => {
    it("a direct hand-off shows and starts the job the shell already enqueued, without enqueueing it again", async () => {
      const { handlers, setJobs } = baseHandlers();
      const ipc = installFakeIpc(handlers);
      render(<App />);
      await screen.findByTestId("queue-empty");
      await settle();

      // The shell enqueued the URL before it emitted the intent, and enqueue emits no core-event.
      setJobs([job({ job_id: "c1", title: "linked.zip", url: "https://example.com/linked.zip" })]);
      await ipc.emit("companion-intent", {
        url: "https://example.com/linked.zip",
        result: { type: "enqueue_direct", job_id: "c1", destination: "/lib/jobs/c1" },
      });

      expect(await screen.findByTestId("queue-row-c1")).toBeInTheDocument();
      await waitFor(() => expect(ipc.callsTo("process_pending").length).toBeGreaterThan(0));
      expect(ipc.callsTo("enqueue_url")).toHaveLength(0);
      expect(ipc.callsTo("find_active_job_for_url")).toHaveLength(0);
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    });

    it("a hand-off that arrived before the webview mounted is delivered on mount", async () => {
      const url = "https://www.youtube.com/watch?v=early";
      const { handlers } = baseHandlers({
        take_pending_intents: () => [{ url, result: { type: "probe_then_confirm", url } }],
        probe_url: () => probeResult([mediaItem({ id: "yt-e", title: "Early bird", qualities: [quality("720p")] })], { url }),
      });
      const ipc = installFakeIpc(handlers);
      render(<App />);

      expect(await screen.findByTestId("probe-confirm")).toBeInTheDocument();
      expect(ipc.callsTo("take_pending_intents")).toHaveLength(1);
      expect(ipc.callsTo("probe_url")).toEqual([expect.objectContaining({ url })]);
    });

    it("a page hand-off opens the probe sheet instead of enqueueing", async () => {
      const url = "https://www.youtube.com/watch?v=abc";
      const { handlers } = baseHandlers({
        probe_url: () => probeResult([mediaItem({ id: "yt-a", title: "From the browser", qualities: [quality("720p")] })], { url }),
      });
      const ipc = installFakeIpc(handlers);
      render(<App />);
      await screen.findByTestId("queue-empty");
      await settle();

      await ipc.emit("companion-intent", { url, result: { type: "probe_then_confirm", url } });

      expect(await screen.findByTestId("probe-confirm")).toBeInTheDocument();
      expect(ipc.callsTo("probe_url")).toEqual([expect.objectContaining({ url })]);
      expect(ipc.callsTo("enqueue_url")).toHaveLength(0);
      expect(ipc.callsTo("find_active_job_for_url")).toHaveLength(0);
    });
  });
});

/** Let the app's async `listen` subscriptions attach before a test emits an event. */
async function settle() {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
}
