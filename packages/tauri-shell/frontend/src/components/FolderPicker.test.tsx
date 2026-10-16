import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { FolderPicker } from "./FolderPicker";
import { installFakeIpc } from "@/test/mockApi";

function setup(pick: () => unknown) {
  const onPicked = vi.fn();
  installFakeIpc({ pick_folder: pick });
  render(<FolderPicker onPicked={onPicked} />);
  return { onPicked, user: userEvent.setup() };
}

describe("FolderPicker", () => {
  it.each(["portal", "zenity", "kdialog"] as const)("returns the path chosen through %s", async (method) => {
    const { onPicked, user } = setup(() => ({ path: "/srv/media", method }));
    await user.click(screen.getByRole("button", { name: "Save to…" }));
    await waitFor(() => expect(onPicked).toHaveBeenCalledWith("/srv/media"));
  });

  it("does nothing when the dialog is cancelled", async () => {
    const { onPicked, user } = setup(() => ({ path: null, method: "portal" }));
    await user.click(screen.getByRole("button", { name: "Save to…" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "Save to…" })).toBeEnabled());
    expect(onPicked).not.toHaveBeenCalled();
    expect(screen.queryByLabelText("Absolute folder path")).toBeNull();
  });

  it("asks for a typed path when no chooser exists", async () => {
    const { onPicked, user } = setup(() => ({ path: null, method: "manual" }));
    await user.click(screen.getByRole("button", { name: "Save to…" }));
    expect(await screen.findByText(/No folder chooser was found/)).toBeInTheDocument();
    const use = screen.getByRole("button", { name: "Use this folder" });
    expect(use).toBeDisabled();
    await user.type(screen.getByLabelText("Absolute folder path"), "relative/dir");
    expect(use).toBeDisabled();
    await user.clear(screen.getByLabelText("Absolute folder path"));
    await user.type(screen.getByLabelText("Absolute folder path"), "/home/u/Videos");
    await user.click(use);
    expect(onPicked).toHaveBeenCalledWith("/home/u/Videos");
  });

  it("falls back to the typed path when the chooser command fails", async () => {
    const { user } = setup(() => {
      throw new Error("boom");
    });
    await user.click(screen.getByRole("button", { name: "Save to…" }));
    expect(await screen.findByLabelText("Absolute folder path")).toBeInTheDocument();
  });

  it("the Type a path button opens the manual dialog directly", async () => {
    const { user } = setup(() => ({ path: "/x", method: "portal" }));
    await user.click(screen.getByTestId("folder-picker-manual"));
    expect(await screen.findByLabelText("Absolute folder path")).toBeInTheDocument();
  });

  it("Cancel frees the button and ignores a chooser reply that arrives later (L-61)", async () => {
    let reply: (value: unknown) => void = () => undefined;
    const onPicked = vi.fn();
    installFakeIpc({ pick_folder: () => new Promise((resolve) => (reply = resolve)) });
    render(<FolderPicker onPicked={onPicked} />);
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "Save to…" }));
    expect(screen.getByRole("button", { name: "Save to…" })).toBeDisabled();
    await user.click(await screen.findByTestId("folder-picker-cancel"));
    expect(screen.getByRole("button", { name: "Save to…" })).toBeEnabled();
    reply({ path: "/late", method: "portal" });
    await new Promise((resolve) => setTimeout(resolve, 10));
    expect(onPicked).not.toHaveBeenCalled();
  });

  it("falls back to a typed path when the chooser never answers (L-61)", async () => {
    const onError = vi.fn();
    installFakeIpc({ pick_folder: () => new Promise(() => undefined) });
    render(<FolderPicker onPicked={vi.fn()} onError={onError} timeoutMs={30} />);
    await userEvent.setup().click(screen.getByRole("button", { name: "Save to…" }));
    expect(await screen.findByLabelText("Absolute folder path")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Save to…" })).toBeEnabled();
    expect(onError).toHaveBeenCalled();
  });

  it("renders the typed-path dialog on <body>, outside any transformed ancestor (UI-8)", async () => {
    installFakeIpc({ pick_folder: () => ({ path: "/x", method: "portal" }) });
    render(
      <div className="ph-probe" data-testid="sheet" style={{ transform: "translate(0, 0)" }}>
        <FolderPicker onPicked={vi.fn()} />
      </div>,
    );
    await userEvent.setup().click(screen.getByTestId("folder-picker-manual"));
    const dialog = await screen.findByRole("dialog");
    expect(screen.getByTestId("sheet")).not.toContainElement(dialog);
    expect(document.body).toContainElement(dialog);
  });

  it("focuses the field, accepts Enter and gives focus back to the button (UI-9, UI-10)", async () => {
    const { onPicked, user } = setup(() => ({ path: "/x", method: "portal" }));
    const opener = screen.getByTestId("folder-picker-manual");
    await user.click(opener);
    const field = await screen.findByLabelText("Absolute folder path");
    expect(field).toHaveFocus();
    await user.type(field, "/mnt/media{Enter}");
    expect(onPicked).toHaveBeenCalledWith("/mnt/media");
    await waitFor(() => expect(screen.queryByLabelText("Absolute folder path")).toBeNull());
    expect(opener).toHaveFocus();
  });

  it("Enter with a relative path does not submit", async () => {
    const { onPicked, user } = setup(() => ({ path: "/x", method: "portal" }));
    await user.click(screen.getByTestId("folder-picker-manual"));
    await user.type(await screen.findByLabelText("Absolute folder path"), "relative{Enter}");
    expect(onPicked).not.toHaveBeenCalled();
    expect(screen.getByLabelText("Absolute folder path")).toBeInTheDocument();
  });
});
