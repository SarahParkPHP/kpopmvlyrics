import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { App, buildJsonExport } from "./App";

describe("App", () => {
  it("renders player controls and language toggles", () => {
    render(<App />);
    expect(screen.getByPlaceholderText("Paste a YouTube MV URL")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /Settings/i }));
    expect(screen.getByRole("button", { name: /Fetch Lyrics/i })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Original/i })).toBeInTheDocument();
  });

  it("builds the requested editor JSON export shape", () => {
    const payload = buildJsonExport(
      { videoId: "abc123", originalUrl: "https://www.youtube.com/watch?v=abc123", title: "Song", artistHint: "Group" },
      {
        song: { id: 1, title: "Song", artist: "Group", groupName: "Group" },
        provider: "fixture",
        members: [{ stageName: "Nayeon", color: "#e84855", imageUrl: "https://example.com/nayeon.jpg" }],
        lines: [
          {
            id: 1,
            songId: 1,
            index: 0,
            member: "Nayeon",
            original: "annyeong",
            romanization: "annyeong",
            english: "hello",
            layer: "backing",
          },
        ],
      },
      [{ lyricIndex: 0, captionIndex: 0, startMs: 1000, endMs: 2400, confidence: 1, needsReview: false }],
    );

    expect(payload).toEqual({
      version: 1,
      video: { platform: "youtube", videoId: "abc123", url: "https://www.youtube.com/watch?v=abc123" },
      members: [{ name: "Nayeon", color: "#e84855", imageUrl: "https://example.com/nayeon.jpg", localImagePath: null }],
      lyrics: [
        {
          index: 0,
          startMs: 1000,
          endMs: 2400,
          layer: "backing",
          member: "Nayeon",
          original: "annyeong",
          romanization: "annyeong",
          english: "hello",
        },
      ],
    });
  });
});
