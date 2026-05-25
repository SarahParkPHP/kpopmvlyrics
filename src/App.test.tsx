import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { App } from "./App";

describe("App", () => {
  it("renders player controls and language toggles", () => {
    render(<App />);
    expect(screen.getByPlaceholderText("Paste a YouTube MV URL")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /Settings/i }));
    expect(screen.getByRole("button", { name: /Fetch Lyrics/i })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Original/i })).toBeInTheDocument();
  });
});
