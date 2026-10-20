import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { Logo } from "./Logo";

describe("Logo", () => {
  it("exposes the brand name", () => {
    render(<Logo />);
    expect(screen.getByRole("img", { name: "Phlegethon" })).toBeInTheDocument();
  });

  it("draws three ember circles", () => {
    render(<Logo />);
    expect(screen.getAllByTestId("logo-circle")).toHaveLength(3);
  });
});
