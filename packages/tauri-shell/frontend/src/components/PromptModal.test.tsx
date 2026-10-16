import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { PromptModal } from "./PromptModal";
import type { PromptRequest } from "@/App";

function request(over: Partial<PromptRequest> = {}): PromptRequest {
  return {
    title: "Add mirror",
    label: "Mirror URL",
    confirmLabel: "Add",
    validate: (v) => (/^https?:\/\//.test(v) ? null : "Not a URL"),
    onSubmit: vi.fn(),
    ...over,
  };
}

describe("PromptModal", () => {
  it("opens with focus in the field (UI-9)", async () => {
    render(<PromptModal request={request()} onClose={vi.fn()} />);
    expect(await screen.findByLabelText("Mirror URL")).toHaveFocus();
  });

  it("Enter in the field submits a valid value and closes (UI-9)", async () => {
    const req = request();
    const onClose = vi.fn();
    render(<PromptModal request={req} onClose={onClose} />);
    const user = userEvent.setup();
    await user.type(await screen.findByLabelText("Mirror URL"), "https://m.example.com/f{Enter}");
    expect(req.onSubmit).toHaveBeenCalledWith("https://m.example.com/f");
    expect(onClose).toHaveBeenCalled();
  });

  it("Enter does nothing while the value is invalid", async () => {
    const req = request();
    const onClose = vi.fn();
    render(<PromptModal request={req} onClose={onClose} />);
    await userEvent.setup().type(await screen.findByLabelText("Mirror URL"), "nope{Enter}");
    expect(req.onSubmit).not.toHaveBeenCalled();
    expect(onClose).not.toHaveBeenCalled();
  });

  it("Enter on the Cancel button does not submit", async () => {
    const req = request();
    render(<PromptModal request={req} onClose={vi.fn()} />);
    const user = userEvent.setup();
    await user.type(await screen.findByLabelText("Mirror URL"), "https://m.example.com/f");
    screen.getByRole("button", { name: "Cancel" }).focus();
    await user.keyboard("{Enter}");
    expect(req.onSubmit).not.toHaveBeenCalled();
  });
});
