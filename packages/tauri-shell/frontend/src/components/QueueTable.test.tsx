import { fireEvent, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { QueueTable, type QueueActions } from "./QueueTable";
import { job } from "@/test/mockApi";
import { NARROW_QUERY } from "@/state/useMediaQuery";
import type { JobRow } from "@/state/useJobs";

function row(overrides: Partial<JobRow> & { job_id: string }): JobRow {
  return { ...job(overrides), speed_bps: null, ...overrides } as JobRow;
}

function actions(): QueueActions {
  return {
    pause: vi.fn(), resume: vi.fn(), retry: vi.fn(), discard: vi.fn(), moveUp: vi.fn(), moveDown: vi.fn(),
    reveal: vi.fn(), open: vi.fn(), copyUrl: vi.fn(), addMirror: vi.fn(), setJobCap: vi.fn(),
    pauseMany: vi.fn(), resumeMany: vi.fn(), retryMany: vi.fn(),
  };
}

/** Overflow menu entries are rendered in a role=menu list; match by visible text. */
function menuItem(text: string): HTMLElement {
  const menu = document.body.querySelector<HTMLElement>("[role=menu]");
  if (!menu) throw new Error("no open overflow menu");
  return within(menu).getByText(text);
}

const jobs: JobRow[] = [
  row({ job_id: "dl", title: "Downloading clip", status: "downloading", bytes_downloaded: 50, total_bytes: 200, speed_bps: 1024 * 1024, queue_index: 0 }),
  row({ job_id: "pa", title: "Paused clip", status: "paused", queue_index: 1 }),
  row({ job_id: "fa", title: "Failed clip", status: "failed", last_error: "HTTP 403", queue_index: 2 }),
  row({ job_id: "ok", title: "Done clip", status: "completed", bytes_downloaded: 10, total_bytes: 10, path: "/d/ok.mp4", queue_index: 3 }),
  row({ job_id: "gx", title: "Generic clip", status: "pending", extraction_tier: "generic_extractor", queue_index: 4 }),
];

describe("QueueTable", () => {
  it("shows the empty state when there are no jobs", () => {
    render(<QueueTable jobs={[]} actions={actions()} hasAnyJobs={false} />);
    expect(screen.getByTestId("queue-empty")).toBeInTheDocument();
  });

  it("renders status tags, progress details, errors and the generic-extraction tag", () => {
    render(<QueueTable jobs={jobs} actions={actions()} hasAnyJobs />);
    expect(within(screen.getByTestId("queue-row-dl")).getByText("Downloading")).toBeInTheDocument();
    expect(within(screen.getByTestId("queue-row-dl")).getByText(/50 B \/ 200 B/)).toBeInTheDocument();
    expect(within(screen.getByTestId("queue-row-dl")).getByText(/1\.0 MB\/s/)).toBeInTheDocument();
    expect(within(screen.getByTestId("queue-row-pa")).getByText("Paused")).toBeInTheDocument();
    expect(within(screen.getByTestId("queue-row-fa")).getByTestId("job-error")).toHaveTextContent("HTTP 403");
    // A tagged error shows its message without the [code] and adds the hint.
    const tagged = row({ job_id: "tg", title: "Tagged", status: "failed", last_error: "[needs_browser_cookies] yt-dlp failed: HTTP Error 403", queue_index: 9 });
    const { unmount } = render(<QueueTable jobs={[tagged]} actions={actions()} hasAnyJobs />);
    const err = screen.getAllByTestId("job-error").at(-1)!;
    expect(err).toHaveTextContent("yt-dlp failed: HTTP Error 403");
    expect(err).not.toHaveTextContent("[needs_browser_cookies]");
    expect(err).toHaveTextContent(/Cloudflare/);
    unmount();
    expect(within(screen.getByTestId("queue-row-ok")).getByText("Completed")).toBeInTheDocument();
    expect(within(screen.getByTestId("queue-row-gx")).getByText("Generic extraction")).toBeInTheDocument();
  });

  it("row buttons call the matching actions", async () => {
    const a = actions();
    render(<QueueTable jobs={jobs} actions={a} hasAnyJobs />);
    const user = userEvent.setup();
    await user.click(within(screen.getByTestId("queue-row-dl")).getByRole("button", { name: /pause/i }));
    expect(a.pause).toHaveBeenCalledWith("dl");
    await user.click(within(screen.getByTestId("queue-row-pa")).getByRole("button", { name: /resume/i }));
    expect(a.resume).toHaveBeenCalledWith("pa");
    await user.click(within(screen.getByTestId("queue-row-fa")).getByRole("button", { name: /retry/i }));
    expect(a.retry).toHaveBeenCalledWith("fa");
  });

  it("overflow menu exposes move, reveal/open (completed only), copy URL and discard", async () => {
    const a = actions();
    render(<QueueTable jobs={jobs} actions={a} hasAnyJobs />);
    const user = userEvent.setup();
    await user.click(within(screen.getByTestId("queue-row-ok")).getByRole("button", { name: /actions for done clip/i }));
    await user.click(menuItem("Reveal in folder"));
    expect(a.reveal).toHaveBeenCalledWith(expect.objectContaining({ job_id: "ok" }));

    await user.click(within(screen.getByTestId("queue-row-pa")).getByRole("button", { name: /actions for paused clip/i }));
    await user.click(menuItem("Move up"));
    expect(a.moveUp).toHaveBeenCalledWith(expect.objectContaining({ job_id: "pa", queue_index: 1 }));

    await user.click(within(screen.getByTestId("queue-row-pa")).getByRole("button", { name: /actions for paused clip/i }));
    await user.click(menuItem("Discard"));
    expect(a.discard).toHaveBeenCalledWith(["pa"]);
  });

  it("filters by state and searches by text", async () => {
    render(<QueueTable jobs={jobs} actions={actions()} hasAnyJobs />);
    const user = userEvent.setup();
    await user.type(screen.getByRole("searchbox"), "failed");
    expect(screen.getByTestId("queue-row-fa")).toBeInTheDocument();
    expect(screen.queryByTestId("queue-row-dl")).not.toBeInTheDocument();
  });

  it("batch discard passes every selected id", async () => {
    const a = actions();
    render(<QueueTable jobs={jobs} actions={a} hasAnyJobs />);
    const user = userEvent.setup();
    await user.click(screen.getByRole("checkbox", { name: /select all/i }));
    await user.click(screen.getByRole("button", { name: "Discard" }));
    expect(a.discard).toHaveBeenCalledWith(expect.arrayContaining(["dl", "pa", "fa", "ok", "gx"]));
  });

  // Carbon 1.118 already drops a hidden row from `selectedRows`, so this passes without the explicit
// visible-selection filter in QueueTable too. It pins the behaviour in case a Carbon upgrade changes it.
it("batch discard leaves out selected rows the filter has hidden (M-68)", async () => {
    const a = actions();
    render(<QueueTable jobs={jobs} actions={a} hasAnyJobs />);
    const user = userEvent.setup();
    await user.click(screen.getByRole("checkbox", { name: /select all/i }));
    // The search field sits behind the batch bar while rows are selected.
    fireEvent.change(screen.getByRole("searchbox", { hidden: true }), { target: { value: "failed" } });
    expect(screen.queryByTestId("queue-row-dl")).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Discard" }));
    expect(a.discard).toHaveBeenCalledTimes(1);
    expect(a.discard).toHaveBeenCalledWith(["fa"]);
  });

  it("labels the actions column for assistive technology (UI-16)", () => {
    render(<QueueTable jobs={jobs} actions={actions()} hasAnyJobs />);
    expect(screen.getByRole("columnheader", { name: "Actions" })).toBeInTheDocument();
  });

  describe("batch actions below the narrow breakpoint (UI-4)", () => {
    function narrowViewport(narrow: boolean) {
      vi.spyOn(window, "matchMedia").mockImplementation(
        (query: string) =>
          ({
            matches: narrow && query === NARROW_QUERY,
            media: query,
            addEventListener: vi.fn(),
            removeEventListener: vi.fn(),
          }) as unknown as MediaQueryList,
      );
    }

    it("shows labelled buttons and the full selection count when wide", async () => {
      narrowViewport(false);
      render(<QueueTable jobs={jobs} actions={actions()} hasAnyJobs />);
      await userEvent.setup().click(screen.getByRole("checkbox", { name: /select all/i }));
      const bar = within(document.querySelector<HTMLElement>(".cds--batch-actions")!);
      expect(bar.getByRole("button", { name: "Discard" })).not.toHaveClass("cds--btn--icon-only");
      expect(screen.getByText("5 items selected")).toBeInTheDocument();
    });

    it("collapses to icon-only buttons that keep their names and actions", async () => {
      narrowViewport(true);
      const a = actions();
      render(<QueueTable jobs={jobs} actions={a} hasAnyJobs />);
      const user = userEvent.setup();
      await user.click(screen.getByRole("checkbox", { name: /select all/i }));
      const bar = within(document.querySelector<HTMLElement>(".cds--batch-actions")!);
      for (const name of ["Pause", "Resume", "Retry", "Discard"]) {
        expect(bar.getByRole("button", { name })).toHaveClass("cds--btn--icon-only");
      }
      expect(screen.queryByText("5 items selected")).not.toBeInTheDocument();
      expect(bar.getByRole("button", { name: "Cancel" })).toBeInTheDocument();
      await user.click(bar.getByRole("button", { name: "Discard" }));
      expect(a.discard).toHaveBeenCalledWith(expect.arrayContaining(["dl", "pa", "fa", "ok", "gx"]));
    });
  });
});
