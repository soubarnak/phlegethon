import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import App from "./App";
import { installFakeIpc, job, mediaItem, probeResult, quality, toolchainStatus } from "@/test/mockApi";

vi.mock("@/tauri/events", () => ({
  onCoreEvent: async () => () => undefined,
  onCompanionIntent: async () => () => undefined,
}));

const extras = { write_subs: false, embed_subs: false, write_chapters: false, embed_chapters: false, write_thumbnail: false, embed_thumbnail: false, write_metadata: false, embed_metadata: false, remux: false };

function baseHandlers(extra: Record<string, (a: Record<string, unknown>) => unknown> = {}) {
  let jobs = [] as ReturnType<typeof job>[];
  const handlers = {
    list_jobs: () => jobs,
    process_pending: () => undefined,
    get_toolchain_status: () => toolchainStatus(),
    split_urls_cmd: (a: Record<string, unknown>) => String(a.text).split(/\s+/).filter(Boolean),
    find_active_job_for_url: () => null,
    get_default_media_extras: () => extras,
    get_media_extras_capabilities: () => ({ ffmpeg_available: true, embed_disabled_reason: null }),
    select_default_quality: (a: Record<string, unknown>) => (a.qualities as Array<{ id: string }>)[0]?.id ?? null,
    preview_probe_destination: () => ({ path_display: "/lib/jobs/…" }),
    clear_pending_probe: () => undefined,
    cancel_probe: () => true,
    list_history: () => [],
    ...extra,
  };
  return { handlers, setJobs: (j: typeof jobs) => (jobs = j) };
}

beforeEach(() => {
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
        return { type: "EnqueueDirect", job_id: "j1", destination: "/lib/jobs/j1" };
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
      enqueue_url: () => ({ type: "ProbeThenConfirm", url }),
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
      enqueue_url: (a) => (a.forceDirect ? { type: "EnqueueDirect", job_id: "p1", destination: "/d" } : { type: "ProbeThenConfirm", url }),
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
});
