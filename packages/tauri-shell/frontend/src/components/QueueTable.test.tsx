import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { QueueTable, type QueueActions } from "./QueueTable";
import { job } from "@/test/mockApi";
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
});
