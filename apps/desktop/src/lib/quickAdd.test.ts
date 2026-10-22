import { describe, expect, it } from "vitest";
import { parseQuickAdd } from "./quickAdd";

const ok = (start: number, end: number, title: string, tags: string[] = []) => ({ start, end, title, tags });

describe("parseQuickAdd", () => {
  it("parses_a_range_title_and_tag", () => {
    expect(parseQuickAdd("9-10:30 deep work #mestrado")).toEqual(ok(540, 630, "deep work", ["mestrado"]));
  });

  it("accepts_the_h_forms_and_several_tags", () => {
    expect(parseQuickAdd("9h-10h30 Aula #mestrado #tc")).toEqual(ok(540, 630, "Aula", ["mestrado", "tc"]));
    expect(parseQuickAdd("9h30-11h x")).toEqual(ok(570, 660, "x"));
  });

  it("accepts_spaces_around_the_separator", () => {
    expect(parseQuickAdd("14 - 15 inglês")).toEqual(ok(840, 900, "inglês"));
  });

  it("accepts_dot_minutes_and_padded_hours", () => {
    expect(parseQuickAdd("09.30-10.45 x")).toEqual(ok(570, 645, "x"));
    expect(parseQuickAdd("09-10 x")).toEqual(ok(540, 600, "x"));
  });

  it("accepts_en_and_em_dashes_and_a_leading_middle_dot", () => {
    expect(parseQuickAdd("09:00–10:30 · Deep work #x")).toEqual(ok(540, 630, "Deep work", ["x"]));
    expect(parseQuickAdd("9—10 y")).toEqual(ok(540, 600, "y"));
  });

  it("accepts_to_and_ate_separators", () => {
    expect(parseQuickAdd("9 to 10 Leitura")).toEqual(ok(540, 600, "Leitura"));
    expect(parseQuickAdd("9 até 10:30 Leitura")).toEqual(ok(540, 630, "Leitura"));
    expect(parseQuickAdd("9to10 Leitura")).toEqual(ok(540, 600, "Leitura"));
  });

  it("allows_24_as_the_end", () => {
    expect(parseQuickAdd("23-24 late")).toEqual(ok(1380, 1440, "late"));
    expect(parseQuickAdd("23:00-24:00 late")).toEqual(ok(1380, 1440, "late"));
  });

  it("snaps_to_the_nearest_15_minutes", () => {
    expect(parseQuickAdd("9:07-10:08 x")).toEqual(ok(540, 615, "x"));
    expect(parseQuickAdd("9:22-9:38 x")).toEqual(ok(555, 585, "x"));
  });

  it("collects_tags_anywhere_after_the_range", () => {
    expect(parseQuickAdd("9-10 #a deep #b work")).toEqual(ok(540, 600, "deep work", ["a", "b"]));
  });

  it("keeps_a_bare_hash_in_the_title", () => {
    expect(parseQuickAdd("9-10 Learn C# #x")).toEqual(ok(540, 600, "Learn C#", ["x"]));
  });

  it("trims_surrounding_whitespace_and_is_case_insensitive_for_to", () => {
    expect(parseQuickAdd("  9 TO 10   x  ")).toEqual(ok(540, 600, "x"));
  });

  it("errors_without_a_range", () => {
    expect(parseQuickAdd("deep work")).toEqual({ error: "Start with a time range, e.g. 9-10:30" });
    expect(parseQuickAdd("")).toEqual({ error: "Start with a time range, e.g. 9-10:30" });
    expect(parseQuickAdd("9 deep work")).toEqual({ error: "Start with a time range, e.g. 9-10:30" });
  });

  it("errors_when_the_end_is_not_after_the_start", () => {
    expect(parseQuickAdd("10-9 x")).toEqual({ error: "The end must be after the start" });
    expect(parseQuickAdd("9-9 x")).toEqual({ error: "The end must be after the start" });
    expect(parseQuickAdd("9:00-9:05 x")).toEqual({ error: "The end must be after the start" });
  });

  it("errors_on_invalid_hours_and_minutes", () => {
    expect(parseQuickAdd("25-26 x")).toEqual({ error: "That isn't a valid time" });
    expect(parseQuickAdd("9:75-10 x")).toEqual({ error: "That isn't a valid time" });
    expect(parseQuickAdd("24-25 x")).toEqual({ error: "That isn't a valid time" });
    expect(parseQuickAdd("9-24:30 x")).toEqual({ error: "That isn't a valid time" });
    expect(parseQuickAdd("24-24 x")).toEqual({ error: "That isn't a valid time" });
  });

  it("errors_on_an_empty_title", () => {
    expect(parseQuickAdd("9-10")).toEqual({ error: "Add a title after the time range" });
    expect(parseQuickAdd("9-10 #tag")).toEqual({ error: "Add a title after the time range" });
    expect(parseQuickAdd("9-10 · ")).toEqual({ error: "Add a title after the time range" });
  });
});
