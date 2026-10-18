import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { ActiveJobMatch } from "@/tauri/types";
import { DuplicateUrlModal, matchStatusLabel } from "./DuplicateUrlModal";

const match: ActiveJobMatch = { job_id: "j1", url: "https://example.com/file.zip", status: "pending", title: "file.zip" };

describe("DuplicateUrlModal", () => {
  it("renders nothing without a match", () => {
    const { container } = render(<DuplicateUrlModal match={null} onResolve={vi.fn()} />);
    expect(container).toBeEmptyDOMElement();
  });

  it("shows the existing job with a translated status", () => {
    render(<DuplicateUrlModal match={match} onResolve={vi.fn()} />);
    expect(screen.getByText("file.zip")).toBeInTheDocument();
    expect(screen.getByText("https://example.com/file.zip")).toBeInTheDocument();
    expect(screen.getByText("Queued")).toBeInTheDocument();
    expect(screen.queryByText("pending")).not.toBeInTheDocument();
  });

  it.each([
    ["Cancel", "cancel"],
    ["Show existing", "focus"],
    ["Add anyway", "enqueue"],
  ])("the %s button resolves %s", async (name, resolution) => {
    const onResolve = vi.fn();
    render(<DuplicateUrlModal match={match} onResolve={onResolve} />);
    await userEvent.setup().click(screen.getByRole("button", { name }));
    expect(onResolve).toHaveBeenCalledWith(resolution);
  });

  it("closing the dialog counts as Cancel", async () => {
    const onResolve = vi.fn();
    render(<DuplicateUrlModal match={match} onResolve={onResolve} />);
    const user = userEvent.setup();
    await user.click(screen.getByText("file.zip"));
    await user.keyboard("{Escape}");
    expect(onResolve).toHaveBeenCalledWith("cancel");
  });

  it("maps every status word the Core can send, and passes unknown ones through", () => {
    expect(matchStatusLabel("downloading")).toBe("Downloading");
    expect(matchStatusLabel("paused")).toBe("Paused");
    expect(matchStatusLabel("failed")).toBe("Failed");
    expect(matchStatusLabel("probing")).toBe("Probing");
    expect(matchStatusLabel("mystery")).toBe("mystery");
  });
});
