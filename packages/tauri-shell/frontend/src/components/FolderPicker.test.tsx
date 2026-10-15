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
});
