import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { ToolchainPanel } from "./ToolchainPanel";
import { ToastProvider } from "@/state/useToasts";
import { installFakeIpc, toolchainStatus, type CommandHandler } from "@/test/mockApi";
import type { SidecarApplyResult, SidecarUpdateCheckResult } from "@/tauri/types";

const withUpdates: SidecarUpdateCheckResult = {
  toolchain: { component: "toolchain", installed_version: "2026.08.19", available_version: "2026.10.01", pinned_version: null, update_available: true },
  plugin: { component: "plugin", installed_version: "0.1.0", available_version: "0.1.1", pinned_version: null, update_available: true },
};

const upToDate: SidecarUpdateCheckResult = {
  toolchain: { component: "toolchain", installed_version: "2026.10.01", available_version: "2026.10.01", pinned_version: null, update_available: false },
  plugin: { component: "plugin", installed_version: "0.1.1", available_version: "0.1.1", pinned_version: null, update_available: false },
};

function setup(overrides: Record<string, CommandHandler> = {}) {
  const ipc = installFakeIpc({
    get_toolchain_status: () => toolchainStatus(),
    get_sidecar_update_settings: () => ({ check_on_launch: true, channel: "stable", toolchain_pin: null, plugin_pin: null }),
    check_sidecar_updates: () => withUpdates,
    apply_sidecar_updates: () => ({ outcome: "applied", applied: ["toolchain", "plugin"] }) satisfies SidecarApplyResult,
    ...overrides,
  });
  render(
    <ToastProvider>
      <ToolchainPanel />
    </ToastProvider>,
  );
  return ipc;
}

async function runCheck(user: ReturnType<typeof userEvent.setup>) {
  await user.click(await screen.findByRole("button", { name: "Check for updates" }));
  await screen.findByTestId("toolchain-update-result");
}

describe("ToolchainPanel sidecar update apply", () => {
  it("offers Apply only after a check reports an update", async () => {
    const user = userEvent.setup();
    setup();
    expect(screen.queryByRole("button", { name: "Apply updates" })).not.toBeInTheDocument();
    await runCheck(user);
    expect(screen.getByRole("button", { name: "Apply updates" })).toBeEnabled();
  });

  it("hides Apply when the check finds nothing newer", async () => {
    const user = userEvent.setup();
    setup({ check_sidecar_updates: () => upToDate });
    await runCheck(user);
    expect(screen.queryByRole("button", { name: "Apply updates" })).not.toBeInTheDocument();
  });

  it("applies, reports success, and re-checks so the update list clears", async () => {
    const user = userEvent.setup();
    let checks = 0;
    const ipc = setup({ check_sidecar_updates: () => (++checks === 1 ? withUpdates : upToDate) });
    await runCheck(user);
    await user.click(screen.getByRole("button", { name: "Apply updates" }));

    expect(await screen.findByText("Updates applied")).toBeInTheDocument();
    expect(ipc.callsTo("apply_sidecar_updates")).toHaveLength(1);
    await waitFor(() => expect(ipc.callsTo("check_sidecar_updates")).toHaveLength(2));
    await waitFor(() => expect(screen.queryByRole("button", { name: "Apply updates" })).not.toBeInTheDocument());
  });

  it("disables Apply and Check while the apply is running", async () => {
    const user = userEvent.setup();
    let finish!: (result: SidecarApplyResult) => void;
    setup({ apply_sidecar_updates: () => new Promise<SidecarApplyResult>((resolve) => (finish = resolve)) });
    await runCheck(user);
    await user.click(screen.getByRole("button", { name: "Apply updates" }));

    const busy = (await screen.findByText("Applying…")).closest("button");
    expect(busy).toBeDisabled();
    expect(screen.getByRole("button", { name: "Check for updates" })).toBeDisabled();

    finish({ outcome: "nothing_to_apply", applied: [] });
    await waitFor(() => expect(screen.getByRole("button", { name: "Apply updates" })).toBeEnabled());
  });

  it.each([
    ["deferred_until_idle", "The update will be applied when the queue is idle."],
    ["blocked_mid_transfer", "Downloads are running; try again when the queue is idle."],
  ] as const)("shows a notice for the %s outcome without re-checking", async (outcome, message) => {
    const user = userEvent.setup();
    const ipc = setup({ apply_sidecar_updates: () => ({ outcome, applied: [] }) });
    await runCheck(user);
    await user.click(screen.getByRole("button", { name: "Apply updates" }));

    expect(await screen.findByText(message)).toBeInTheDocument();
    expect(ipc.callsTo("check_sidecar_updates")).toHaveLength(1);
  });

  it("reports a failed apply through the error toast", async () => {
    const user = userEvent.setup();
    setup({ apply_sidecar_updates: () => Promise.reject("plugin checksum mismatch") });
    await runCheck(user);
    await user.click(screen.getByRole("button", { name: "Apply updates" }));

    expect(await screen.findByText("Could not apply the updates")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Apply updates" })).toBeEnabled();
  });
});
