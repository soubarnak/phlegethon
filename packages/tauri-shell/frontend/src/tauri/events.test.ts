import { describe, expect, it, vi } from "vitest";
import { installFakeIpc } from "@/test/mockApi";
import { onCompanionIntent, onCoreEvent } from "./events";

describe("events", () => {
  it("delivers the payload of core-event, and stops after unlisten", async () => {
    const ipc = installFakeIpc();
    const handler = vi.fn();
    const off = await onCoreEvent(handler);
    const event = { type: "JobPaused", job_id: "j1" };
    await ipc.emit("core-event", event);
    expect(handler).toHaveBeenCalledWith(event);

    off();
    await ipc.emit("core-event", { type: "JobResumed", job_id: "j1" });
    expect(handler).toHaveBeenCalledTimes(1);
  });

  it("delivers companion-intent separately from core-event", async () => {
    const ipc = installFakeIpc();
    const core = vi.fn();
    const companion = vi.fn();
    await onCoreEvent(core);
    await onCompanionIntent(companion);
    const intent = { url: "https://a.example", result: { type: "probe_then_confirm", url: "https://a.example" } };
    await ipc.emit("companion-intent", intent);
    expect(companion).toHaveBeenCalledWith(intent);
    expect(core).not.toHaveBeenCalled();
  });
});
