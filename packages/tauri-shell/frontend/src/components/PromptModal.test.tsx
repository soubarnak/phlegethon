import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { PromptRequest } from "@/App";
import { PromptModal } from "./PromptModal";

function request(over: Partial<PromptRequest> = {}): PromptRequest {
  return {
    title: "Add mirror",
    label: "Mirror URL",
    confirmLabel: "Add",
    validate: (v) => (/^https?:\/\//.test(v) ? null : "Enter an http(s) URL"),
    onSubmit: vi.fn(),
    ...over,
  };
}

describe("PromptModal", () => {
  it("renders nothing without a request", () => {
    const { container } = render(<PromptModal request={null} onClose={vi.fn()} />);
    expect(container).toBeEmptyDOMElement();
  });

  it("keeps the confirm button disabled and shows the validation message once typing starts", async () => {
    render(<PromptModal request={request()} onClose={vi.fn()} />);
    const user = userEvent.setup();
    expect(screen.getByRole("button", { name: "Add" })).toBeDisabled();
    await user.type(screen.getByLabelText("Mirror URL"), "ftp://x");
    expect(screen.getByText("Enter an http(s) URL")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Add" })).toBeDisabled();
  });

  it("submits the typed value once and closes", async () => {
    const onSubmit = vi.fn();
    const onClose = vi.fn();
    render(<PromptModal request={request({ onSubmit })} onClose={onClose} />);
    const user = userEvent.setup();
    await user.type(screen.getByLabelText("Mirror URL"), "https://mirror.example/f.iso");
    await user.click(screen.getByRole("button", { name: "Add" }));
    expect(onSubmit).toHaveBeenCalledTimes(1);
    expect(onSubmit).toHaveBeenCalledWith("https://mirror.example/f.iso");
    expect(onClose).toHaveBeenCalled();
  });

  it("Cancel closes without submitting", async () => {
    const onSubmit = vi.fn();
    const onClose = vi.fn();
    render(<PromptModal request={request({ onSubmit })} onClose={onClose} />);
    await userEvent.setup().click(screen.getByRole("button", { name: "Cancel" }));
    expect(onClose).toHaveBeenCalled();
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it("clears the field when a new request opens", async () => {
    const { rerender } = render(<PromptModal request={request()} onClose={vi.fn()} />);
    await userEvent.setup().type(screen.getByLabelText("Mirror URL"), "https://a.example");
    rerender(<PromptModal request={request({ label: "Cap (KB/s)" })} onClose={vi.fn()} />);
    expect(screen.getByLabelText("Cap (KB/s)")).toHaveValue("");
  });
});
