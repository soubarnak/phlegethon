import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { ProbeModal } from "./ProbeModal";
import { installFakeIpc, mediaItem, probeResult, quality } from "@/test/mockApi";
import type { ProbeResult } from "@/tauri/types";

function props(over: Partial<React.ComponentProps<typeof ProbeModal>> = {}) {
  return {
    url: "https://www.youtube.com/playlist?list=PL1",
    initialDestination: null,
    onClose: vi.fn(),
    onConfirmed: vi.fn(),
    onTryPlain: vi.fn(),
    onOpenSettings: vi.fn(),
    onError: vi.fn(),
    ...over,
  };
}

function baseIpc(result: ProbeResult | Error) {
  return installFakeIpc({
    get_default_media_extras: () => ({ write_subs: false, embed_subs: false, write_chapters: false, embed_chapters: false, write_thumbnail: false, embed_thumbnail: false, write_metadata: false, embed_metadata: false, remux: false }),
    get_media_extras_capabilities: () => ({ ffmpeg_available: true, embed_disabled_reason: null }),
    probe_url: () => {
      if (result instanceof Error) throw result.message;
      return result;
    },
    select_default_quality: (a) => {
      const qs = a.qualities as Array<{ id: string }>;
      return qs.find((q) => q.id === "720p")?.id ?? qs[0]?.id ?? null;
    },
    preview_probe_destination: () => ({ path_display: "/lib/jobs/Playlist-…" }),
    confirm_probe_selection: () => ["job-1", "job-2"],
    cancel_probe: () => true,
    clear_pending_probe: () => undefined,
    enrich_probe_item_qualities: () => [quality("1080p", "1080p", 900), quality("720p", "720p", 500)],
  });
}

const items = [
  mediaItem({ id: "yt-a", title: "First video", qualities: [quality("1080p", "1080p", 900), quality("720p", "720p", 500)] }),
  mediaItem({ id: "yt-b", title: "Second video", archived: true, qualities: [quality("720p", "720p", 400)] }),
  mediaItem({ id: "yt-c", title: "Third video", qualities: [quality("best", "best")] }),
];

describe("ProbeModal", () => {
  it("lists items, selects all but archived by default, and confirms the chosen qualities", async () => {
    const ipc = baseIpc(probeResult(items));
    const p = props();
    render(<ProbeModal {...p} />);
    expect(screen.getByTestId("probe-loading")).toBeInTheDocument();
    await screen.findByTestId("probe-row-yt-a");

    expect(within(screen.getByTestId("probe-row-yt-b")).getByTestId("archived-tag")).toBeInTheDocument();
    expect(screen.getByRole("checkbox", { name: /select first video/i })).toBeChecked();
    expect(screen.getByRole("checkbox", { name: /select second video/i })).not.toBeChecked();
    expect(screen.getByRole("checkbox", { name: /select third video/i })).toBeChecked();

    const confirm = await screen.findByTestId("probe-confirm");
    expect(confirm).toHaveTextContent("Download 2 selected");
    await userEvent.setup().click(confirm);

    await waitFor(() => expect(p.onConfirmed).toHaveBeenCalledWith(["job-1", "job-2"]));
    const [call] = ipc.callsTo("confirm_probe_selection");
    expect(call.selection).toMatchObject({
      url: "https://www.youtube.com/playlist?list=PL1",
      items: [
        { item_id: "yt-a", quality_id: "720p" }, // Default Quality Policy pick
        { item_id: "yt-c", quality_id: "best" },
      ],
    });
    expect(call.destination).toBeNull();
  });

  it("re-selecting an archived item includes it (explicit re-download)", async () => {
    const ipc = baseIpc(probeResult(items));
    render(<ProbeModal {...props()} />);
    const user = userEvent.setup();
    await screen.findByTestId("probe-row-yt-b");
    await user.click(screen.getByRole("checkbox", { name: /select second video/i }));
    await user.click(await screen.findByTestId("probe-confirm"));
    await waitFor(() => expect(ipc.callsTo("confirm_probe_selection")).toHaveLength(1));
    const ids = (ipc.callsTo("confirm_probe_selection")[0].selection as { items: Array<{ item_id: string }> }).items.map((i) => i.item_id);
    expect(ids).toEqual(["yt-a", "yt-b", "yt-c"]);
  });

  it("disables download when nothing is selected", async () => {
    baseIpc(probeResult(items));
    render(<ProbeModal {...props()} />);
    const user = userEvent.setup();
    await screen.findByTestId("probe-row-yt-a");
    await user.click(screen.getByRole("button", { name: "Deselect all" }));
    expect(screen.getByTestId("probe-confirm")).toBeDisabled();
  });

  it("warns when the result came from a fallback tier", async () => {
    baseIpc(probeResult(items.slice(0, 1), { tier: "generic_extractor", extractor_key: "Generic" }));
    render(<ProbeModal {...props()} />);
    const notice = await screen.findByTestId("probe-tier-notice");
    expect(notice).toHaveTextContent("Generic extraction");
  });

  it("lazily loads exact formats for preset qualities on first interaction", async () => {
    const preset = mediaItem({ id: "yt-p", title: "Preset video", qualities: [quality("720p", "720p")], qualities_are_presets: true });
    const ipc = baseIpc(probeResult([preset]));
    render(<ProbeModal {...props()} />);
    const user = userEvent.setup();
    const row = await screen.findByTestId("probe-row-yt-p");
    await user.click(within(row).getByRole("combobox"));
    await waitFor(() => expect(ipc.callsTo("enrich_probe_item_qualities")).toHaveLength(1));
    expect(ipc.callsTo("enrich_probe_item_qualities")[0]).toEqual({ probeUrl: "https://www.youtube.com/playlist?list=PL1", itemId: "yt-p" });
  });

  it("shows the exact error with a plain-download escape hatch and a cookie hint", async () => {
    baseIpc(new Error("[needs_browser_cookies] yt-dlp probe failed: HTTP Error 403"));
    const p = props({ url: "https://en.luxuretv.com/videos/x-1.html" });
    render(<ProbeModal {...p} />);
    const error = await screen.findByTestId("probe-error");
    expect(error).toHaveTextContent("HTTP Error 403");
    expect(error).toHaveTextContent(/Cloudflare/i);
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "Try plain download" }));
    expect(p.onTryPlain).toHaveBeenCalledWith("https://en.luxuretv.com/videos/x-1.html");
    await user.click(screen.getByRole("button", { name: "Open cookie settings" }));
    expect(p.onOpenSettings).toHaveBeenCalled();
  });

  it("cancelling while probing aborts the probe and clears the pending result", async () => {
    const ipc = installFakeIpc({
      get_default_media_extras: () => ({}),
      get_media_extras_capabilities: () => ({ ffmpeg_available: true, embed_disabled_reason: null }),
      probe_url: () => new Promise(() => undefined),
      cancel_probe: () => true,
      clear_pending_probe: () => undefined,
    });
    const p = props();
    render(<ProbeModal {...p} />);
    await screen.findByTestId("probe-loading");
    await userEvent.setup().click(screen.getByRole("button", { name: "Cancel" }));
    expect(p.onClose).toHaveBeenCalled();
    await waitFor(() => expect(ipc.callsTo("cancel_probe")).toHaveLength(1));
    expect(ipc.callsTo("clear_pending_probe")).toHaveLength(1);
  });
});
