import { describe, expect, it, vi } from "vitest";
import type { ActiveJobMatch } from "@/tauri/types";
import { processUrlInput, resolveDuplicateUrl, textFromDataTransfer } from "./urlInput";

describe("resolveDuplicateUrl", () => {
  const match: ActiveJobMatch = { job_id: "j", url: "https://x", status: "pending", title: "X" };
  it("enqueues without asking when there is no duplicate", async () => {
    const confirmDuplicate = vi.fn();
    const r = await resolveDuplicateUrl(
      { findActiveJobForUrl: async () => null, confirmDuplicate, focusExisting: vi.fn() },
      "https://x",
    );
    expect(r).toBe("enqueue");
    expect(confirmDuplicate).not.toHaveBeenCalled();
  });
  it("focuses the existing job when asked", async () => {
    const focusExisting = vi.fn();
    const r = await resolveDuplicateUrl(
      { findActiveJobForUrl: async () => match, confirmDuplicate: async () => "focus", focusExisting },
      "https://x",
    );
    expect(r).toBe("focus");
    expect(focusExisting).toHaveBeenCalledWith(match);
  });
});

describe("processUrlInput", () => {
  it("enqueues each URL unless the duplicate prompt says otherwise", async () => {
    const enqueueOne = vi.fn(async (_url: string) => undefined);
    const result = await processUrlInput(
      {
        splitUrls: async () => ["https://a", "https://b", "https://c"],
        resolveDuplicate: async (u) => (u === "https://b" ? "cancel" : "enqueue"),
        enqueueOne,
      },
      "ignored",
    );
    expect(result).toEqual({ enqueued: 2, total: 3, skipped: ["https://b"], failed: [] });
    expect(enqueueOne.mock.calls.map((c) => c[0])).toEqual(["https://a", "https://c"]);
  });
  it("keeps going when one URL fails and reports which ones did (M-67)", async () => {
    const enqueueOne = vi.fn(async (url: string) => {
      if (url === "https://bad") throw new Error("no library root");
    });
    const result = await processUrlInput(
      {
        splitUrls: async () => ["https://a", "https://bad", "https://c"],
        resolveDuplicate: async () => "enqueue",
        enqueueOne,
      },
      "ignored",
    );
    expect(result.enqueued).toBe(2);
    expect(result.failed).toEqual([{ url: "https://bad", error: new Error("no library root") }]);
    expect(enqueueOne.mock.calls.map((c) => c[0])).toEqual(["https://a", "https://bad", "https://c"]);
  });
  it("counts a failing duplicate check as that URL failing, not the batch", async () => {
    const result = await processUrlInput(
      {
        splitUrls: async () => ["https://a", "https://b"],
        resolveDuplicate: async (u) => {
          if (u === "https://a") throw "lookup failed";
          return "enqueue";
        },
        enqueueOne: async () => undefined,
      },
      "ignored",
    );
    expect(result).toMatchObject({ enqueued: 1, total: 2 });
    expect(result.failed.map((f) => f.url)).toEqual(["https://a"]);
  });
  it("reports nothing for text without links", async () => {
    const result = await processUrlInput(
      { splitUrls: async () => [], resolveDuplicate: vi.fn(), enqueueOne: vi.fn() },
      "hello",
    );
    expect(result.total).toBe(0);
  });
});

describe("textFromDataTransfer", () => {
  it("merges uri-list and plain text without duplicates or comments", () => {
    const store: Record<string, string> = {
      "text/uri-list": "# comment\nhttps://a.example/x\nhttps://b.example/y",
      "text/plain": "https://a.example/x",
    };
    const dt = { types: Object.keys(store), getData: (k: string) => store[k] ?? "" };
    expect(textFromDataTransfer(dt as never)).toBe("https://a.example/x\nhttps://b.example/y");
    expect(textFromDataTransfer(null)).toBe("");
  });
});
