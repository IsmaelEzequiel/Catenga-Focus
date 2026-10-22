//! Read-only queries over the derived index (search, browse, history). No Tauri.

use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

// Matches are wrapped in U+0002 / U+0003 (control characters, never HTML) by the
// `char(2)` / `char(3)` arguments of highlight() and snippet() below.

pub const SEARCH_LIMIT: usize = 50;
pub const HISTORY_LIMIT: usize = 500;
const BROWSE_LIMIT: usize = 8;
const SUGGEST_LIMIT: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Suggestion {
    pub label: String,
    pub count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub date: String,
    pub start: u16,
    pub end: u16,
    pub ord: usize,
    pub title: String,
    pub tags: Vec<String>,
    pub title_hl: String,
    pub snippet: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResponse {
    pub hits: Vec<SearchHit>,
    pub titles: Vec<Suggestion>,
    pub tags: Vec<Suggestion>,
    pub truncated: bool,
    pub indexing: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HistoryKind {
    Title,
    Tag,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub date: String,
    pub start: u16,
    pub end: u16,
    pub ord: usize,
    pub title: String,
    pub tags: Vec<String>,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryResult {
    pub kind: HistoryKind,
    pub label: String,
    pub entries: Vec<HistoryEntry>,
    pub truncated: bool,
    pub indexing: bool,
}

const MAX_TOKENS: usize = 8;

/// Grouping key for titles: trimmed, whitespace runs collapsed, lowercased.
/// Diacritics are kept on purpose ("Redação" and "Redacao" are different histories).
pub fn norm(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Grouping key for tags: `norm` without a leading `#`.
pub fn norm_tag(s: &str) -> String {
    norm(s.trim().trim_start_matches('#'))
}

/// Turns free text into a safe FTS5 MATCH expression of AND-ed prefix tokens.
pub fn build_match_query(input: &str) -> Option<String> {
    let tokens: Vec<String> = input
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .take(MAX_TOKENS)
        .map(|t| format!("\"{}\"*", t.replace('"', "\"\"")))
        .collect();
    (!tokens.is_empty()).then(|| tokens.join(" "))
}

fn split_tags(tags: String) -> Vec<String> {
    tags.split_whitespace().map(str::to_string).collect()
}

/// Titles (or tags) whose normalised form contains `needle`, most used first.
/// The label is the form used by the most recent block. An empty needle matches all.
fn suggest_titles(
    conn: &Connection,
    needle: &str,
    limit: usize,
) -> rusqlite::Result<Vec<Suggestion>> {
    let mut stmt = conn.prepare(
        "SELECT (SELECT b2.title FROM blocks b2 WHERE b2.title_norm = b.title_norm
                 ORDER BY b2.date DESC, b2.start DESC, b2.ord DESC LIMIT 1), COUNT(*) AS c
         FROM blocks b
         WHERE b.title_norm <> '' AND (?1 = '' OR instr(b.title_norm, ?1) > 0)
         GROUP BY b.title_norm ORDER BY c DESC, b.title_norm LIMIT ?2",
    )?;
    stmt.query_map(params![needle, limit as i64], |r| {
        Ok(Suggestion {
            label: r.get(0)?,
            count: r.get::<_, i64>(1)? as usize,
        })
    })?
    .collect()
}

fn suggest_tags(
    conn: &Connection,
    needle: &str,
    limit: usize,
) -> rusqlite::Result<Vec<Suggestion>> {
    let mut stmt = conn.prepare(
        "SELECT (SELECT t2.tag FROM block_tags t2 JOIN blocks b2 ON b2.id = t2.block_id
                 WHERE t2.tag_norm = t.tag_norm
                 ORDER BY b2.date DESC, b2.start DESC, b2.ord DESC LIMIT 1),
                COUNT(DISTINCT t.block_id) AS c
         FROM block_tags t
         WHERE ?1 = '' OR instr(t.tag_norm, ?1) > 0
         GROUP BY t.tag_norm ORDER BY c DESC, t.tag_norm LIMIT ?2",
    )?;
    stmt.query_map(params![needle, limit as i64], |r| {
        Ok(Suggestion {
            label: r.get(0)?,
            count: r.get::<_, i64>(1)? as usize,
        })
    })?
    .collect()
}

fn hits(conn: &Connection, match_query: &str) -> rusqlite::Result<(Vec<SearchHit>, bool)> {
    let mut stmt = conn.prepare(
        "SELECT b.date, b.start, b.\"end\", b.ord, b.title, b.tags,
                highlight(blocks_fts, 0, char(2), char(3)),
                snippet(blocks_fts, 2, char(2), char(3), '…', 16)
         FROM blocks_fts JOIN blocks b ON b.id = blocks_fts.rowid
         WHERE blocks_fts MATCH ?1
         ORDER BY bm25(blocks_fts, 10.0, 5.0, 1.0), b.date DESC, b.start DESC, b.ord DESC
         LIMIT ?2",
    )?;
    let mut hits = stmt
        .query_map(params![match_query, (SEARCH_LIMIT + 1) as i64], |r| {
            Ok(SearchHit {
                date: r.get(0)?,
                start: r.get(1)?,
                end: r.get(2)?,
                ord: r.get::<_, i64>(3)? as usize,
                title: r.get(4)?,
                tags: split_tags(r.get(5)?),
                title_hl: r.get(6)?,
                snippet: r.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let truncated = hits.len() > SEARCH_LIMIT;
    hits.truncate(SEARCH_LIMIT);
    Ok((hits, truncated))
}

/// Full-text search, or the browse lists when `input` has no text. `indexing` is left false.
pub fn search(conn: &Connection, input: &str) -> rusqlite::Result<SearchResponse> {
    let mut response = SearchResponse::default();
    if input.trim().is_empty() {
        response.titles = suggest_titles(conn, "", BROWSE_LIMIT)?;
        response.tags = suggest_tags(conn, "", BROWSE_LIMIT)?;
        return Ok(response);
    }
    if let Some(match_query) = build_match_query(input) {
        (response.hits, response.truncated) = hits(conn, &match_query)?;
    }
    if input.trim_start().starts_with('#') {
        response.tags = suggest_tags(conn, &norm_tag(input), SUGGEST_LIMIT)?;
    } else {
        let needle = norm(input);
        response.titles = suggest_titles(conn, &needle, SUGGEST_LIMIT)?;
        response.tags = suggest_tags(conn, &norm_tag(input), SUGGEST_LIMIT)?;
    }
    Ok(response)
}

/// All blocks sharing a title or a tag, newest first. `indexing` is left false.
pub fn history(
    conn: &Connection,
    kind: HistoryKind,
    value: &str,
) -> rusqlite::Result<HistoryResult> {
    let (key, filter) = match kind {
        HistoryKind::Title => (norm(value), "title_norm = ?1"),
        HistoryKind::Tag => (
            norm_tag(value),
            "id IN (SELECT block_id FROM block_tags WHERE tag_norm = ?1)",
        ),
    };
    let mut result = HistoryResult {
        kind,
        label: value.trim().trim_start_matches('#').to_string(),
        entries: Vec::new(),
        truncated: false,
        indexing: false,
    };
    if key.is_empty() {
        return Ok(result);
    }
    let sql = format!(
        "SELECT date, start, \"end\", ord, title, tags, body FROM blocks WHERE {filter}
         ORDER BY date DESC, start DESC, ord DESC LIMIT ?2"
    );
    let mut stmt = conn.prepare(&sql)?;
    result.entries = stmt
        .query_map(params![key, (HISTORY_LIMIT + 1) as i64], |r| {
            Ok(HistoryEntry {
                date: r.get(0)?,
                start: r.get(1)?,
                end: r.get(2)?,
                ord: r.get::<_, i64>(3)? as usize,
                title: r.get(4)?,
                tags: split_tags(r.get(5)?),
                body: r.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    result.truncated = result.entries.len() > HISTORY_LIMIT;
    result.entries.truncate(HISTORY_LIMIT);
    if let Some(first) = result.entries.first() {
        result.label = match kind {
            HistoryKind::Title => first.title.clone(),
            HistoryKind::Tag => first
                .tags
                .iter()
                .find(|t| norm_tag(t) == key)
                .cloned()
                .unwrap_or(result.label),
        };
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::Index;
    use crate::index::test_util::Env;

    const MARK_START: char = '\u{2}';
    const MARK_END: char = '\u{3}';

    #[test]
    fn norm_trims_collapses_whitespace_and_lowercases() {
        assert_eq!(norm("  Deep \t Work\n"), "deep work");
        assert_eq!(norm("ÁRVORE"), "árvore");
        assert_eq!(norm(""), "");
        assert_eq!(norm("   "), "");
    }

    #[test]
    fn norm_keeps_diacritics() {
        assert_eq!(norm("Redação"), "redação");
        assert_ne!(norm("Redação"), norm("Redacao"));
    }

    #[test]
    fn norm_tag_strips_hash_and_lowercases() {
        assert_eq!(norm_tag("#Headline"), "headline");
        assert_eq!(norm_tag("Mestrado"), "mestrado");
        assert_eq!(norm_tag(" #X "), "x");
    }

    #[test]
    fn match_query_single_token_is_quoted_prefix() {
        assert_eq!(build_match_query("sear").as_deref(), Some("\"sear\"*"));
    }

    #[test]
    fn match_query_multiple_tokens_are_anded() {
        assert_eq!(
            build_match_query("deep  work").as_deref(),
            Some("\"deep\"* \"work\"*")
        );
    }

    #[test]
    fn match_query_none_for_empty_whitespace_or_punctuation_only() {
        for s in ["", "   ", "\t\n", "\"\"", "*", "-+()^#:", "\0"] {
            assert_eq!(build_match_query(s), None, "{s:?}");
        }
    }

    pub(crate) const HOSTILE: &[&str] = &[
        "\"",
        "a\"b",
        "AND",
        "OR",
        "NOT",
        "NEAR(a b)",
        "a*",
        "-a",
        "title:a",
        "^a",
        "(a",
        "a)",
        "a OR b",
        "#tag",
        "\"\"",
        "*",
    ];

    #[test]
    fn match_query_neutralises_operators_and_syntax() {
        for input in HOSTILE {
            let Some(q) = build_match_query(input) else {
                continue;
            };
            for tok in q.split(' ') {
                let inner = tok
                    .strip_prefix('"')
                    .and_then(|t| t.strip_suffix("\"*"))
                    .unwrap_or_else(|| panic!("{input:?} -> {q:?}: bad token {tok:?}"));
                assert!(
                    !inner.is_empty() && inner.chars().all(char::is_alphanumeric),
                    "{input:?} -> {q:?}"
                );
            }
        }
        assert_eq!(
            build_match_query("a OR b").as_deref(),
            Some("\"a\"* \"OR\"* \"b\"*")
        );
    }

    #[test]
    fn match_query_caps_token_count() {
        let q = build_match_query("a b c d e f g h i j k").unwrap();
        assert_eq!(q.split(' ').count(), 8);
    }

    #[test]
    fn match_query_handles_unicode_and_emoji() {
        assert_eq!(
            build_match_query("redação 日本語 🎉 x").as_deref(),
            Some("\"redação\"* \"日本語\"* \"x\"*")
        );
    }

    // ---- DB-backed tests ----

    fn setup(files: &[(&str, &str)]) -> (Env, Index) {
        let env = Env::new();
        for (date, text) in files {
            env.write(date, text);
        }
        let idx = env.open();
        idx.sync_all();
        (env, idx)
    }

    fn q(idx: &Index, input: &str) -> SearchResponse {
        search(&idx.reader().unwrap(), input).unwrap()
    }

    fn hist(idx: &Index, kind: HistoryKind, value: &str) -> HistoryResult {
        history(&idx.reader().unwrap(), kind, value).unwrap()
    }

    fn titles(r: &SearchResponse) -> Vec<&str> {
        r.hits.iter().map(|h| h.title.as_str()).collect()
    }

    #[test]
    fn search_hostile_input_never_errors() {
        let (_env, idx) = setup(&[("2026-10-20", "## 09:00–10:00 · A #t\nand or not near\n")]);
        let conn = idx.reader().unwrap();
        let big = "word ".repeat(2000);
        for input in
            HOSTILE
                .iter()
                .copied()
                .chain([big.as_str(), "a\0b", "\0", "x:y", "\u{2}\u{3}"])
        {
            assert!(search(&conn, input).is_ok(), "{input:?}");
        }
    }

    #[test]
    fn search_finds_by_body_title_and_tag() {
        let (_env, idx) = setup(&[(
            "2026-10-20",
            "## 09:00–10:00 · Searchlight #headline\nuniquebody here\n## 11:00–12:00 · Other\nnothing\n",
        )]);
        for word in ["uniquebody", "searchlight", "headline"] {
            let r = q(&idx, word);
            assert_eq!(titles(&r), ["Searchlight"], "{word}");
        }
        assert!(q(&idx, "absentword").hits.is_empty());
    }

    #[test]
    fn search_is_case_insensitive() {
        let (_env, idx) = setup(&[(
            "2026-10-20",
            "## 09:00–10:00 · Searchlight #Headline\nBODY\n",
        )]);
        assert_eq!(q(&idx, "SEARCHLIGHT").hits.len(), 1);
        assert_eq!(q(&idx, "headline").hits.len(), 1);
        assert_eq!(q(&idx, "body").hits.len(), 1);
    }

    #[test]
    fn search_is_diacritic_insensitive() {
        let (_env, idx) = setup(&[
            (
                "2026-10-20",
                "## 09:00–10:00 · Redação\numa ação importante\n",
            ),
            ("2026-10-21", "## 09:00–10:00 · redacao\nacao simples\n"),
        ]);
        assert_eq!(q(&idx, "redacao").hits.len(), 2);
        assert_eq!(q(&idx, "redação").hits.len(), 2);
        assert_eq!(q(&idx, "ACAO").hits.len(), 2);
        assert_eq!(q(&idx, "ação").hits.len(), 2);
    }

    #[test]
    fn search_prefix_matches_as_you_type() {
        let (_env, idx) = setup(&[("2026-10-20", "## 09:00–10:00 · Searchlight\nx\n")]);
        assert_eq!(q(&idx, "sear").hits.len(), 1);
        assert_eq!(q(&idx, "searchlight").hits.len(), 1);
        assert!(q(&idx, "searchlights").hits.is_empty());
    }

    #[test]
    fn search_multi_token_requires_all_tokens() {
        let (_env, idx) = setup(&[(
            "2026-10-20",
            "## 09:00–10:00 · A\nalpha beta\n## 10:00–11:00 · B\nalpha only\n",
        )]);
        assert_eq!(titles(&q(&idx, "alpha beta")), ["A"]);
        assert_eq!(q(&idx, "alpha").hits.len(), 2);
    }

    #[test]
    fn search_ranks_title_match_above_body_match() {
        let (_env, idx) = setup(&[
            ("2026-10-20", "## 09:00–10:00 · Body hit\nzebra\n"),
            ("2026-10-19", "## 09:00–10:00 · Zebra\nnothing\n"),
        ]);
        assert_eq!(titles(&q(&idx, "zebra")), ["Zebra", "Body hit"]);
    }

    #[test]
    fn search_returns_marked_snippet_and_title_highlight() {
        let (_env, idx) = setup(&[(
            "2026-10-20",
            "## 09:00–10:00 · Big Zebra\nthe quick zebra runs\n",
        )]);
        let hit = &q(&idx, "zebra").hits[0];
        assert_eq!(hit.title_hl, "Big \u{2}Zebra\u{3}");
        assert!(hit.snippet.contains("\u{2}zebra\u{3}"), "{:?}", hit.snippet);
        for text in [&hit.title_hl, &hit.snippet] {
            let starts = text.matches(MARK_START).count();
            assert_eq!(starts, text.matches(MARK_END).count());
        }
    }

    #[test]
    fn search_orders_equal_rank_by_newest_date() {
        let same = "## 09:00–10:00 · Same\nzebra\n";
        let (_env, idx) = setup(&[
            ("2026-10-18", same),
            ("2026-10-20", same),
            ("2026-10-19", same),
        ]);
        let dates: Vec<_> = q(&idx, "zebra")
            .hits
            .iter()
            .map(|h| h.date.clone())
            .collect();
        assert_eq!(dates, ["2026-10-20", "2026-10-19", "2026-10-18"]);
    }

    fn many_blocks(n: usize, title: &str) -> String {
        (0..n)
            .map(|i| {
                let (s, e) = (i * 30, i * 30 + 30);
                format!(
                    "## {:02}:{:02}–{:02}:{:02} · {title}\nzebra {i}\n",
                    s / 60,
                    s % 60,
                    e / 60,
                    e % 60
                )
            })
            .collect()
    }

    #[test]
    fn search_limits_to_50_and_sets_truncated() {
        let (_env, idx) = setup(&[
            ("2026-10-20", &many_blocks(40, "T")),
            ("2026-10-21", &many_blocks(20, "T")),
        ]);
        let r = q(&idx, "zebra");
        assert_eq!(r.hits.len(), 50);
        assert!(r.truncated);
        let exact = q(&idx, "zebra 3");
        assert!(!exact.truncated);
    }

    #[test]
    fn search_hit_carries_date_start_end_ord_title_tags() {
        let (_env, idx) = setup(&[(
            "2026-10-20",
            "## 09:00–10:00 · First\nx\n## 14:30–15:45 · Second one #a #B\nzebra\n",
        )]);
        let hit = &q(&idx, "zebra").hits[0];
        assert_eq!(
            (
                hit.date.as_str(),
                hit.start,
                hit.end,
                hit.ord,
                hit.title.as_str()
            ),
            ("2026-10-20", 870, 945, 1, "Second one")
        );
        assert_eq!(hit.tags, ["a", "B"]);
    }

    fn browse_files() -> Vec<(&'static str, String)> {
        let mut day1 = String::new();
        for (i, (t, tag)) in [
            ("Alpha", "x"),
            ("Alpha", "x"),
            ("Alpha", "y"),
            ("Beta", "y"),
            ("Beta", "y"),
            ("Gamma", "z"),
        ]
        .iter()
        .enumerate()
        {
            day1.push_str(&format!("## {:02}:00–{:02}:30 · {t} #{tag}\nn\n", i, i));
        }
        vec![("2026-10-20", day1)]
    }

    #[test]
    fn search_empty_query_returns_browse_top_titles_and_tags_by_count() {
        let files = browse_files();
        let refs: Vec<(&str, &str)> = files.iter().map(|(d, t)| (*d, t.as_str())).collect();
        let (_env, idx) = setup(&refs);
        for input in ["", "   "] {
            let r = q(&idx, input);
            assert!(r.hits.is_empty());
            let t: Vec<_> = r
                .titles
                .iter()
                .map(|s| (s.label.as_str(), s.count))
                .collect();
            assert_eq!(t, [("Alpha", 3), ("Beta", 2), ("Gamma", 1)]);
            let g: Vec<_> = r.tags.iter().map(|s| (s.label.as_str(), s.count)).collect();
            assert_eq!(g, [("y", 3), ("x", 2), ("z", 1)]);
        }
    }

    #[test]
    fn search_browse_is_capped_at_eight() {
        let text: String = (0..12)
            .map(|i| format!("## {i:02}:00–{i:02}:30 · T{i} #g{i}\nn\n"))
            .collect();
        let (_env, idx) = setup(&[("2026-10-20", &text)]);
        let r = q(&idx, "");
        assert_eq!(r.titles.len(), 8);
        assert_eq!(r.tags.len(), 8);
    }

    #[test]
    fn search_suggests_titles_and_tags_containing_input() {
        let (_env, idx) = setup(&[(
            "2026-10-20",
            "## 09:00–10:00 · Deep work #workflow\nn\n## 10:00–11:00 · Network\nn\n## 11:00–12:00 · Other #misc\nn\n",
        )]);
        let r = q(&idx, "WORK");
        let mut t: Vec<_> = r.titles.iter().map(|s| s.label.as_str()).collect();
        t.sort_unstable();
        assert_eq!(t, ["Deep work", "Network"]);
        let g: Vec<_> = r.tags.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(g, ["workflow"]);
    }

    #[test]
    fn search_suggestions_are_capped_at_four() {
        let text: String = (0..7)
            .map(|i| format!("## {i:02}:00–{i:02}:30 · task{i} #task{i}\nn\n"))
            .collect();
        let (_env, idx) = setup(&[("2026-10-20", &text)]);
        let r = q(&idx, "task");
        assert_eq!(r.titles.len(), 4);
        assert_eq!(r.tags.len(), 4);
    }

    #[test]
    fn search_hash_prefix_suggests_only_tags() {
        let (_env, idx) = setup(&[(
            "2026-10-20",
            "## 09:00–10:00 · Headline notes #headline\nn\n## 10:00–11:00 · Head start\nn\n",
        )]);
        let r = q(&idx, "#head");
        assert!(r.titles.is_empty());
        assert_eq!(r.tags.len(), 1);
        assert_eq!(r.tags[0].label, "headline");
    }

    #[test]
    fn search_suggestion_label_uses_most_recent_display_form() {
        let (_env, idx) = setup(&[
            ("2026-10-20", "## 09:00–10:00 · Deep Work #Study\nn\n"),
            ("2026-10-21", "## 09:00–10:00 · deep work #study\nn\n"),
            ("2026-10-19", "## 09:00–10:00 · DEEP WORK #STUDY\nn\n"),
        ]);
        let r = q(&idx, "deep");
        assert_eq!(
            r.titles,
            [Suggestion {
                label: "deep work".into(),
                count: 3
            }]
        );
        let r = q(&idx, "#stu");
        assert_eq!(
            r.tags,
            [Suggestion {
                label: "study".into(),
                count: 3
            }]
        );
    }

    #[test]
    fn history_by_title_ignores_case_and_extra_whitespace() {
        let (_env, idx) = setup(&[
            ("2026-10-20", "## 09:00–10:00 · Deep   work\na\n"),
            ("2026-10-21", "## 09:00–10:00 · deep work\nb\n"),
            ("2026-10-22", "## 09:00–10:00 · Deep works\nc\n"),
        ]);
        for value in ["  Deep   Work", "deep work", "DEEP WORK "] {
            let h = hist(&idx, HistoryKind::Title, value);
            assert_eq!(h.entries.len(), 2, "{value:?}");
        }
    }

    #[test]
    fn history_by_title_does_not_fold_diacritics() {
        let (_env, idx) = setup(&[
            ("2026-10-20", "## 09:00–10:00 · Redação\na\n"),
            ("2026-10-21", "## 09:00–10:00 · Redacao\nb\n"),
        ]);
        let h = hist(&idx, HistoryKind::Title, "redação");
        assert_eq!(h.entries.len(), 1);
        assert_eq!(h.entries[0].date, "2026-10-20");
    }

    #[test]
    fn history_by_title_empty_value_returns_empty() {
        let (_env, idx) = setup(&[("2026-10-20", "## 09:00–10:00\nuntitled\n")]);
        for value in ["", "   "] {
            let h = hist(&idx, HistoryKind::Title, value);
            assert!(h.entries.is_empty());
            assert!(!h.truncated);
        }
        assert!(hist(&idx, HistoryKind::Tag, "").entries.is_empty());
    }

    #[test]
    fn history_by_tag_matches_across_different_titles_and_is_case_insensitive() {
        let (_env, idx) = setup(&[
            ("2026-10-20", "## 09:00–10:00 · A #Study\na\n"),
            ("2026-10-21", "## 09:00–10:00 · B #x #study\nb\n"),
            ("2026-10-22", "## 09:00–10:00 · C #other\nc\n"),
        ]);
        for value in ["study", "#STUDY", " Study "] {
            let h = hist(&idx, HistoryKind::Tag, value);
            let t: Vec<_> = h.entries.iter().map(|e| e.title.as_str()).collect();
            assert_eq!(t, ["B", "A"], "{value:?}");
        }
    }

    #[test]
    fn history_orders_newest_first_with_start_then_ord_tiebreak() {
        let (_env, idx) = setup(&[
            (
                "2026-10-20",
                "## 14:00–15:00 · T\nlate\n## 09:00–10:00 · T\nearly\n",
            ),
            (
                "2026-10-21",
                "## 09:00–10:00 · T\nfirst\n## 09:00–10:00 · T\nsecond\n",
            ),
            ("2026-10-19", "## 09:00–10:00 · T\noldest\n"),
        ]);
        let h = hist(&idx, HistoryKind::Title, "t");
        let got: Vec<_> = h
            .entries
            .iter()
            .map(|e| (e.date.as_str(), e.start, e.ord))
            .collect();
        assert_eq!(
            got,
            [
                ("2026-10-21", 540, 1),
                ("2026-10-21", 540, 0),
                ("2026-10-20", 840, 0),
                ("2026-10-20", 540, 1),
                ("2026-10-19", 540, 0)
            ]
        );
    }

    #[test]
    fn history_truncates_at_500_and_sets_flag() {
        let files: Vec<(String, String)> = (1..=11)
            .map(|d| (format!("2026-10-{d:02}"), many_blocks(48, "Daily")))
            .collect();
        let refs: Vec<(&str, &str)> = files
            .iter()
            .map(|(d, t)| (d.as_str(), t.as_str()))
            .collect();
        let (_env, idx) = setup(&refs);
        let h = hist(&idx, HistoryKind::Title, "daily");
        assert_eq!(h.entries.len(), 500);
        assert!(h.truncated);
        assert_eq!(h.entries[0].date, "2026-10-11");
    }

    #[test]
    fn history_includes_blocks_with_empty_notes() {
        let (_env, idx) = setup(&[(
            "2026-10-20",
            "## 09:00–10:00 · T\n## 10:00–11:00 · T\nnote\n",
        )]);
        let h = hist(&idx, HistoryKind::Title, "t");
        assert_eq!(h.entries.len(), 2);
        assert!(h.entries.iter().any(|e| e.body.trim().is_empty()));
    }

    #[test]
    fn history_label_is_most_recent_display_form() {
        let (_env, idx) = setup(&[
            ("2026-10-20", "## 09:00–10:00 · Deep Work #Study\na\n"),
            ("2026-10-21", "## 09:00–10:00 · deep  work #study\nb\n"),
        ]);
        assert_eq!(
            hist(&idx, HistoryKind::Title, "deep work").label,
            "deep  work"
        );
        assert_eq!(hist(&idx, HistoryKind::Tag, "STUDY").label, "study");
        assert_eq!(hist(&idx, HistoryKind::Tag, "#nothing").label, "nothing");
    }

    #[test]
    fn history_entry_carries_full_body_tags_and_ord() {
        let (_env, idx) = setup(&[(
            "2026-10-20",
            "## 09:00–10:00 · Other\nx\n## 10:00–11:00 · Target #a #b\n\nline one\n\nline two\n",
        )]);
        let e = &hist(&idx, HistoryKind::Title, "target").entries[0];
        assert_eq!(e.ord, 1);
        assert_eq!((e.start, e.end), (600, 660));
        assert_eq!(e.tags, ["a", "b"]);
        assert_eq!(e.body, "\nline one\n\nline two\n");
    }

    #[test]
    fn deleting_db_and_syncing_again_gives_identical_search_and_history_results() {
        let (env, idx) = setup(&[
            (
                "2026-10-20",
                "## 09:00–10:00 · Deep work #study\nzebra note\n## 11:00–12:00 · Other\nzebra\n",
            ),
            (
                "2026-10-21",
                "## 09:00–10:00 · Deep Work #study\nmore zebra\n",
            ),
        ]);
        let before = (
            q(&idx, "zebra"),
            q(&idx, ""),
            hist(&idx, HistoryKind::Title, "deep work"),
            hist(&idx, HistoryKind::Tag, "study"),
        );
        drop(idx);
        for suffix in ["", "-wal", "-shm"] {
            let mut p = env.db.clone().into_os_string();
            p.push(suffix);
            let _ = std::fs::remove_file(p);
        }
        let idx = env.open();
        idx.sync_all();
        let after = (
            q(&idx, "zebra"),
            q(&idx, ""),
            hist(&idx, HistoryKind::Title, "deep work"),
            hist(&idx, HistoryKind::Tag, "study"),
        );
        assert_eq!(before, after);
        assert!(!before.0.hits.is_empty());
    }
}
