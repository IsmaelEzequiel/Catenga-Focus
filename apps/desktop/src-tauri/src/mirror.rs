//! The read-only copy of finished timer sessions at the end of a block's note:
//! `⏱ HH:MM–HH:MM · N min · mode`. Pure string functions; see `storage` for use.
//!
//! Only a trailing group of such lines (optionally preceded by one blank line)
//! is mirror; a `⏱` line in the middle of user text is user text.

use crate::sessions::{Session, SessionBlock, parse_ts, seconds_between};

fn is_hhmm(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 5 && b[2] == b':' && [0, 1, 3, 4].iter().all(|&i| b[i].is_ascii_digit())
}

/// `⏱ HH:MM–HH:MM · N min · (free|pomodoro)` (accepts `-` for the dash), no newline.
fn is_mirror_line(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("⏱ ") else {
        return false;
    };
    let Some((from, rest)) = rest.split_at_checked(5) else {
        return false;
    };
    let Some(rest) = rest.strip_prefix('–').or_else(|| rest.strip_prefix('-')) else {
        return false;
    };
    let Some((to, rest)) = rest.split_at_checked(5) else {
        return false;
    };
    let Some(rest) = rest.strip_prefix(" · ") else {
        return false;
    };
    let Some((minutes, mode)) = rest.split_once(" min · ") else {
        return false;
    };
    is_hhmm(from)
        && is_hhmm(to)
        && !minutes.is_empty()
        && minutes.bytes().all(|b| b.is_ascii_digit())
        && (mode == "free" || mode == "pomodoro")
}

/// Splits a block body into `(user text, mirror lines)`. Bodies are LF-only
/// (the day-file parser normalises CRLF).
pub fn split(body: &str) -> (&str, Vec<&str>) {
    let lines: Vec<&str> = body.split_inclusive('\n').collect();
    let mut first = lines.len();
    while first > 0 && is_mirror_line(lines[first - 1].trim_end_matches('\n')) {
        first -= 1;
    }
    if first == lines.len() {
        return (body, Vec::new());
    }
    let mirror = lines[first..]
        .iter()
        .map(|l| l.trim_end_matches('\n'))
        .collect();
    if first > 0 && lines[first - 1].trim().is_empty() {
        first -= 1;
    }
    let user_len: usize = lines[..first].iter().map(|l| l.len()).sum();
    (&body[..user_len], mirror)
}

/// The body without its trailing mirror group.
pub fn strip(body: &str) -> String {
    split(body).0.to_string()
}

/// `body` followed by `lines` (one blank line between them when the body has text).
pub fn append(body: &str, lines: &[String]) -> String {
    if lines.is_empty() {
        return body.to_string();
    }
    let mut out = body.to_string();
    if !out.is_empty() {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }
    for line in lines {
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// `HH:MM` of an RFC 3339 timestamp in the offset it was written with.
fn local_hhmm(ts: &str) -> Option<&str> {
    parse_ts(ts)?;
    ts.get(11..16)
}

/// One mirror line per finished session of `block`, in session order.
pub fn lines_for(block: &SessionBlock, sessions: &[Session]) -> Vec<String> {
    sessions
        .iter()
        .filter(|s| s.block.same_block(block))
        .filter_map(|s| {
            let end = s.end.as_deref()?;
            let minutes = seconds_between(&s.start, end)?.max(0) / 60;
            Some(format!(
                "⏱ {}–{} · {minutes} min · {}",
                local_hhmm(&s.start)?,
                local_hhmm(end)?,
                s.mode.as_str()
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::Mode;

    const L1: &str = "⏱ 09:05–09:50 · 45 min · pomodoro";
    const L2: &str = "⏱ 10:00–10:25 · 25 min · free";

    fn lines(l: &[&str]) -> Vec<String> {
        l.iter().map(|s| (*s).to_string()).collect()
    }

    fn blk(start: &str, end: &str, title: &str) -> SessionBlock {
        SessionBlock {
            start: start.into(),
            end: end.into(),
            title: title.into(),
            tags: vec![],
            project: None,
        }
    }

    fn sess(start: &str, end: Option<&str>, mode: Mode, block: SessionBlock) -> Session {
        Session {
            start: start.into(),
            end: end.map(Into::into),
            mode,
            block,
            last_seen: None,
        }
    }

    #[test]
    fn append_then_strip_is_identity() {
        for body in [
            "",
            "\n",
            "Read.\n",
            "Read.\n\n",
            "a\n\nb\n",
            "\n\n\n",
            "x\n\n\n\n",
        ] {
            let out = append(body, &lines(&[L1, L2]));
            assert_eq!(strip(&out), body, "{body:?}");
        }
    }

    #[test]
    fn append_layout() {
        assert_eq!(append("Read.\n", &lines(&[L1])), format!("Read.\n\n{L1}\n"));
        assert_eq!(append("", &lines(&[L1])), format!("{L1}\n"));
        assert_eq!(append("Read.\n", &[]), "Read.\n");
    }

    #[test]
    fn strip_without_mirror_is_unchanged() {
        for body in ["", "text\n", "text\n\n", "\n"] {
            assert_eq!(strip(body), body);
        }
    }

    #[test]
    fn strip_keeps_user_lines_in_the_middle() {
        let body = format!("{L1}\nmore text\n");
        assert_eq!(strip(&body), body);
        let body = format!("a\n{L1}\nb\n{L2}\n");
        assert_eq!(strip(&body), format!("a\n{L1}\nb\n"));
    }

    #[test]
    fn strip_keeps_malformed_mirror_lookalikes() {
        for bad in [
            "⏱ 9:05–09:50 · 45 min · pomodoro",
            "⏱ 09:05–09:50 · 45 min · other",
            "⏱ 09:05–09:50 · 45 minutes · free",
            "⏱ 09:05–09:50 · min · free",
            "⏱ 09:05–09:50 · 45 min · free extra",
            " ⏱ 09:05–09:50 · 45 min · free",
            "⏱ 09:05 09:50 · 45 min · free",
        ] {
            let body = format!("note\n\n{bad}\n");
            assert_eq!(strip(&body), body, "{bad}");
        }
    }

    #[test]
    fn strip_accepts_ascii_hyphen() {
        let body = "note\n\n⏱ 09:05-09:50 · 45 min · free\n";
        assert_eq!(strip(body), "note\n");
    }

    #[test]
    fn strip_removes_only_one_blank_line_before_group() {
        let body = format!("note\n\n\n{L1}\n");
        assert_eq!(strip(&body), "note\n\n");
    }

    #[test]
    fn strip_of_only_mirror_body_is_empty() {
        assert_eq!(strip(&format!("{L1}\n{L2}\n")), "");
    }

    #[test]
    fn strip_works_on_crlf_file_bodies() {
        let file = crate::day_file::parse(&format!("## 09:00–10:00 · A\r\nnote\r\n\r\n{L1}\r\n"));
        assert_eq!(strip(&file.blocks[0].body), "note\n");
    }

    #[test]
    fn split_returns_lines() {
        let body = format!("note\n\n{L1}\n{L2}\n");
        let (user, mirror) = split(&body);
        assert_eq!(user, "note\n");
        assert_eq!(mirror, vec![L1, L2]);
    }

    #[test]
    fn lines_for_formats_finished_sessions_of_the_block() {
        let b = blk("09:00", "10:30", "Deep work");
        let all = [
            sess(
                "2026-10-20T09:05:12-03:00",
                Some("2026-10-20T09:50:12-03:00"),
                Mode::Pomodoro,
                b.clone(),
            ),
            sess(
                "2026-10-20T10:00:00-03:00",
                Some("2026-10-20T10:25:59-03:00"),
                Mode::Free,
                b.clone(),
            ),
        ];
        assert_eq!(lines_for(&b, &all), lines(&[L1, L2]));
    }

    #[test]
    fn lines_for_skips_running_and_other_blocks() {
        let b = blk("09:00", "10:30", "Deep work");
        let other = blk("11:00", "12:00", "Deep work");
        let all = [
            sess("2026-10-20T09:05:12-03:00", None, Mode::Free, b.clone()),
            sess(
                "2026-10-20T11:00:00-03:00",
                Some("2026-10-20T11:30:00-03:00"),
                Mode::Free,
                other,
            ),
        ];
        assert!(lines_for(&b, &all).is_empty());
    }

    #[test]
    fn lines_for_matches_trimmed_title_ignoring_tags() {
        let mut b = blk("09:00", "10:30", "Deep work");
        b.tags = vec!["x".into()];
        let s = sess(
            "2026-10-20T09:05:00-03:00",
            Some("2026-10-20T09:50:00-03:00"),
            Mode::Free,
            blk("09:00", "10:30", "  Deep work "),
        );
        assert_eq!(lines_for(&b, &[s]).len(), 1);
    }

    #[test]
    fn lines_for_uses_the_recorded_offset_for_local_times() {
        let b = blk("09:00", "10:30", "A");
        let s = sess(
            "2026-10-20T09:05:00+01:00",
            Some("2026-10-20T09:35:00+01:00"),
            Mode::Free,
            b.clone(),
        );
        assert_eq!(
            lines_for(&b, &[s]),
            lines(&["⏱ 09:05–09:35 · 30 min · free"])
        );
    }
}
