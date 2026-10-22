//! Timer sessions: the sidecar file `YYYY-MM-DD.sessions.json` next to a day file.
//!
//! The app owns this file: fields it does not know are dropped when it is
//! rewritten. Invalid JSON (or an unknown version) is an error and the file is
//! never overwritten until the user fixes it.

use crate::atomic;
use crate::storage;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

pub const VERSION: u32 = 1;
/// Sessions shorter than this are discarded when stopped (accidental clicks).
pub const MIN_SESSION_SECONDS: i64 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Free,
    Pomodoro,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Free => "free",
            Mode::Pomodoro => "pomodoro",
        }
    }
}

/// Snapshot of the block a session belongs to; `start`/`end` are `"HH:MM"`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionBlock {
    pub start: String,
    pub end: String,
    pub title: String,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Google calendar id of the block's project, when it had one at start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

impl SessionBlock {
    /// This block as the snapshot of a session that used to be `old`: the
    /// project is kept when the new snapshot does not name one.
    pub fn following(&self, old: &SessionBlock) -> SessionBlock {
        SessionBlock {
            project: self.project.clone().or_else(|| old.project.clone()),
            ..self.clone()
        }
    }

    /// Blocks are linked to sessions by time range and trimmed title (tags don't count).
    pub fn same_block(&self, other: &SessionBlock) -> bool {
        self.start == other.start
            && self.end == other.end
            && self.title.trim() == other.title.trim()
    }
}

/// `end` is `None` while running; then `last_seen` is kept fresh by the timer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub start: String,
    pub end: Option<String>,
    pub mode: Mode,
    pub block: SessionBlock,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Sidecar {
    version: u32,
    #[serde(default)]
    sessions: Vec<Session>,
}

/// Parses an RFC 3339 timestamp written by this module.
pub fn parse_ts(s: &str) -> Option<Timestamp> {
    s.parse().ok()
}

/// Whole seconds from `start` to `end`; `None` if either does not parse.
pub fn seconds_between(start: &str, end: &str) -> Option<i64> {
    Some(parse_ts(end)?.as_second() - parse_ts(start)?.as_second())
}

/// Serialises every read-modify-write of any sidecar in this process.
static LOCK: Mutex<()> = Mutex::new(());

fn lock() -> MutexGuard<'static, ()> {
    LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `root/YYYY/MM/YYYY-MM-DD.sessions.json`.
pub fn sidecar_path(root: &Path, date: &str) -> Result<PathBuf, String> {
    let md = storage::day_path(root, date)?;
    Ok(md.with_file_name(format!("{date}.sessions.json")))
}

/// Reads a sidecar by path without taking the write lock (for the index,
/// which only needs a consistent snapshot: writes are atomic renames).
pub fn read_file(path: &Path) -> Result<Vec<Session>, String> {
    read_at(path)
}

fn read_at(path: &Path) -> Result<Vec<Session>, String> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("reading {}: {e}", path.display())),
    };
    let bad = |e: &dyn std::fmt::Display| {
        format!(
            "{} is not valid ({e}); fix or delete it — timer sessions for this day can't be saved until then",
            path.display()
        )
    };
    let sidecar: Sidecar = serde_json::from_slice(&bytes).map_err(|e| bad(&e))?;
    if sidecar.version != VERSION {
        return Err(bad(&format!("unsupported version {}", sidecar.version)));
    }
    Ok(sidecar.sessions)
}

/// That day's sessions in file order; `[]` when there is no sidecar.
pub fn read(root: &Path, date: &str) -> Result<Vec<Session>, String> {
    let _guard = lock();
    read_at(&sidecar_path(root, date)?)
}

/// Applies `f` to the day's sessions and writes the result (sorted by start,
/// atomically) if it changed. Refuses when the existing file is invalid.
pub fn update<R>(
    root: &Path,
    date: &str,
    f: impl FnOnce(&mut Vec<Session>) -> Result<R, String>,
) -> Result<R, String> {
    let _guard = lock();
    let path = sidecar_path(root, date)?;
    let before = read_at(&path)?;
    let mut after = before.clone();
    let result = f(&mut after)?;
    after.sort_by_key(|s| parse_ts(&s.start).map(Timestamp::as_second));
    if after != before {
        let json = serde_json::to_string_pretty(&Sidecar {
            version: VERSION,
            sessions: after,
        })
        .map_err(|e| format!("serializing sessions: {e}"))?;
        atomic::write_atomic(&path, (json + "\n").as_bytes())?;
    }
    Ok(result)
}

/// Inserts `session` or replaces the one with the same `start` (its id).
pub fn upsert(root: &Path, date: &str, session: Session) -> Result<(), String> {
    update(root, date, |all| {
        match all.iter_mut().find(|s| s.start == session.start) {
            Some(slot) => *slot = session,
            None => all.push(session),
        }
        Ok(())
    })
}

/// Removes the session starting at `start` (used for discarded short sessions).
pub fn remove(root: &Path, date: &str, start: &str) -> Result<(), String> {
    update(root, date, |all| {
        all.retain(|s| s.start != start);
        Ok(())
    })
}

/// Deletes a session and returns what is left. The timer service refuses the
/// running one; any other open session is an orphan and may go.
pub fn delete(root: &Path, date: &str, start: &str) -> Result<Vec<Session>, String> {
    update(root, date, |all| {
        all.retain(|s| s.start != start);
        Ok(all.clone())
    })
}

/// Rewrites the block snapshot of every session whose block is `from`.
/// Returns whether anything changed.
pub fn relink(
    root: &Path,
    date: &str,
    links: &[(SessionBlock, SessionBlock)],
) -> Result<bool, String> {
    update(root, date, |all| {
        let mut changed = false;
        for session in all.iter_mut() {
            if let Some((_, to)) = links
                .iter()
                .find(|(from, _)| from.same_block(&session.block))
            {
                let next = to.following(&session.block);
                if session.block != next {
                    session.block = next;
                    changed = true;
                }
            }
        }
        Ok(changed)
    })
}

/// Closes a running session at `end` (RFC 3339). `None` when it lasted less
/// than `MIN_SESSION_SECONDS` and must be discarded.
pub fn finish(session: &Session, end: &str) -> Option<Session> {
    if seconds_between(&session.start, end)? < MIN_SESSION_SECONDS {
        return None;
    }
    Some(Session {
        end: Some(end.to_string()),
        last_seen: None,
        ..session.clone()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_util::TempDir;

    const DATE: &str = "2026-10-20";

    fn blk(start: &str, end: &str, title: &str) -> SessionBlock {
        SessionBlock {
            start: start.into(),
            end: end.into(),
            title: title.into(),
            tags: vec!["t".into()],
            project: None,
        }
    }

    fn s(start: &str, end: Option<&str>) -> Session {
        Session {
            start: start.into(),
            end: end.map(Into::into),
            mode: Mode::Free,
            block: blk("09:00", "10:00", "A"),
            last_seen: None,
        }
    }

    const T0: &str = "2026-10-20T09:00:00-03:00";
    const T1: &str = "2026-10-20T09:30:00-03:00";
    const T2: &str = "2026-10-20T10:00:00-03:00";

    #[test]
    fn missing_sidecar_reads_empty_and_writes_nothing() {
        let tmp = TempDir::new();
        assert!(read(&tmp.0, DATE).unwrap().is_empty());
        update(&tmp.0, DATE, |_| Ok(())).unwrap();
        assert!(!sidecar_path(&tmp.0, DATE).unwrap().exists());
    }

    #[test]
    fn sidecar_lives_next_to_the_day_file() {
        assert_eq!(
            sidecar_path(Path::new("/r"), DATE).unwrap(),
            Path::new("/r/2026/10/2026-10-20.sessions.json")
        );
        assert!(sidecar_path(Path::new("/r"), "../x").is_err());
    }

    #[test]
    fn upsert_writes_pretty_sorted_json() {
        let tmp = TempDir::new();
        upsert(&tmp.0, DATE, s(T2, Some("2026-10-20T10:30:00-03:00"))).unwrap();
        upsert(&tmp.0, DATE, s(T0, Some(T1))).unwrap();
        let text = fs::read_to_string(sidecar_path(&tmp.0, DATE).unwrap()).unwrap();
        assert!(text.contains("\n  \"version\": 1"), "{text}");
        let all = read(&tmp.0, DATE).unwrap();
        assert_eq!(all[0].start, T0);
        assert_eq!(all[1].start, T2);
    }

    #[test]
    fn upsert_replaces_by_start() {
        let tmp = TempDir::new();
        upsert(&tmp.0, DATE, s(T0, None)).unwrap();
        upsert(&tmp.0, DATE, s(T0, Some(T1))).unwrap();
        let all = read(&tmp.0, DATE).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].end.as_deref(), Some(T1));
    }

    #[test]
    fn serialized_shape_matches_the_plan() {
        let mut running = s(T0, None);
        running.last_seen = Some(T1.into());
        running.mode = Mode::Pomodoro;
        assert_eq!(
            serde_json::to_value(&running).unwrap(),
            serde_json::json!({
                "start": T0, "end": null, "mode": "pomodoro", "lastSeen": T1,
                "block": {"start": "09:00", "end": "10:00", "title": "A", "tags": ["t"]}
            })
        );
        let done = s(T0, Some(T1));
        let v = serde_json::to_value(&done).unwrap();
        assert!(v.get("lastSeen").is_none());
        assert_eq!(v["end"], T1);
    }

    #[test]
    fn unknown_fields_are_dropped_on_rewrite() {
        let tmp = TempDir::new();
        let p = sidecar_path(&tmp.0, DATE).unwrap();
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(
            &p,
            format!(
                r#"{{"version":1,"extra":1,"sessions":[{{"start":"{T0}","end":"{T1}","mode":"free","note":"x","block":{{"start":"09:00","end":"10:00","title":"A","tags":[]}}}}]}}"#
            ),
        )
        .unwrap();
        upsert(&tmp.0, DATE, s(T2, Some("2026-10-20T10:30:00-03:00"))).unwrap();
        let text = fs::read_to_string(&p).unwrap();
        assert!(!text.contains("extra") && !text.contains("\"note\""));
        assert_eq!(read(&tmp.0, DATE).unwrap().len(), 2);
    }

    #[test]
    fn invalid_json_is_an_error_and_never_overwritten() {
        let tmp = TempDir::new();
        let p = sidecar_path(&tmp.0, DATE).unwrap();
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, "{ nope").unwrap();
        assert!(read(&tmp.0, DATE).is_err());
        assert!(upsert(&tmp.0, DATE, s(T0, None)).is_err());
        assert!(delete(&tmp.0, DATE, T0).is_err());
        assert!(relink(&tmp.0, DATE, &[]).is_err());
        assert_eq!(fs::read_to_string(&p).unwrap(), "{ nope");
    }

    #[test]
    fn unsupported_version_refuses_writes() {
        let tmp = TempDir::new();
        let p = sidecar_path(&tmp.0, DATE).unwrap();
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, r#"{"version":2,"sessions":[]}"#).unwrap();
        assert!(upsert(&tmp.0, DATE, s(T0, None)).is_err());
        assert_eq!(
            fs::read_to_string(&p).unwrap(),
            r#"{"version":2,"sessions":[]}"#
        );
    }

    #[test]
    fn delete_removes_finished_and_open_sessions() {
        let tmp = TempDir::new();
        upsert(&tmp.0, DATE, s(T0, Some(T1))).unwrap();
        upsert(&tmp.0, DATE, s(T2, None)).unwrap();
        let left = delete(&tmp.0, DATE, T0).unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].start, T2);
        assert_eq!(
            delete(&tmp.0, DATE, "2026-10-20T11:00:00-03:00")
                .unwrap()
                .len(),
            1
        ); // Open sessions are deletable here; the timer service guards the running one.
        assert!(delete(&tmp.0, DATE, T2).unwrap().is_empty());
    }

    #[test]
    fn remove_drops_by_start() {
        let tmp = TempDir::new();
        upsert(&tmp.0, DATE, s(T0, None)).unwrap();
        remove(&tmp.0, DATE, T0).unwrap();
        assert!(read(&tmp.0, DATE).unwrap().is_empty());
    }

    #[test]
    fn relink_rewrites_matching_snapshots_only() {
        let tmp = TempDir::new();
        upsert(&tmp.0, DATE, s(T0, Some(T1))).unwrap();
        let mut other = s(T2, Some("2026-10-20T10:30:00-03:00"));
        other.block = blk("11:00", "12:00", "B");
        upsert(&tmp.0, DATE, other.clone()).unwrap();
        let to = blk("09:30", "10:30", "A2");
        let changed = relink(&tmp.0, DATE, &[(blk("09:00", "10:00", "A"), to.clone())]).unwrap();
        assert!(changed);
        let all = read(&tmp.0, DATE).unwrap();
        assert_eq!(all[0].block, to);
        assert_eq!(all[1], other);
    }

    #[test]
    fn relink_without_match_is_a_noop_and_does_not_write() {
        let tmp = TempDir::new();
        assert!(
            !relink(
                &tmp.0,
                DATE,
                &[(blk("1:00", "2:00", "Z"), blk("3:00", "4:00", "Z"))]
            )
            .unwrap()
        );
        assert!(!sidecar_path(&tmp.0, DATE).unwrap().exists());
    }

    #[test]
    fn finish_discards_sessions_under_a_minute() {
        let run = s(T0, None);
        assert_eq!(finish(&run, "2026-10-20T09:00:59-03:00"), None);
        assert_eq!(finish(&run, T0), None);
    }

    #[test]
    fn finish_keeps_a_minute_or_more() {
        let run = s(T0, None);
        let done = finish(&run, "2026-10-20T09:01:00-03:00").unwrap();
        assert_eq!(done.end.as_deref(), Some("2026-10-20T09:01:00-03:00"));
        assert_eq!(done.last_seen, None);
        assert_eq!(done.start, T0);
    }

    #[test]
    fn no_temp_files_left_behind() {
        let tmp = TempDir::new();
        upsert(&tmp.0, DATE, s(T0, None)).unwrap();
        let dir = sidecar_path(&tmp.0, DATE).unwrap();
        let names: Vec<_> = fs::read_dir(dir.parent().unwrap())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["2026-10-20.sessions.json"]);
    }

    #[test]
    fn relink_keeps_the_project_unless_the_new_snapshot_names_one() {
        let tmp = TempDir::new();
        let mut old = blk("09:00", "10:00", "A");
        old.project = Some("mest@x".into());
        upsert(
            &tmp.0,
            DATE,
            Session {
                start: "2026-10-20T09:00:00-03:00".into(),
                end: Some("2026-10-20T09:30:00-03:00".into()),
                mode: Mode::Free,
                block: old.clone(),
                last_seen: None,
            },
        )
        .unwrap();
        // from/to arrive without a project (null): the session keeps its own
        let moved = blk("10:00", "11:00", "A2");
        assert!(relink(&tmp.0, DATE, &[(blk("09:00", "10:00", "A"), moved.clone())]).unwrap());
        let got = read(&tmp.0, DATE).unwrap();
        assert_eq!(got[0].block.title, "A2");
        assert_eq!(got[0].block.project.as_deref(), Some("mest@x"));
        // a snapshot that names a project replaces it
        let mut elsewhere = moved.clone();
        elsewhere.project = Some("work@x".into());
        assert!(relink(&tmp.0, DATE, &[(moved, elsewhere)]).unwrap());
        assert_eq!(
            read(&tmp.0, DATE).unwrap()[0].block.project.as_deref(),
            Some("work@x")
        );
    }

    #[test]
    fn project_is_omitted_from_the_sidecar_when_absent_and_read_back_when_present() {
        let tmp = TempDir::new();
        let mk = |start: &str, project: Option<&str>| {
            let mut block = blk("09:00", "10:00", "A");
            block.project = project.map(str::to_string);
            Session {
                start: start.into(),
                end: None,
                mode: Mode::Free,
                block,
                last_seen: None,
            }
        };
        upsert(&tmp.0, DATE, mk("2026-10-20T09:00:00-03:00", None)).unwrap();
        upsert(
            &tmp.0,
            DATE,
            mk("2026-10-20T11:00:00-03:00", Some("mest@x")),
        )
        .unwrap();
        let text = fs::read_to_string(sidecar_path(&tmp.0, DATE).unwrap()).unwrap();
        assert_eq!(text.matches("\"project\"").count(), 1, "{text}");
        let got = read(&tmp.0, DATE).unwrap();
        assert_eq!(got[0].block.project, None);
        assert_eq!(got[1].block.project.as_deref(), Some("mest@x"));
    }
}
