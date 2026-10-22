import { describe, expect, it } from "vitest";
import { addTagChips, cleanTagNames, formatTitleInput, isValidTagName, parseTagChips, parseTitleInput, suggestTags, tagColorVar, tagVar } from "./tags";

describe("parseTitleInput", () => {
  it("parse_peels_trailing_tags", () => {
    expect(parseTitleInput("Searchlight #headline #deep-work")).toEqual({
      title: "Searchlight",
      tags: ["headline", "deep-work"],
    });
  });

  it("parse_keeps_mid_hash", () => {
    expect(parseTitleInput("Fix #123 bug")).toEqual({ title: "Fix #123 bug", tags: [] });
    expect(parseTitleInput("Learn C#")).toEqual({ title: "Learn C#", tags: [] });
    expect(parseTitleInput("Title #")).toEqual({ title: "Title #", tags: [] });
  });

  it("parse_empty_input", () => {
    expect(parseTitleInput("")).toEqual({ title: "", tags: [] });
    expect(parseTitleInput("   ")).toEqual({ title: "", tags: [] });
  });

  it("parse_only_tags", () => {
    expect(parseTitleInput("#a #b")).toEqual({ title: "", tags: ["a", "b"] });
    expect(parseTitleInput("#solo")).toEqual({ title: "", tags: ["solo"] });
  });

  it("parse_unicode_tags", () => {
    expect(parseTitleInput("Reunião #pós_grad")).toEqual({ title: "Reunião", tags: ["pós_grad"] });
  });

  it("format_roundtrips_parse", () => {
    for (const s of ["Searchlight #headline", "#a #b", "Plain title", "Fix #123 bug #x", ""]) {
      const { title, tags } = parseTitleInput(s);
      expect(formatTitleInput(title, tags)).toBe(s);
    }
  });
});

describe("tagColorVar", () => {
  it("tagColor_first_mapped_tag_wins", () => {
    expect(tagColorVar(["a", "b", "c"], { b: "sky", c: "leaf" })).toBe("var(--color-tag-sky)");
  });

  it("tagColor_ignores_unknown_palette_names", () => {
    const c = tagColorVar(["a"], { a: "purple" });
    expect(c).toMatch(/^var\(--color-tag-(sun|clay|leaf|sky)\)$/);
  });

  it("tagColor_fallback_is_deterministic", () => {
    const a = tagColorVar(["mestrado"], {});
    expect(a).toBe(tagColorVar(["mestrado"], {}));
    expect(a).toMatch(/^var\(--color-tag-(sun|clay|leaf|sky)\)$/);
    const seen = new Set(["a", "b", "c", "d", "e", "f", "g", "h"].map((t) => tagColorVar([t], {})));
    expect(seen.size).toBeGreaterThan(1);
  });

  it("tagColor_untagged_is_neutral", () => {
    expect(tagColorVar([], { a: "sky" })).toBeNull();
  });
});

describe("tag chip input", () => {
  it("parse_splits_on_space_comma_and_strips_hash", () => {
    expect(parseTagChips("#rust, go  #Study")).toEqual(["rust", "go", "Study"]);
  });
  it("parse_drops_empty_and_invalid_tokens", () => {
    expect(parseTagChips(" , # ,, ")).toEqual([]);
    expect(parseTagChips("ok a/b")).toEqual(["ok"]);
  });
  it("add_dedupes_case_insensitively_keeping_the_first", () => {
    expect(addTagChips(["Rust"], "rust go GO")).toEqual(["Rust", "go"]);
  });
  it("clean_normalises_a_list", () => {
    expect(cleanTagNames(["#Rust", " rust ", "", "#", "Go"])).toEqual(["Rust", "Go"]);
  });
  it("suggest_filters_unused_by_prefix_then_substring_and_limits", () => {
    const names = ["rust", "rustacean", "trust", "go", "mestrado"];
    expect(suggestTags(names, ["go"], "rus")).toEqual(["rust", "rustacean", "trust"]);
    expect(suggestTags(names, ["rust"], "#rus")).toEqual(["rustacean", "trust"]);
    expect(suggestTags(names, [], "", 2)).toEqual(["rust", "rustacean"]);
  });
  it("tag_var_uses_the_palette_name", () => {
    expect(tagVar("clay")).toBe("var(--color-tag-clay)");
    expect(tagVar("nope")).toBe("var(--color-tag-sun)");
  });
  it("is_valid_tag_name_matches_the_heading_rule", () => {
    expect(isValidTagName("rust-2")).toBe(true);
    expect(isValidTagName("a b")).toBe(false);
    expect(isValidTagName("")).toBe(false);
  });
});
