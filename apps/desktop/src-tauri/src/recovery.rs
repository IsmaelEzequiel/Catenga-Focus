//! Durable copies of notes whose save failed (see `docs/plans/save-recovery.md`).
//!
//! One file per day in the recovery folder (app data dir, never the notes folder):
//! `YYYY-MM-DD.json` = the full snapshot the failed save tried to write, plus the
//! revision it was based on. An unreadable file is listed with an error and never
//! deleted or overwritten in place.

use crate::atomic;
use crate::day_file::Block;
use crate::storage::validate_date;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use tauri::State;

/// The folder holding the recovery files.
pub struct RecoveryDir(pub PathBuf);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Stored {
    date: String,
    base_rev: String,
    /// Unix milliseconds.
    saved_at: u64,
    blocks: Vec<Block>,
}

/// A recovery entry; `error` is set (and the rest empty) for a file that could not be read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Recovery {
    pub date: String,
    pub base_rev: String,
    pub saved_at: u64,
    pub blocks: Vec<Block>,
    pub error: Option<String>,
}

fn file_for(dir: &Path, date: &str) -> Result<PathBuf, String> {
    validate_date(date)?;
    Ok(dir.join(format!("{date}.json")))
}

fn read_entry(path: &Path, date: &str) -> Result<Stored, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let stored: Stored = serde_json::from_str(&text)
        .map_err(|e| format!("{} is not a valid recovery file: {e}", path.display()))?;
    if stored.date != date {
        return Err(format!(
            "{} is for {}, not {date}",
            path.display(),
            stored.date
        ));
    }
    Ok(stored)
}

/// Writes (or replaces) the recovery file of `date`. A file that cannot be read is set
/// aside under another name first, never overwritten.
pub fn put(
    dir: &Path,
    date: &str,
    blocks: &[Block],
    base_rev: &str,
    now_ms: u64,
) -> Result<(), String> {
    let path = file_for(dir, date)?;
    if path.exists() && read_entry(&path, date).is_err() {
        let aside = dir.join(format!("{date}.json.unreadable-{now_ms}"));
        fs::rename(&path, &aside)
            .map_err(|e| format!("keeping unreadable {}: {e}", path.display()))?;
    }
    let stored = Stored {
        date: date.to_string(),
        base_rev: base_rev.to_string(),
        saved_at: now_ms,
        blocks: blocks.to_vec(),
    };
    let json = serde_json::to_string_pretty(&stored).map_err(|e| e.to_string())?;
    atomic::write_atomic(&path, (json + "\n").as_bytes())
}

/// Every recovery entry, ordered by date. A missing folder means none.
pub fn list(dir: &Path) -> Result<Vec<Recovery>, String> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("reading {}: {e}", dir.display())),
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(date) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".json"))
            .filter(|d| validate_date(d).is_ok())
            .map(str::to_string)
        else {
            continue;
        };
        out.push(match read_entry(&path, &date) {
            Ok(s) => Recovery {
                date,
                base_rev: s.base_rev,
                saved_at: s.saved_at,
                blocks: s.blocks,
                error: None,
            },
            Err(error) => Recovery {
                date,
                base_rev: String::new(),
                saved_at: 0,
                blocks: Vec::new(),
                error: Some(error),
            },
        });
    }
    out.sort_by(|a, b| a.date.cmp(&b.date));
    Ok(out)
}

/// Deletes the recovery file of `date` (no error when there is none).
pub fn remove(dir: &Path, date: &str) -> Result<(), String> {
    let path = file_for(dir, date)?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("removing {}: {e}", path.display())),
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

#[tauri::command]
pub fn recovery_put(
    dir: State<RecoveryDir>,
    date: String,
    blocks: Vec<Block>,
    base_rev: String,
) -> Result<(), String> {
    put(&dir.0, &date, &blocks, &base_rev, now_ms())
}

#[tauri::command]
pub fn recovery_list(dir: State<RecoveryDir>) -> Result<Vec<Recovery>, String> {
    list(&dir.0)
}

#[tauri::command]
pub fn recovery_drop(dir: State<RecoveryDir>, date: String) -> Result<(), String> {
    remove(&dir.0, &date)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_util::TempDir;

    fn block(title: &str, body: &str) -> Block {
        Block {
            start: 540,
            end: 600,
            title: title.into(),
            tags: vec!["t".into()],
            body: body.into(),
        }
    }

    #[test]
    fn put_list_drop_round_trip() {
        let tmp = TempDir::new();
        let dir = tmp.0.join("recovery");
        assert!(list(&dir).unwrap().is_empty());
        put(&dir, "2026-10-21", &[block("A", "text\n")], "rev1", 1000).unwrap();
        let got = list(&dir).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].date, "2026-10-21");
        assert_eq!(got[0].base_rev, "rev1");
        assert_eq!(got[0].saved_at, 1000);
        assert_eq!(got[0].blocks, vec![block("A", "text\n")]);
        assert_eq!(got[0].error, None);
        remove(&dir, "2026-10-21").unwrap();
        assert!(list(&dir).unwrap().is_empty());
        remove(&dir, "2026-10-21").unwrap();
    }

    #[test]
    fn overwrite_keeps_the_newest() {
        let tmp = TempDir::new();
        put(&tmp.0, "2026-10-21", &[block("A", "one\n")], "r1", 1).unwrap();
        put(&tmp.0, "2026-10-21", &[block("A", "two\n")], "r2", 2).unwrap();
        let got = list(&tmp.0).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].blocks[0].body, "two\n");
        assert_eq!(got[0].base_rev, "r2");
    }

    #[test]
    fn corrupt_file_is_listed_with_an_error_and_not_deleted() {
        let tmp = TempDir::new();
        let path = tmp.0.join("2026-10-20.json");
        fs::write(&path, "{ not json").unwrap();
        put(&tmp.0, "2026-10-21", &[block("A", "x\n")], "r", 1).unwrap();
        let got = list(&tmp.0).unwrap();
        assert_eq!(got.len(), 2);
        assert!(got[0].error.is_some());
        assert!(got[0].blocks.is_empty());
        assert_eq!(got[1].error, None);
        assert_eq!(fs::read_to_string(&path).unwrap(), "{ not json");
    }

    #[test]
    fn put_over_a_corrupt_file_sets_it_aside() {
        let tmp = TempDir::new();
        let path = tmp.0.join("2026-10-21.json");
        fs::write(&path, "garbage").unwrap();
        put(&tmp.0, "2026-10-21", &[block("A", "x\n")], "r", 7).unwrap();
        assert_eq!(
            fs::read_to_string(tmp.0.join("2026-10-21.json.unreadable-7")).unwrap(),
            "garbage"
        );
        assert_eq!(list(&tmp.0).unwrap()[0].error, None);
    }

    #[test]
    fn file_for_another_date_is_an_error_entry() {
        let tmp = TempDir::new();
        put(&tmp.0, "2026-10-21", &[], "r", 1).unwrap();
        fs::rename(tmp.0.join("2026-10-21.json"), tmp.0.join("2026-10-22.json")).unwrap();
        assert!(list(&tmp.0).unwrap()[0].error.is_some());
    }

    #[test]
    fn rejects_bad_dates_and_ignores_other_files() {
        let tmp = TempDir::new();
        assert!(put(&tmp.0, "../x", &[], "r", 1).is_err());
        assert!(remove(&tmp.0, "nope").is_err());
        fs::write(tmp.0.join("notes.txt"), "x").unwrap();
        fs::write(tmp.0.join("bad.json"), "x").unwrap();
        assert!(list(&tmp.0).unwrap().is_empty());
    }
}
