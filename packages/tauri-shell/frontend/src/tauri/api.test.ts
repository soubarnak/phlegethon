import { readFileSync } from "node:fs";
import path from "node:path";
import { describe, expect, it } from "vitest";
import { installFakeIpc } from "@/test/mockApi";
import { api } from "./api";

describe("api", () => {
  it("every command it invokes is registered in the Tauri shell", () => {
    const shell = readFileSync(path.resolve(__dirname, "../../../src-tauri/src/lib.rs"), "utf8");
    const handler = /generate_handler!\[([\s\S]*?)\]/.exec(shell)?.[1] ?? "";
    const registered = new Set([...handler.matchAll(/\b([a-z_0-9]+)\b/g)].map((m) => m[1]));
    expect(registered.size).toBeGreaterThan(30);

    const source = readFileSync(path.resolve(__dirname, "api.ts"), "utf8");
    const used = [...source.matchAll(/invoke<[^>]*(?:<[^>]*>)?[^>]*>\(\s*"([a-z_0-9]+)"/g)].map((m) => m[1]);
    expect(used.length).toBeGreaterThan(30);
    expect(used.filter((cmd) => !registered.has(cmd))).toEqual([]);
  });

  it("sends the argument names the Rust commands take", async () => {
    const ipc = installFakeIpc({
      enqueue_url: () => ({ type: "EnqueueDirect", job_id: "j", destination: "/d" }),
      discard_job: () => undefined,
      reorder_job: () => undefined,
      split_urls_cmd: () => [],
      find_active_job_for_url: () => null,
      set_job_bandwidth_cap: () => undefined,
      pick_folder: () => ({ path: null, method: "portal" }),
      clear_history: () => 0,
    });
    await api.enqueueUrl("https://a.example/x", "/dest", true);
    await api.discardJob("j1");
    await api.reorderJob(2, 0);
    await api.splitUrls("a b");
    await api.findActiveJobForUrl("https://a.example/x");
    await api.setJobBandwidthCap("j1", null);
    await api.pickFolder();
    await api.clearHistory(null);

    expect(ipc.callsTo("enqueue_url")).toEqual([{ url: "https://a.example/x", destination: "/dest", forceDirect: true }]);
    expect(ipc.callsTo("discard_job")).toEqual([{ jobId: "j1" }]);
    expect(ipc.callsTo("reorder_job")).toEqual([{ fromIndex: 2, toIndex: 0 }]);
    expect(ipc.callsTo("split_urls_cmd")).toEqual([{ text: "a b" }]);
    expect(ipc.callsTo("find_active_job_for_url")).toEqual([{ url: "https://a.example/x" }]);
    expect(ipc.callsTo("set_job_bandwidth_cap")).toEqual([{ jobId: "j1", cap: null }]);
    expect(ipc.callsTo("pick_folder")).toEqual([{ title: null }]);
    expect(ipc.callsTo("clear_history")).toEqual([{ outcome: null }]);
  });

  it("rejects with the string the shell returned", async () => {
    installFakeIpc({ list_jobs: () => Promise.reject("core unavailable") });
    await expect(api.listJobs()).rejects.toBe("core unavailable");
  });
});
