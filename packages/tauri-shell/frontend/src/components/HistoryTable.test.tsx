import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { HistoryTable } from "./HistoryTable";
import { ToastProvider } from "@/state/useToasts";
import { formatBytes } from "@/lib/progress";
import { installFakeIpc } from "@/test/mockApi";
import type { ArchiveEntry } from "@/tauri/types";

const entry = (id: string, overrides: Partial<ArchiveEntry> = {}): ArchiveEntry => ({
  media_id: id,
  title: `Clip ${id}`,
  url: `https://video.example/${id}`,
  completed_at_unix_secs: 1_000,
  destination: `/lib/jobs/${id}/${id}.mp4`,
  job_id: `job-${id}`,
  outcome: "Completed",
  size_bytes: 1024,
  ...overrides,
});

/** `count` entries, `m01` the oldest, `m{count}` the newest. */
function manyEntries(count: number): ArchiveEntry[] {
  return Array.from({ length: count }, (_, i) => {
    const n = String(i + 1).padStart(2, "0");
    return entry(`m${n}`, { completed_at_unix_secs: 1_000 + i });
  });
}

function renderHistory(entries: ArchiveEntry[], onRedownload = vi.fn()) {
  const ipc = installFakeIpc({
    list_history: () => entries,
    clear_history: () => entries.length,
    remove_history_entry: () => undefined,
  });
  render(
    <ToastProvider>
      <HistoryTable refreshKey={0} onRedownload={onRedownload} />
    </ToastProvider>,
  );
  return { ipc, onRedownload, user: userEvent.setup() };
}

/** Media ids of the rows currently shown, in on-screen order. */
function shownIds(): string[] {
  return screen
    .queryAllByTestId(/^history-row-/)
    .map((row) => row.dataset.testid!.replace("history-row-", ""));
}

async function chooseOption(user: ReturnType<typeof userEvent.setup>, dropdownName: string, optionName: string) {
  await user.click(screen.getByRole("combobox", { name: dropdownName }));
  await user.click(await screen.findByRole("option", { name: optionName }));
}

describe("HistoryTable search", () => {
  const entries = [
    entry("alpha", { title: "Mountain Walk", url: "https://video.example/a1", destination: "/lib/jobs/alpha/walk-final.mp4" }),
    entry("beta", { title: "Cooking Show", url: "https://video.example/b2", destination: "/lib/jobs/beta/recipe.webm" }),
    entry("gamma", { title: "Podcast", url: "https://audio.example/gamma-ep", destination: "/lib/jobs/gamma/episode.mp3" }),
  ];

  it("matches the title, URL and file name, ignoring case", async () => {
    const { user } = renderHistory(entries);
    const search = await screen.findByPlaceholderText("Search by title, URL or file name");

    await user.type(search, "MOUNTAIN");
    expect(shownIds()).toEqual(["alpha"]);

    await user.clear(search);
    await user.type(search, "audio.example");
    expect(shownIds()).toEqual(["gamma"]);

    await user.clear(search);
    await user.type(search, "recipe");
    expect(shownIds()).toEqual(["beta"]);
  });

  it("says so when nothing matches, and offers no Clear history while searching", async () => {
    const { user } = renderHistory(entries);
    const search = await screen.findByPlaceholderText("Search by title, URL or file name");

    await user.type(search, "nothing like this");
    expect(screen.getByTestId("history-empty")).toHaveTextContent("No matching history entries.");
    expect(screen.getByRole("button", { name: "Clear history" })).toBeDisabled();
    expect(screen.getByText("Clear the search to use Clear history.")).toBeInTheDocument();
  });

  it("re-enables Clear history once the search is cleared, and clears the shown outcome", async () => {
    const { user, ipc } = renderHistory(entries);
    const search = await screen.findByPlaceholderText("Search by title, URL or file name");

    await user.type(search, "podcast");
    await user.click(screen.getByRole("button", { name: "Clear search" }));
    // Same finish time, so the order falls back to media_id.
    expect(shownIds()).toEqual(["alpha", "beta", "gamma"]);

    await user.click(screen.getByRole("button", { name: "Clear history" }));
    const dialog = await screen.findByRole("dialog");
    await user.click(within(dialog).getByRole("button", { name: "Clear history" }));
    await waitFor(() => expect(ipc.callsTo("clear_history")).toEqual([{ outcome: null }]));
  });
});

describe("HistoryTable sort", () => {
  const entries = [
    entry("a", { title: "bravo", completed_at_unix_secs: 2_000, size_bytes: 500 }),
    entry("b", { title: "Alpha", completed_at_unix_secs: 1_000, size_bytes: 9_000 }),
    entry("c", { title: "charlie", completed_at_unix_secs: 3_000, size_bytes: null, outcome: "Failed", destination: "/lib/jobs/c" }),
    entry("d", { title: "Delta", completed_at_unix_secs: 4_000, size_bytes: 20 }),
  ];

  it("shows the newest finished first by default", async () => {
    renderHistory(entries);
    await screen.findByTestId("history-row-a");
    expect(shownIds()).toEqual(["d", "c", "a", "b"]);
  });

  it("sorts by name A to Z and Z to A", async () => {
    const { user } = renderHistory(entries);
    await screen.findByTestId("history-row-a");

    await chooseOption(user, "Sort", "Name, A to Z");
    expect(shownIds()).toEqual(["b", "a", "c", "d"]);

    await chooseOption(user, "Sort", "Name, Z to A");
    expect(shownIds()).toEqual(["d", "c", "a", "b"]);
  });

  it("sorts by size with unknown sizes last in both directions", async () => {
    const { user } = renderHistory(entries);
    await screen.findByTestId("history-row-a");

    await chooseOption(user, "Sort", "Size, largest first");
    expect(shownIds()).toEqual(["b", "a", "d", "c"]);

    await chooseOption(user, "Sort", "Size, smallest first");
    expect(shownIds()).toEqual(["d", "a", "b", "c"]);
  });

  it("sorts by finished time, oldest first", async () => {
    const { user } = renderHistory(entries);
    await screen.findByTestId("history-row-a");

    await chooseOption(user, "Sort", "Finished, oldest first");
    expect(shownIds()).toEqual(["b", "a", "c", "d"]);
  });

  it("shows the size of each entry, or a dash when unknown", async () => {
    renderHistory(entries);
    const row = await screen.findByTestId("history-row-b");
    expect(row.querySelector(".ph-history__col--size")).toHaveTextContent(formatBytes(9_000));
    expect(screen.getByTestId("history-row-c").querySelector(".ph-history__col--size")).toHaveTextContent(formatBytes(null));
  });
});

describe("HistoryTable pagination", () => {
  it("shows 25 entries per page and moves to the next page", async () => {
    const { user } = renderHistory(manyEntries(30));
    await screen.findByTestId("history-row-m30");
    expect(screen.getAllByTestId(/^history-row-/)).toHaveLength(25);
    expect(shownIds()[0]).toBe("m30");

    await user.click(screen.getByRole("button", { name: "Next page" }));
    expect(shownIds()).toEqual(["m05", "m04", "m03", "m02", "m01"]);
  });

  it("changes the page size to 50 and 100", async () => {
    const { user } = renderHistory(manyEntries(120));
    await screen.findByTestId("history-row-m120");

    await user.selectOptions(screen.getByLabelText("Items per page:"), "50");
    expect(screen.getAllByTestId(/^history-row-/)).toHaveLength(50);

    await user.selectOptions(screen.getByLabelText("Items per page:"), "100");
    expect(screen.getAllByTestId(/^history-row-/)).toHaveLength(100);
  });

  it("returns to page 1 when the search changes", async () => {
    const { user } = renderHistory(manyEntries(30));
    await screen.findByTestId("history-row-m30");
    await user.click(screen.getByRole("button", { name: "Next page" }));
    expect(shownIds()).toHaveLength(5);

    // "clip m2" matches m20 to m29: ten entries, all on the first page, newest first.
    await user.type(await screen.findByPlaceholderText("Search by title, URL or file name"), "clip m2");
    expect(shownIds()).toEqual(["m29", "m28", "m27", "m26", "m25", "m24", "m23", "m22", "m21", "m20"]);
    expect(screen.getByRole("button", { name: "Previous page" })).toBeDisabled();
  });

  it("keeps the chosen page size while searching", async () => {
    const { user } = renderHistory(manyEntries(30));
    await screen.findByTestId("history-row-m30");
    await user.selectOptions(screen.getByLabelText("Items per page:"), "50");
    await user.type(await screen.findByPlaceholderText("Search by title, URL or file name"), "m0");
    expect(shownIds()).toHaveLength(9);
  });
});

describe("HistoryTable Download again", () => {
  it("passes the entry URL to onRedownload from the row menu", async () => {
    const onRedownload = vi.fn();
    const { user, ipc } = renderHistory([entry("x1", { title: "Saved video", url: "https://video.example/saved" })], onRedownload);
    const row = await screen.findByTestId("history-row-x1");

    await user.click(within(row).getByRole("button", { name: /actions for saved video/i }));
    const menu = document.body.querySelector<HTMLElement>("[role=menu]")!;
    await user.click(within(menu).getByText("Download again"));

    expect(onRedownload).toHaveBeenCalledTimes(1);
    expect(onRedownload).toHaveBeenCalledWith("https://video.example/saved");
    // The table itself does not enqueue; the App routes the URL through the duplicate-aware add path.
    expect(ipc.callsTo("enqueue_url")).toHaveLength(0);
  });
});
