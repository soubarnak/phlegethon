import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { UrlBar } from "./UrlBar";
import { installFakeIpc } from "@/test/mockApi";

function setup(over: Partial<React.ComponentProps<typeof UrlBar>> = {}) {
  const props = {
    onSubmit: vi.fn(async () => undefined),
    destination: null as string | null,
    onDestinationChange: vi.fn(),
    clipboardWatch: false,
    onClipboardWatchChange: vi.fn(),
    ...over,
  };
  installFakeIpc({ pick_folder: () => ({ path: "/home/u/Videos", method: "portal" }), split_urls_cmd: (a) => String(a.text).split(/\s+/).filter(Boolean) });
  render(<UrlBar {...props} />);
  return props;
}

describe("UrlBar", () => {
  it("Add is disabled until there is text, then submits and clears", async () => {
    const p = setup();
    const user = userEvent.setup();
    const add = screen.getByRole("button", { name: "Add" });
    expect(add).toBeDisabled();
    await user.type(screen.getByLabelText("URLs"), "https://example.com/a.zip");
    expect(add).toBeEnabled();
    await user.click(add);
    expect(p.onSubmit).toHaveBeenCalledWith("https://example.com/a.zip");
    await waitFor(() => expect(screen.getByLabelText("URLs")).toHaveValue(""));
  });

  it("Enter submits, Shift+Enter inserts a newline", async () => {
    const p = setup();
    const user = userEvent.setup();
    const box = screen.getByLabelText("URLs");
    await user.type(box, "https://a.example{Shift>}{Enter}{/Shift}https://b.example");
    expect(p.onSubmit).not.toHaveBeenCalled();
    expect(box).toHaveValue("https://a.example\nhttps://b.example");
    await user.type(box, "{Enter}");
    expect(p.onSubmit).toHaveBeenCalledWith("https://a.example\nhttps://b.example");
  });

  it("Save to… picks a folder; the tag clears it", async () => {
    const p = setup();
    await userEvent.setup().click(screen.getByRole("button", { name: "Save to…" }));
    await waitFor(() => expect(p.onDestinationChange).toHaveBeenCalledWith("/home/u/Videos"));
  });

  it("shows the chosen destination and lets the user clear it", async () => {
    const p = setup({ destination: "/home/u/Videos" });
    expect(screen.getByText(/Saving to \/home\/u\/Videos/)).toBeInTheDocument();
    await userEvent.setup().click(screen.getByRole("button", { name: "Clear Save to folder" }));
    expect(p.onDestinationChange).toHaveBeenCalledWith(null);
  });

  it("dropping links onto the bar submits them", () => {
    const p = setup();
    const store: Record<string, string> = { "text/plain": "https://dropped.example/video" };
    fireEvent.drop(screen.getByLabelText("Add downloads"), {
      dataTransfer: { types: Object.keys(store), getData: (k: string) => store[k] ?? "" },
    });
    expect(p.onSubmit).toHaveBeenCalledWith("https://dropped.example/video");
  });

  it("pasting outside an input adds the clipboard text", () => {
    const p = setup();
    const event = new Event("paste", { bubbles: true, cancelable: true }) as unknown as ClipboardEvent;
    Object.defineProperty(event, "clipboardData", { value: { getData: () => "https://pasted.example/v" } });
    document.body.dispatchEvent(event);
    expect(p.onSubmit).toHaveBeenCalledWith("https://pasted.example/v");
  });

  it("clipboard toggle reports changes", async () => {
    const p = setup();
    await userEvent.setup().click(screen.getByRole("switch", { name: /watch clipboard/i }));
    expect(p.onClipboardWatchChange).toHaveBeenCalledWith(true);
  });
});
