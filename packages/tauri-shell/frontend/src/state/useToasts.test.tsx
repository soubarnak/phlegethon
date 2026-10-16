import { act, render, renderHook, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ReactNode } from "react";
import { describe, expect, it, vi } from "vitest";
import { ToastProvider, useToasts } from "./useToasts";

const wrapper = ({ children }: { children: ReactNode }) => <ToastProvider>{children}</ToastProvider>;

describe("useToasts", () => {
  it("throws outside the provider", () => {
    vi.spyOn(console, "error").mockImplementation(() => undefined);
    expect(() => renderHook(() => useToasts())).toThrow(/ToastProvider/);
  });

  it("shows a titled toast for each kind", () => {
    const { result } = renderHook(() => useToasts(), { wrapper });
    // The provider renders its toasts next to the hook's host, so render them through a probe.
    act(() => {
      result.current.info("Info title", "info detail");
      result.current.success("Success title");
      result.current.warning("Warning title");
    });
    expect(screen.getByText("Info title")).toBeInTheDocument();
    expect(screen.getByText("info detail")).toBeInTheDocument();
    expect(screen.getByText("Success title")).toBeInTheDocument();
    expect(screen.getByText("Warning title")).toBeInTheDocument();
  });

  it("error() shows the Core message and the hint for a known error code", () => {
    const { result } = renderHook(() => useToasts(), { wrapper });
    act(() => result.current.error("Probe failed", "[geo_blocked] not available in your country"));
    expect(screen.getByText("Probe failed")).toBeInTheDocument();
    expect(screen.getByText("not available in your country")).toBeInTheDocument();
    expect(screen.getByText(/not available in your region/)).toBeInTheDocument();
  });

  it("keeps only the five newest toasts", () => {
    const { result } = renderHook(() => useToasts(), { wrapper });
    act(() => {
      for (let i = 1; i <= 7; i++) result.current.info(`Toast ${i}`);
    });
    expect(screen.queryByText("Toast 1")).not.toBeInTheDocument();
    expect(screen.queryByText("Toast 2")).not.toBeInTheDocument();
    expect(screen.getByText("Toast 3")).toBeInTheDocument();
    expect(screen.getByText("Toast 7")).toBeInTheDocument();
  });

  it("the close button dismisses a toast", async () => {
    function Trigger() {
      const toasts = useToasts();
      return <button onClick={() => toasts.warning("Heads up")}>go</button>;
    }
    render(
      <ToastProvider>
        <Trigger />
      </ToastProvider>,
    );
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "go" }));
    const toast = screen.getByText("Heads up").closest("[role=status], [role=alert]") as HTMLElement;
    await user.click(within(toast).getByRole("button", { name: /close/i }));
    expect(screen.queryByText("Heads up")).not.toBeInTheDocument();
  });
});
