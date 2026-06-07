import { describe, expect, it } from "vitest";
import { buildMessageWithNoteContext } from "./chatNoteContext";

describe("buildMessageWithNoteContext", () => {
  it("wraps user message with note body in French", () => {
    const out = buildMessageWithNoteContext(
      "Qu'en penses-tu ?",
      { noteId: "abc", title: "Ma note", markdown: "# Hello\n\nWorld", intent: "discuss" },
      "fr",
    );
    expect(out).toContain("Ma note");
    expect(out).toContain("# Hello");
    expect(out).toContain("Qu'en penses-tu ?");
    expect(out).toContain("source principale");
  });

  it("includes improve instructions in English", () => {
    const out = buildMessageWithNoteContext(
      "Fix this",
      { noteId: "x", title: "Draft", markdown: "text", intent: "improve" },
      "en",
    );
    expect(out).toContain("improve and correct");
    expect(out).toContain("notes_write");
  });

  it("truncates very long notes", () => {
    const long = "x".repeat(50_000);
    const out = buildMessageWithNoteContext(
      "hi",
      { noteId: "1", title: "T", markdown: long },
      "en",
    );
    expect(out).toContain("truncated");
    expect(out.length).toBeLessThan(long.length);
  });
});
