import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { Credit } from "./Credit";

describe("Credit", () => {
  it("shows the brand line", () => {
    render(<Credit />);
    expect(screen.getByText("Developed by Bongbetic")).toBeInTheDocument();
  });

  it("hides the glyph from assistive technology", () => {
    render(<Credit />);
    expect(screen.getByTestId("credit-glyph")).toHaveAttribute("aria-hidden", "true");
  });
});
