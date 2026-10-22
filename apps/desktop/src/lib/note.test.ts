import { describe, expect, it } from "vitest";
import { applyNote, bodyToNote, isLastBlock } from "./note";

describe("bodyToNote", () => {
  it("bodyToNote_empty", () => {
    expect(bodyToNote("")).toBe("");
    expect(bodyToNote("\n\n")).toBe("");
  });

  it("bodyToNote_strips_lead_and_trail_blank_lines", () => {
    expect(bodyToNote("\nhello\nworld\n\n\n")).toBe("hello\nworld");
  });

  it("bodyToNote_keeps_inner_blank_lines", () => {
    expect(bodyToNote("\na\n\n\nb\n\n")).toBe("a\n\n\nb");
  });

  it("bodyToNote_keeps_indent_of_first_line", () => {
    expect(bodyToNote("\n    code\nmore\n")).toBe("    code\nmore");
  });

  it("bodyToNote_without_blank_after_heading", () => {
    expect(bodyToNote("note\n")).toBe("note");
  });
});

describe("applyNote", () => {
  it("applyNote_empty_note_gives_empty_body", () => {
    expect(applyNote("\nold\n\n", "", false)).toBe("");
  });

  it("applyNote_whitespace_only_note_gives_empty_body", () => {
    expect(applyNote("old\n", "  \n\t\n", true)).toBe("");
  });

  it("applyNote_adds_blank_line_after_heading_when_body_was_empty", () => {
    expect(applyNote("", "hi", true)).toBe("\nhi\n");
  });

  it("applyNote_keeps_original_lead_and_trail", () => {
    expect(applyNote("old\n\n\n", "new", false)).toBe("new\n\n\n");
    expect(applyNote("\n\nold\n\n", "new", false)).toBe("\n\nnew\n\n");
  });

  it("applyNote_adds_trailing_blank_when_not_last", () => {
    expect(applyNote("", "hi", false)).toBe("\nhi\n\n");
  });

  it("applyNote_no_trailing_blank_when_last", () => {
    expect(applyNote("\nold\n", "new", true)).toBe("\nnew\n");
  });

  it("applyNote_trims_trailing_newlines_typed_by_user", () => {
    expect(applyNote("", "hi\n\n\n", true)).toBe("\nhi\n");
  });

  it("applyNote_normalizes_crlf", () => {
    expect(applyNote("", "a\r\nb", true)).toBe("\na\nb\n");
  });

  it("applyNote_keeps_lookalike_and_fence_text_unchanged", () => {
    const note = "## 10:00–11:00 · X\n```\nopen";
    expect(applyNote("", note, true)).toBe(`\n${note}\n`);
  });

  it("applyNote_then_bodyToNote_is_identity_for_trimmed_text", () => {
    for (const text of ["a", "a\n\nb", "  indented\nx", "one\n\n\ntwo"]) {
      for (const old of ["", "\nold\n\n", "old\n"]) {
        expect(bodyToNote(applyNote(old, text, false))).toBe(text);
        expect(bodyToNote(applyNote(old, text, true))).toBe(text);
      }
    }
  });
});

describe("isLastBlock", () => {
  const b = (key: string, start: number, end: number) => ({ key, start, end });

  it("isLastBlock_uses_start_then_end", () => {
    const blocks = [b("a", 540, 600), b("c", 840, 900), b("b", 540, 700), b("d", 840, 880)];
    expect(isLastBlock(blocks, "c")).toBe(true);
    expect(isLastBlock(blocks, "d")).toBe(false);
    expect(isLastBlock(blocks, "a")).toBe(false);
  });

  it("isLastBlock_ties_go_to_later_index", () => {
    const blocks = [b("a", 540, 600), b("b", 540, 600)];
    expect(isLastBlock(blocks, "b")).toBe(true);
    expect(isLastBlock(blocks, "a")).toBe(false);
  });
});
