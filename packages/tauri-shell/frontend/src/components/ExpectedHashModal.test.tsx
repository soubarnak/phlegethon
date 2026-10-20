import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { ExpectedHashModal } from "./ExpectedHashModal";
import { ToastProvider } from "@/state/useToasts";
import { installFakeIpc, type CommandHandler } from "@/test/mockApi";

const request = { jobId: "j1", title: "ubuntu.iso" };
const hex = (length: number) => "0123456789abcdef".repeat(8).slice(0, length);

function setup(handlers: Record<string, CommandHandler> = {}, onClose = vi.fn()) {
  const ipc = installFakeIpc({
    set_job_expected_hash: () => undefined,
    get_job_expected_hash: () => null,
    clear_job_expected_hash: () => undefined,
    ...handlers,
  });
  render(
    <ToastProvider>
      <ExpectedHashModal request={request} onClose={onClose} />
    </ToastProvider>,
  );
  return { ipc, onClose, user: userEvent.setup() };
}

describe("ExpectedHashModal", () => {
  it("renders nothing without a request", () => {
    installFakeIpc();
    render(
      <ToastProvider>
        <ExpectedHashModal request={null} onClose={vi.fn()} />
      </ToastProvider>,
    );
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  it("keeps Set hash disabled until a digest is entered", async () => {
    const { user } = setup();
    const save = screen.getByRole("button", { name: "Set hash" });
    expect(save).toBeDisabled();
    await user.type(screen.getByLabelText("Digest"), hex(64));
    expect(save).toBeEnabled();
  });

  it("says how many characters the digest has when the length is wrong", async () => {
    const { user } = setup();
    await user.type(screen.getByLabelText("Digest"), hex(40));
    expect(screen.getByText("Got 40 characters; expected 64.")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Set hash" })).toBeDisabled();
  });

  it("rejects non-hex characters", async () => {
    const { user } = setup();
    await user.type(screen.getByLabelText("Digest"), `z${hex(63).slice(1)}`);
    expect(screen.getByText("Use only the characters 0–9 and a–f.")).toBeInTheDocument();
  });

  it("validates against the length of the selected algorithm", async () => {
    const { user } = setup();
    await user.selectOptions(screen.getByLabelText("Algorithm"), "md5");
    await user.type(screen.getByLabelText("Digest"), hex(32));
    expect(screen.getByRole("button", { name: "Set hash" })).toBeEnabled();
  });

  it("saves the normalized digest with the chosen algorithm and reports success", async () => {
    const { ipc, onClose, user } = setup();
    await user.selectOptions(screen.getByLabelText("Algorithm"), "sha-1");
    await user.type(screen.getByLabelText("Digest"), ` ${hex(40).toUpperCase().slice(0, 20)} ${hex(40).slice(20)} `);
    await user.click(screen.getByRole("button", { name: "Set hash" }));

    await waitFor(() => expect(ipc.callsTo("set_job_expected_hash")).toHaveLength(1));
    expect(ipc.callsTo("set_job_expected_hash")[0]).toEqual({ jobId: "j1", algorithm: "sha-1", digest: hex(40) });
    expect(await screen.findByText("Expected hash saved")).toBeInTheDocument();
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("submits on Enter from the digest field", async () => {
    const { ipc, user } = setup();
    await user.type(screen.getByLabelText("Digest"), `${hex(64)}{Enter}`);
    await waitFor(() => expect(ipc.callsTo("set_job_expected_hash")).toHaveLength(1));
    expect(ipc.callsTo("set_job_expected_hash")[0]).toMatchObject({ algorithm: "sha-256" });
  });

  it("keeps the dialog open and shows the error when the Core rejects the hash", async () => {
    const { onClose, user } = setup({
      set_job_expected_hash: () => Promise.reject("job j1 not found"),
    });
    await user.type(screen.getByLabelText("Digest"), hex(64));
    await user.click(screen.getByRole("button", { name: "Set hash" }));

    expect(await screen.findByText("Could not set the expected hash")).toBeInTheDocument();
    expect(onClose).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: "Set hash" })).toBeEnabled();
  });

  it("Cancel closes without calling the Core", async () => {
    const { ipc, onClose, user } = setup();
    await user.click(screen.getByRole("button", { name: "Cancel" }));
    expect(onClose).toHaveBeenCalled();
    expect(ipc.callsTo("set_job_expected_hash")).toHaveLength(0);
  });

  it("shows the current expected hash and clears it", async () => {
    const { ipc, onClose, user } = setup({
      get_job_expected_hash: () => ({ algorithm: "sha-1", digest: hex(40) }),
    });
    expect(await screen.findByTestId("expected-hash-current")).toHaveTextContent(hex(40));
    expect(screen.getByText("Current expected hash (SHA-1):")).toBeInTheDocument();
    expect(ipc.callsTo("get_job_expected_hash")).toEqual([{ jobId: "j1" }]);

    await user.click(screen.getByRole("button", { name: "Clear expected hash" }));
    await waitFor(() => expect(ipc.callsTo("clear_job_expected_hash")).toEqual([{ jobId: "j1" }]));
    expect(await screen.findByText("Expected hash cleared")).toBeInTheDocument();
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("offers no Clear button when the job has no expected hash", async () => {
    const { ipc } = setup();
    await waitFor(() => expect(ipc.callsTo("get_job_expected_hash")).toHaveLength(1));
    expect(screen.queryByRole("button", { name: "Clear expected hash" })).not.toBeInTheDocument();
    expect(screen.queryByTestId("expected-hash-current")).not.toBeInTheDocument();
  });

  it("keeps the dialog open when clearing fails", async () => {
    const { onClose, user } = setup({
      get_job_expected_hash: () => ({ algorithm: "md5", digest: hex(32) }),
      clear_job_expected_hash: () => Promise.reject("job j1 not found"),
    });
    await user.click(await screen.findByRole("button", { name: "Clear expected hash" }));
    expect(await screen.findByText("Could not clear the expected hash")).toBeInTheDocument();
    expect(onClose).not.toHaveBeenCalled();
  });
});
