import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { MirrorsModal } from "./MirrorsModal";
import { ToastProvider } from "@/state/useToasts";
import { installFakeIpc, type CommandHandler } from "@/test/mockApi";

const request = { jobId: "m1", title: "ubuntu.iso" };
const urls = [
  "https://a.example.com/ubuntu.iso",
  "https://b.example.com/ubuntu.iso",
  "https://c.example.com/ubuntu.iso",
];

function setup(handlers: Record<string, CommandHandler> = {}, props: { currentUrl?: string | null; onClose?: () => void } = {}) {
  const ipc = installFakeIpc({ job_mirrors: () => urls, ...handlers });
  const onClose = props.onClose ?? vi.fn();
  const view = (currentUrl: string | null) => (
    <ToastProvider>
      <MirrorsModal request={request} currentUrl={currentUrl} onClose={onClose} />
    </ToastProvider>
  );
  const utils = render(view(props.currentUrl ?? null));
  return { ipc, onClose, rerender: (currentUrl: string | null) => utils.rerender(view(currentUrl)) };
}

describe("MirrorsModal", () => {
  it("lists the job's mirrors in the order the Core tries them, read-only", async () => {
    const { ipc } = setup();
    const list = await screen.findByTestId("mirror-list");
    expect(ipc.callsTo("job_mirrors")).toEqual([{ jobId: "m1" }]);
    const items = within(list).getAllByRole("listitem");
    expect(items.map((li) => li.textContent)).toEqual(urls);
    expect(screen.queryByRole("button", { name: /add|remove|delete/i })).not.toBeInTheDocument();
  });

  it("marks the mirror the job is downloading from, and moves the mark on failover", async () => {
    const { rerender } = setup({}, { currentUrl: urls[0] });
    const list = await screen.findByTestId("mirror-list");
    expect(within(list).getAllByRole("listitem")[0]).toHaveAttribute("aria-current", "true");
    expect(within(list).getByText("Current")).toBeInTheDocument();

    rerender(urls[1]);
    await waitFor(() => expect(within(list).getAllByRole("listitem")[1]).toHaveAttribute("aria-current", "true"));
    expect(within(list).getAllByRole("listitem")[0]).not.toHaveAttribute("aria-current");
    expect(within(list).getAllByText("Current")).toHaveLength(1);
  });

  it("says so when the Core reports no mirrors", async () => {
    setup({ job_mirrors: () => [] });
    expect(await screen.findByText("No mirrors are known for this job.")).toBeInTheDocument();
  });

  it("shows an error toast and closes when the mirrors cannot be read", async () => {
    const { onClose } = setup({ job_mirrors: () => Promise.reject("job m1 not found") });
    expect(await screen.findByText("Could not read the mirrors")).toBeInTheDocument();
    expect(onClose).toHaveBeenCalled();
  });

  it("renders nothing without a request", () => {
    installFakeIpc({ job_mirrors: () => urls });
    render(
      <ToastProvider>
        <MirrorsModal request={null} currentUrl={null} onClose={vi.fn()} />
      </ToastProvider>,
    );
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  it("closes from the close button", async () => {
    const user = userEvent.setup();
    const { onClose } = setup();
    await screen.findByTestId("mirror-list");
    await user.click(screen.getByRole("button", { name: "Close" }));
    expect(onClose).toHaveBeenCalled();
  });
});
