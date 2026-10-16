import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { describe, expect, it } from "vitest";
import { DiscardModal } from "@/components/DiscardModal";
import { FOCUS_FALLBACK_ID, useReturnFocus } from "./useReturnFocus";

function Dialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  useReturnFocus(open);
  return open ? (
    <div role="dialog">
      <button onClick={onClose}>Close dialog</button>
    </div>
  ) : null;
}

function Harness({ withUrlField = false }: { withUrlField?: boolean }) {
  const [open, setOpen] = useState(false);
  const [showOpener, setShowOpener] = useState(true);
  return (
    <>
      {withUrlField && <textarea id={FOCUS_FALLBACK_ID} aria-label="URLs" />}
      <button>Other</button>
      {showOpener && <button onClick={() => setOpen(true)}>Open dialog</button>}
      <button onClick={() => setShowOpener(false)}>Remove opener</button>
      <Dialog open={open} onClose={() => setOpen(false)} />
    </>
  );
}

describe("useReturnFocus (UI-10)", () => {
  it("returns focus to the element that opened the dialog", async () => {
    render(<Harness />);
    const user = userEvent.setup();
    const opener = screen.getByRole("button", { name: "Open dialog" });
    await user.click(opener);
    screen.getByRole("button", { name: "Close dialog" }).focus();
    await user.click(screen.getByRole("button", { name: "Close dialog" }));
    expect(opener).toHaveFocus();
  });

  it("falls back to the URL field when the opener has left the page", async () => {
    render(<Harness withUrlField />);
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "Open dialog" }));
    // The opener unmounts while the dialog is open (a row menu item, for example).
    await user.click(screen.getByRole("button", { name: "Remove opener" }));
    await user.click(screen.getByRole("button", { name: "Close dialog" }));
    expect(screen.getByLabelText("URLs")).toHaveFocus();
  });

  it("restores focus after a Discard dialog is cancelled", async () => {
    function Page() {
      const [open, setOpen] = useState(false);
      return (
        <>
          <button onClick={() => setOpen(true)}>Discard selected</button>
          <DiscardModal
            copy={open ? { title: "Discard?", body: "Body", confirmLabel: "Discard", cancelLabel: "Keep", deletesFiles: false } : null}
            onConfirm={() => setOpen(false)}
            onCancel={() => setOpen(false)}
          />
        </>
      );
    }
    render(<Page />);
    const user = userEvent.setup();
    const trigger = screen.getByRole("button", { name: "Discard selected" });
    await user.click(trigger);
    await user.click(await screen.findByRole("button", { name: "Keep" }));
    expect(trigger).toHaveFocus();
  });
});
