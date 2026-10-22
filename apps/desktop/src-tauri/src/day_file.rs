//! Parser and serializer for the markdown day file (`YYYY-MM-DD.md`).
//! Pure: no I/O. See "Data" in CLAUDE.md for the format.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Eol {
    #[default]
    Lf,
    Crlf,
}

/// One time block. `start`/`end` are minutes since 00:00 (`0 <= start < end <= 1440`).
/// `body` is the raw text under the heading; every line ends in `\n`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Block {
    pub start: u16,
    pub end: u16,
    pub title: String,
    pub tags: Vec<String>,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayFile {
    pub preamble: String,
    pub blocks: Vec<Block>,
    pub eol: Eol,
    pub final_newline: bool,
}

impl Default for DayFile {
    fn default() -> Self {
        DayFile {
            preamble: String::new(),
            blocks: Vec::new(),
            eol: Eol::Lf,
            final_newline: true,
        }
    }
}

const DASH: char = '–';
const DOT: char = '·';

pub fn parse(text: &str) -> DayFile {
    let eol = if text.contains("\r\n") {
        Eol::Crlf
    } else {
        Eol::Lf
    };
    let mut text = text.replace("\r\n", "\n");
    let final_newline = text.is_empty() || text.ends_with('\n');
    if !final_newline {
        text.push('\n');
    }

    let mut preamble = String::new();
    let mut blocks: Vec<Block> = Vec::new();
    let mut fence: Option<(char, usize)> = None;

    for line in text.split_inclusive('\n') {
        let content = line.strip_suffix('\n').unwrap_or(line);
        match fence {
            Some((ch, len)) => {
                if closes_fence(content, ch, len) {
                    fence = None;
                }
            }
            None => {
                if let Some(opener) = opens_fence(content) {
                    fence = Some(opener);
                } else if let Some(block) = parse_heading(content) {
                    blocks.push(block);
                    continue;
                }
            }
        }
        match blocks.last_mut() {
            Some(b) => b.body.push_str(line),
            None => preamble.push_str(line),
        }
    }

    DayFile {
        preamble,
        blocks,
        eol,
        final_newline,
    }
}

fn fence_run(line: &str) -> Option<(char, usize, &str)> {
    let trimmed = line.trim_start_matches(' ');
    if line.len() - trimmed.len() > 3 {
        return None;
    }
    let ch = trimmed.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let len = trimmed.chars().take_while(|c| *c == ch).count();
    (len >= 3).then(|| (ch, len, &trimmed[len..]))
}

fn opens_fence(line: &str) -> Option<(char, usize)> {
    fence_run(line).map(|(ch, len, _)| (ch, len))
}

fn closes_fence(line: &str, ch: char, min_len: usize) -> bool {
    matches!(fence_run(line), Some((c, len, rest)) if c == ch && len >= min_len && rest.trim().is_empty())
}

fn parse_hhmm(s: &str) -> Option<(u16, u16)> {
    let b = s.as_bytes();
    if b.len() != 5 || b[2] != b':' {
        return None;
    }
    if ![0, 1, 3, 4].iter().all(|&i| b[i].is_ascii_digit()) {
        return None;
    }
    let num = |i: usize| u16::from(b[i] - b'0') * 10 + u16::from(b[i + 1] - b'0');
    Some((num(0), num(3)))
}

fn parse_heading(line: &str) -> Option<Block> {
    let rest = line.strip_prefix("## ")?;
    let (sh, sm) = parse_hhmm(rest.get(0..5)?)?;
    let rest = &rest[5..];
    let rest = rest.strip_prefix(DASH).or_else(|| rest.strip_prefix('-'))?;
    let (eh, em) = parse_hhmm(rest.get(0..5)?)?;
    let rest = rest[5..].strip_prefix(' ')?.strip_prefix(DOT)?;
    let rest = if rest.is_empty() {
        rest
    } else {
        rest.strip_prefix(' ')?
    };

    if sh > 23 || sm > 59 || eh > 24 || em > 59 || (eh == 24 && em != 0) {
        return None;
    }
    let (start, end) = (sh * 60 + sm, eh * 60 + em);
    if end <= start {
        return None;
    }
    let (title, tags) = split_title_tags(rest);
    Some(Block {
        start,
        end,
        title,
        tags,
        body: String::new(),
    })
}

fn is_tag(token: &str) -> bool {
    match token.strip_prefix('#') {
        Some(name) => {
            !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
        }
        None => false,
    }
}

fn split_title_tags(rest: &str) -> (String, Vec<String>) {
    let mut rest = rest.trim();
    let mut tags = Vec::new();
    loop {
        let (head, token) = match rest.rfind(char::is_whitespace) {
            Some(i) => (rest[..i].trim_end(), &rest[i..]),
            None => ("", rest),
        };
        let token = token.trim_start();
        if !is_tag(token) {
            break;
        }
        tags.push(token[1..].to_string());
        rest = head;
    }
    tags.reverse();
    (rest.to_string(), tags)
}

fn guard_body(body: &str, is_last: bool) -> String {
    let mut out = String::with_capacity(body.len());
    let mut fence: Option<(char, usize)> = None;
    for line in body.split_inclusive('\n') {
        let content = line.strip_suffix('\n').unwrap_or(line);
        match fence {
            Some((ch, len)) => {
                if closes_fence(content, ch, len) {
                    fence = None;
                }
            }
            None => {
                if let Some(opener) = opens_fence(content) {
                    fence = Some(opener);
                } else if parse_heading(content).is_some() {
                    out.push('\\');
                }
            }
        }
        out.push_str(line);
    }
    if let (Some((ch, len)), false) = (fence, is_last) {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.extend(std::iter::repeat_n(ch, len));
        out.push('\n');
    }
    out
}

fn fmt_time(minutes: u16) -> String {
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

impl DayFile {
    pub fn serialize(&self) -> String {
        let mut out = self.preamble.clone();
        for b in &self.blocks {
            out.push_str("## ");
            out.push_str(&fmt_time(b.start));
            out.push(DASH);
            out.push_str(&fmt_time(b.end));
            out.push(' ');
            out.push(DOT);
            if !b.title.is_empty() {
                out.push(' ');
                out.push_str(&b.title);
            }
            for tag in &b.tags {
                out.push_str(" #");
                out.push_str(tag);
            }
            out.push('\n');
            out.push_str(&b.body);
        }
        if !self.final_newline && out.ends_with('\n') {
            out.pop();
        }
        match self.eol {
            Eol::Lf => out,
            Eol::Crlf => out.replace('\n', "\r\n"),
        }
    }

    /// Stable sort by `(start, end)`.
    pub fn sort_blocks(&mut self) {
        self.blocks.sort_by_key(|b| (b.start, b.end));
    }

    /// Makes free-text bodies safe to write: escapes heading look-alikes with a
    /// leading `\` and closes an open fence on every block but the last, so a
    /// note can never create or swallow a block on re-parse. No-op on parsed bodies.
    pub fn guard_bodies(&mut self) {
        let last = self.blocks.len().saturating_sub(1);
        for (i, block) in self.blocks.iter_mut().enumerate() {
            block.body = guard_body(&block.body, i == last);
        }
    }
}

impl Block {
    /// Sanitised copy that is guaranteed to serialize to a heading that re-parses.
    pub fn normalized(&self) -> Result<Block, String> {
        if self.start >= self.end || self.end > 1440 {
            return Err(format!(
                "invalid block times: start {} end {} (need start < end <= 1440)",
                self.start, self.end
            ));
        }
        let title: String = self
            .title
            .chars()
            .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
            .collect();
        let tags = self
            .tags
            .iter()
            .map(|t| t.trim().trim_start_matches('#').trim().to_string())
            .filter(|t| !t.is_empty())
            .collect();
        let mut body = self.body.clone();
        if !body.is_empty() && !body.ends_with('\n') {
            body.push('\n');
        }
        Ok(Block {
            start: self.start,
            end: self.end,
            title: title.trim().to_string(),
            tags,
            body,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blk(start: u16, end: u16, title: &str, tags: &[&str], body: &str) -> Block {
        Block {
            start,
            end,
            title: title.into(),
            tags: tags.iter().map(|t| t.to_string()).collect(),
            body: body.into(),
        }
    }

    fn rt(text: &str) {
        assert_eq!(parse(text).serialize(), text, "round trip of {text:?}");
    }

    // ---- parsing ----

    #[test]
    fn parse_empty_file() {
        let d = parse("");
        assert_eq!(d, DayFile::default());
        assert_eq!(d.serialize(), "");
    }

    #[test]
    fn parse_preamble_only() {
        let d = parse("hello\nworld\n");
        assert_eq!(d.preamble, "hello\nworld\n");
        assert!(d.blocks.is_empty());
    }

    #[test]
    fn parse_single_block() {
        let d = parse("## 09:00–10:30 · Searchlight #headline\n");
        assert_eq!(d.preamble, "");
        assert_eq!(
            d.blocks,
            vec![blk(540, 630, "Searchlight", &["headline"], "")]
        );
    }

    #[test]
    fn parse_blocks_with_bodies() {
        let d = parse("## 09:00–10:00 · A\nnote a\n\n## 14:00–15:00 · B #x #y\nnote b\n");
        assert_eq!(d.blocks.len(), 2);
        assert_eq!(d.blocks[0].body, "note a\n\n");
        assert_eq!(d.blocks[1].tags, vec!["x", "y"]);
        assert_eq!(d.blocks[1].body, "note b\n");
    }

    #[test]
    fn parse_accepts_ascii_hyphen() {
        let d = parse("## 09:00-10:00 · A\n");
        assert_eq!(d.blocks, vec![blk(540, 600, "A", &[], "")]);
    }

    #[test]
    fn parse_end_24_00() {
        let d = parse("## 23:00–24:00 · Late\n");
        assert_eq!(d.blocks, vec![blk(1380, 1440, "Late", &[], "")]);
    }

    #[test]
    fn parse_empty_title() {
        let d = parse("## 09:00–10:00 ·\n");
        assert_eq!(d.blocks, vec![blk(540, 600, "", &[], "")]);
    }

    #[test]
    fn parse_empty_title_with_tags() {
        let d = parse("## 09:00–10:00 · #tag\n");
        assert_eq!(d.blocks, vec![blk(540, 600, "", &["tag"], "")]);
    }

    #[test]
    fn parse_unicode_title_and_tags() {
        let d = parse("## 09:00–10:00 · Reunião café #pós-grad #ação_1\n");
        assert_eq!(
            d.blocks,
            vec![blk(540, 600, "Reunião café", &["pós-grad", "ação_1"], "")]
        );
    }

    #[test]
    fn parse_tags_trailing_only() {
        let d = parse("## 09:00–10:00 · Fix #a thing #b\n");
        assert_eq!(d.blocks[0].title, "Fix #a thing");
        assert_eq!(d.blocks[0].tags, vec!["b"]);
    }

    #[test]
    fn parse_hash_number_mid_title_is_title() {
        let d = parse("## 09:00–10:00 · Fix #123 bug\n");
        assert_eq!(d.blocks[0].title, "Fix #123 bug");
        assert!(d.blocks[0].tags.is_empty());
    }

    #[test]
    fn parse_lone_hash_is_not_tag() {
        let d = parse("## 09:00–10:00 · Title #\n");
        assert_eq!(d.blocks[0].title, "Title #");
        assert!(d.blocks[0].tags.is_empty());
    }

    #[test]
    fn parse_csharp_is_not_tag() {
        let d = parse("## 09:00–10:00 · Learn C#\n");
        assert_eq!(d.blocks[0].title, "Learn C#");
        assert!(d.blocks[0].tags.is_empty());
    }

    // ---- invalid headings stay as text ----

    fn assert_kept_in_preamble(line: &str) {
        let text = format!("{line}\n");
        let d = parse(&text);
        assert!(d.blocks.is_empty(), "{line:?} must not be a heading");
        assert_eq!(d.preamble, text);
        rt(&text);
    }

    #[test]
    fn malformed_single_digit_hour_kept_in_body() {
        let text = "## 09:00–10:00 · A\n## 9:00–10:00 · B\n";
        let d = parse(text);
        assert_eq!(d.blocks.len(), 1);
        assert_eq!(d.blocks[0].body, "## 9:00–10:00 · B\n");
        rt(text);
    }

    #[test]
    fn malformed_bad_hour_kept() {
        assert_kept_in_preamble("## 25:00–26:00 · X");
    }

    #[test]
    fn malformed_bad_minute_kept() {
        assert_kept_in_preamble("## 09:60–10:00 · X");
    }

    #[test]
    fn malformed_start_24_00_kept() {
        assert_kept_in_preamble("## 24:00–24:00 · X");
    }

    #[test]
    fn malformed_end_equals_start_kept() {
        assert_kept_in_preamble("## 09:00–09:00 · X");
    }

    #[test]
    fn malformed_end_before_start_kept() {
        assert_kept_in_preamble("## 10:00–09:00 · X");
    }

    #[test]
    fn malformed_missing_separator_kept() {
        assert_kept_in_preamble("## 09:00–10:00 X");
        assert_kept_in_preamble("## 09:00–10:00");
        assert_kept_in_preamble("## 09:00 – 10:00 · X");
    }

    #[test]
    fn malformed_before_first_block_goes_to_preamble() {
        let text = "intro\n## 9-10 foo\n## 09:00–10:00 · A\n";
        let d = parse(text);
        assert_eq!(d.preamble, "intro\n## 9-10 foo\n");
        assert_eq!(d.blocks.len(), 1);
        rt(text);
    }

    #[test]
    fn malformed_after_block_appended_to_body() {
        let text = "## 09:00–10:00 · A\nnotes\n## 9-10 foo\nmore\n";
        let d = parse(text);
        assert_eq!(d.blocks[0].body, "notes\n## 9-10 foo\nmore\n");
        rt(text);
    }

    #[test]
    fn h1_h3_and_indented_h2_are_not_headings() {
        let text = "# 09:00–10:00 · A\n### 09:00–10:00 · B\n  ## 09:00–10:00 · C\n";
        let d = parse(text);
        assert!(d.blocks.is_empty());
        assert_eq!(d.preamble, text);
    }

    #[test]
    fn two_hashes_without_space_not_heading() {
        assert_kept_in_preamble("##09:00–10:00 · A");
    }

    // ---- fences ----

    #[test]
    fn heading_inside_backtick_fence_is_body() {
        let text = "## 09:00–10:00 · A\n```\n## 10:00–11:00 · Fake\n```\n";
        let d = parse(text);
        assert_eq!(d.blocks.len(), 1);
        assert_eq!(d.blocks[0].body, "```\n## 10:00–11:00 · Fake\n```\n");
        rt(text);
    }

    #[test]
    fn heading_inside_tilde_fence_is_body() {
        let text = "## 09:00–10:00 · A\n~~~\n## 10:00–11:00 · Fake\n~~~\n## 11:00–12:00 · B\n";
        let d = parse(text);
        assert_eq!(d.blocks.len(), 2);
        rt(text);
    }

    #[test]
    fn longer_closing_fence_closes() {
        let text = "## 09:00–10:00 · A\n```\nx\n````\n## 11:00–12:00 · B\n";
        assert_eq!(parse(text).blocks.len(), 2);
    }

    #[test]
    fn mismatched_fence_char_does_not_close() {
        let text = "## 09:00–10:00 · A\n```\n~~~\n## 11:00–12:00 · B\n```\n";
        assert_eq!(parse(text).blocks.len(), 1);
    }

    #[test]
    fn unclosed_fence_swallows_rest() {
        let text = "## 09:00–10:00 · A\n```\n## 11:00–12:00 · B\n";
        let d = parse(text);
        assert_eq!(d.blocks.len(), 1);
        rt(text);
    }

    #[test]
    fn fence_in_preamble_hides_heading() {
        let text = "```\n## 09:00–10:00 · A\n```\n## 10:00–11:00 · B\n";
        let d = parse(text);
        assert_eq!(d.preamble, "```\n## 09:00–10:00 · A\n```\n");
        assert_eq!(d.blocks.len(), 1);
        rt(text);
    }

    // ---- line endings / newlines ----

    #[test]
    fn crlf_roundtrip() {
        rt("pre\r\n## 09:00–10:00 · A #t\r\nnote\r\n\r\n## 11:00–12:00 · B\r\n");
    }

    #[test]
    fn crlf_sets_eol_and_serializes_crlf() {
        let d = parse("## 09:00–10:00 · A\r\nnote\r\n");
        assert_eq!(d.eol, Eol::Crlf);
        assert_eq!(d.blocks[0].body, "note\n");
        assert_eq!(d.serialize(), "## 09:00–10:00 · A\r\nnote\r\n");
    }

    #[test]
    fn mixed_endings_unified_to_crlf() {
        let d = parse("a\r\nb\n## 09:00–10:00 · A\nx\r\n");
        assert_eq!(d.eol, Eol::Crlf);
        assert_eq!(d.serialize(), "a\r\nb\r\n## 09:00–10:00 · A\r\nx\r\n");
    }

    #[test]
    fn lf_file_has_no_cr_after_serialize() {
        let out = parse("a\n## 09:00–10:00 · A\nx\n").serialize();
        assert!(!out.contains('\r'));
    }

    #[test]
    fn no_trailing_newline_roundtrip() {
        rt("## 09:00–10:00 · A\nlast line");
        rt("just text");
    }

    #[test]
    fn no_trailing_newline_last_body_empty_roundtrip() {
        let d = parse("## 09:00–10:00 · A");
        assert!(!d.final_newline);
        assert_eq!(d.blocks[0].body, "");
        assert_eq!(d.serialize(), "## 09:00–10:00 · A");
    }

    #[test]
    fn blank_and_whitespace_lines_preserved() {
        rt("\n\n## 09:00–10:00 · A\n\n   \n\t\n\nx\n\n");
    }

    #[test]
    fn trailing_spaces_in_body_preserved() {
        rt("## 09:00–10:00 · A\nline with spaces   \n  indented\n");
    }

    // ---- round trip ----

    #[test]
    fn roundtrip_claude_md_sample() {
        rt(
            "## 09:00–10:30 · Searchlight #headline\n\nNotes for this block in plain markdown.\n\n## 14:00–15:00 · Deep work #mestrado\n\nMore notes.\n",
        );
    }

    #[test]
    fn roundtrip_preamble_and_blocks() {
        rt("# My day\n\nSome intro.\n\n## 09:00–10:00 · A\nx\n## 10:00–11:00 · B #t\ny\n");
    }

    #[test]
    fn roundtrip_with_malformed_lines() {
        rt("## nope\n## 09:00–10:00 · A\n## 9-10 foo\n## 25:00–26:00 · Z\n");
    }

    #[test]
    fn roundtrip_with_fences() {
        rt("## 09:00–10:00 · A\n```rust\n## 10:00–11:00 · Fake\n```\n~~~\nx\n~~~\n");
    }

    #[test]
    fn roundtrip_empty_bodies() {
        rt("## 09:00–10:00 · A\n## 10:00–11:00 · B\n## 11:00–12:00 ·\n");
    }

    #[test]
    fn reserialize_is_idempotent_for_noncanonical_heading() {
        let once = parse("## 09:00-10:00 ·   Title   #a   #b  \nbody\n");
        let text = once.serialize();
        let twice = parse(&text);
        assert_eq!(once, twice);
        assert_eq!(twice.serialize(), text);
    }

    // ---- serialize / sort / normalise ----

    #[test]
    fn serialize_hyphen_becomes_en_dash() {
        assert_eq!(
            parse("## 09:00-10:00 · A\n").serialize(),
            "## 09:00–10:00 · A\n"
        );
    }

    #[test]
    fn serialize_empty_title_no_trailing_space() {
        let d = DayFile {
            blocks: vec![blk(540, 600, "", &[], "")],
            ..DayFile::default()
        };
        assert_eq!(d.serialize(), "## 09:00–10:00 ·\n");
    }

    #[test]
    fn serialize_tags_prefixed() {
        let d = DayFile {
            blocks: vec![blk(540, 600, "T", &["a", "b"], "")],
            ..DayFile::default()
        };
        assert_eq!(d.serialize(), "## 09:00–10:00 · T #a #b\n");
    }

    #[test]
    fn sort_blocks_by_start_then_end_stable() {
        let mut d = DayFile {
            blocks: vec![
                blk(600, 660, "c", &[], ""),
                blk(540, 700, "b2", &[], ""),
                blk(540, 600, "b1", &[], ""),
                blk(600, 660, "d", &[], ""),
            ],
            ..DayFile::default()
        };
        d.sort_blocks();
        let titles: Vec<_> = d.blocks.iter().map(|b| b.title.as_str()).collect();
        assert_eq!(titles, vec!["b1", "b2", "c", "d"]);
    }

    #[test]
    fn sort_keeps_bodies_attached() {
        let mut d = parse("## 14:00–15:00 · B\nbody b\n## 09:00–10:00 · A\nbody a\n");
        d.sort_blocks();
        assert_eq!(
            d.serialize(),
            "## 09:00–10:00 · A\nbody a\n## 14:00–15:00 · B\nbody b\n"
        );
    }

    #[test]
    fn sort_with_unterminated_last_body_does_not_merge_headings() {
        let mut d = parse("## 14:00–15:00 · B\nbody b\n## 09:00–10:00 · A\nno newline at end");
        d.sort_blocks();
        let out = d.serialize();
        assert_eq!(
            out,
            "## 09:00–10:00 · A\nno newline at end\n## 14:00–15:00 · B\nbody b"
        );
        assert_eq!(parse(&out).blocks.len(), 2);
    }

    #[test]
    fn serialize_does_not_sort() {
        rt("## 14:00–15:00 · B\n## 09:00–10:00 · A\n");
    }

    #[test]
    fn normalized_strips_newlines_and_hash_from_input() {
        let b = blk(540, 600, "  a\nb\r\nc  ", &["#x", " #y ", "#", ""], "note");
        let n = b.normalized().unwrap();
        assert_eq!(n.title, "a b  c");
        assert_eq!(n.tags, vec!["x", "y"]);
        assert_eq!(n.body, "note\n");
    }

    #[test]
    fn normalized_rejects_invalid_times() {
        assert!(blk(600, 600, "", &[], "").normalized().is_err());
        assert!(blk(700, 600, "", &[], "").normalized().is_err());
        assert!(blk(0, 1441, "", &[], "").normalized().is_err());
        assert!(blk(0, 1440, "", &[], "").normalized().is_ok());
    }

    #[test]
    fn overlapping_blocks_preserved() {
        rt("## 09:00–11:00 · A\n## 10:00–12:00 · B\n");
        assert_eq!(
            parse("## 09:00–11:00 · A\n## 10:00–12:00 · B\n")
                .blocks
                .len(),
            2
        );
    }

    // ---- guard_bodies ----

    fn guarded(blocks: Vec<Block>) -> DayFile {
        let mut d = DayFile {
            blocks,
            ..DayFile::default()
        };
        d.guard_bodies();
        d
    }

    fn one(body: &str) -> String {
        guarded(vec![blk(540, 600, "A", &[], body)]).blocks[0]
            .body
            .clone()
    }

    #[test]
    fn guard_escapes_heading_lookalike_in_body() {
        assert_eq!(
            one("x\n## 10:00–11:00 · X\ny\n"),
            "x\n\\## 10:00–11:00 · X\ny\n"
        );
    }

    #[test]
    fn guard_escapes_ascii_hyphen_heading() {
        assert_eq!(one("## 10:00-11:00 · X\n"), "\\## 10:00-11:00 · X\n");
    }

    #[test]
    fn guard_escape_changes_only_that_line() {
        let body = "a\n## 10:00–11:00 · X\n  b  \n\nc";
        let out = one(body);
        assert_eq!(out.replacen('\\', "", 1), body);
    }

    #[test]
    fn guard_escaped_line_does_not_reparse_as_block() {
        let d = guarded(vec![blk(540, 600, "A", &[], "## 10:00–11:00 · X\nmore\n")]);
        let back = parse(&d.serialize());
        assert_eq!(back.blocks.len(), 1);
        assert_eq!(back.blocks[0].body, d.blocks[0].body);
    }

    #[test]
    fn guard_leaves_malformed_heading_alone() {
        let body = "## 9-10 foo\n## 25:00–26:00 · X\n##  10:00–11:00 · X\n";
        assert_eq!(one(body), body);
    }

    #[test]
    fn guard_leaves_heading_inside_backtick_fence() {
        let body = "```\n## 10:00–11:00 · X\n```\n";
        assert_eq!(one(body), body);
    }

    #[test]
    fn guard_leaves_heading_inside_tilde_fence() {
        let body = "~~~\n## 10:00–11:00 · X\n~~~\n";
        assert_eq!(one(body), body);
    }

    #[test]
    fn guard_escapes_heading_after_closed_fence() {
        assert_eq!(
            one("```\ncode\n```\n## 10:00–11:00 · X\n"),
            "```\ncode\n```\n\\## 10:00–11:00 · X\n"
        );
    }

    #[test]
    fn guard_closes_open_fence_on_non_last_block() {
        let d = guarded(vec![
            blk(540, 600, "A", &[], "```\ncode\n"),
            blk(600, 660, "B", &[], "b\n"),
        ]);
        assert_eq!(d.blocks[0].body, "```\ncode\n```\n");
        let back = parse(&d.serialize());
        assert_eq!(back.blocks.len(), 2);
        assert_eq!(back.blocks[1].body, "b\n");
    }

    #[test]
    fn guard_closing_fence_matches_opener_char_and_length() {
        let d = guarded(vec![
            blk(540, 600, "A", &[], "~~~~~ rust\ncode\n"),
            blk(600, 660, "B", &[], ""),
        ]);
        assert_eq!(d.blocks[0].body, "~~~~~ rust\ncode\n~~~~~\n");
    }

    #[test]
    fn guard_leaves_open_fence_on_last_block() {
        let d = guarded(vec![
            blk(540, 600, "A", &[], "a\n"),
            blk(600, 660, "B", &[], "```\ncode\n"),
        ]);
        assert_eq!(d.blocks[1].body, "```\ncode\n");
    }

    #[test]
    fn guard_is_noop_on_parsed_bodies() {
        for text in [
            "intro\n## 09:00–10:00 · A\nnote\n\n## 10:00–11:00 · B #x\n",
            "## 09:00–10:00 · A\n```\n## 10:00–11:00 · Fake\n```\n## 11:00–12:00 · B\n",
            "## 09:00–10:00 · A\n## 9-10 foo\n\\## 10:00–11:00 · X\n## 10:00–11:00 · B\n```\nopen\n",
            "## 09:00-10:00 · A\r\nx\r\n## 10:00–11:00 · B",
        ] {
            let mut d = parse(text);
            let before = d.blocks.clone();
            d.guard_bodies();
            assert_eq!(d.blocks, before, "{text:?}");
            assert_eq!(d.serialize(), parse(text).serialize());
        }
    }

    #[test]
    fn guard_is_idempotent() {
        let mut d = DayFile {
            blocks: vec![
                blk(540, 600, "A", &[], "## 10:00–11:00 · X\n```\nc\n"),
                blk(600, 660, "B", &[], "## 11:00-12:00 · Y\n"),
            ],
            ..DayFile::default()
        };
        d.guard_bodies();
        let once = d.clone();
        d.guard_bodies();
        assert_eq!(d, once);
    }

    #[test]
    fn guard_keeps_crlf_serialization() {
        let mut d = parse("## 09:00–10:00 · A\r\nx\r\n## 10:00–11:00 · B\r\n");
        d.blocks[0].body = "## 10:00–11:00 · X\n```\nc\n".into();
        d.guard_bodies();
        let out = d.serialize();
        assert!(!out.replace("\r\n", "").contains('\n'));
        assert!(out.contains("\\## 10:00–11:00 · X\r\n```\r\nc\r\n```\r\n"));
        assert_eq!(parse(&out).eol, Eol::Crlf);
    }
}
