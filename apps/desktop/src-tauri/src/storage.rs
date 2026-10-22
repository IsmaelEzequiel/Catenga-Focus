//! Day-file storage under the notes folder. Pure over `&Path`, no Tauri.

use crate::atomic;
use crate::day_file::{self, Block, DayFile};
use crate::mirror;
use crate::notes_crypto;
use crate::sessions::{self, Session, SessionBlock};
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::hash::{DefaultHasher, Hasher};
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

/// Validates `YYYY-MM-DD` and returns `(year, month, day)`.
pub fn validate_date(date: &str) -> Result<(u16, u8, u8), String> {
    let err = || format!("invalid date {date:?}, expected YYYY-MM-DD");
    let b = date.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return Err(err());
    }
    let digits = |r: std::ops::Range<usize>| -> Option<u32> {
        let s = &date[r];
        s.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| s.parse().ok())
            .flatten()
    };
    let (y, m, d) = match (digits(0..4), digits(5..7), digits(8..10)) {
        (Some(y), Some(m), Some(d)) => (y as u16, m as u8, d as u8),
        _ => return Err(err()),
    };
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let days = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return Err(err()),
    };
    if d == 0 || d > days {
        return Err(err());
    }
    Ok((y, m, d))
}

pub fn day_path(root: &Path, date: &str) -> Result<PathBuf, String> {
    validate_date(date)?;
    Ok(root
        .join(&date[0..4])
        .join(&date[5..7])
        .join(format!("{date}.md")))
}

/// Short hex hash of the raw file bytes; `""` means "file does not exist".
/// Computed from content (never mtime) and only compared within one process run.
pub fn rev_of(bytes: &[u8]) -> String {
    let mut h = DefaultHasher::new();
    h.write(bytes);
    format!("{:016x}", h.finish())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadedDay {
    pub blocks: Vec<Block>,
    pub rev: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum SaveOutcome {
    Saved { rev: String },
    Conflict { blocks: Vec<Block>, rev: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DayChanged {
    pub date: String,
    pub rev: String,
}

/// The parsed file and its rev; `None` when the file does not exist.
fn read_existing(path: &Path) -> Result<Option<(DayFile, String)>, String> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("reading {}: {e}", path.display())),
    };
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| format!("reading {}: file is not valid UTF-8", path.display()))?;
    Ok(Some((day_file::parse(text), rev_of(&bytes))))
}

/// Blocks as the frontend sees them: timer mirror lines removed from the bodies.
fn stripped(mut blocks: Vec<Block>) -> Vec<Block> {
    for b in &mut blocks {
        b.body = mirror::strip(&b.body);
    }
    blocks
}

fn hhmm(minutes: u16) -> String {
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

/// The block as sessions refer to it.
fn session_block(b: &Block) -> SessionBlock {
    SessionBlock {
        start: hhmm(b.start),
        end: hhmm(b.end),
        title: b.title.trim().to_string(),
        tags: b.tags.clone(),
        project: None,
    }
}

/// Where the mirror lines of a write come from: the sessions, or (when the
/// sidecar is invalid and so unreadable) whatever the file already has.
enum MirrorSource {
    Sessions(Vec<Session>),
    Keep(Vec<(SessionBlock, Vec<String>)>),
}

impl MirrorSource {
    fn load(root: &Path, date: &str, existing: Option<&DayFile>) -> MirrorSource {
        match sessions::read(root, date) {
            Ok(all) => MirrorSource::Sessions(all),
            Err(e) => {
                eprintln!("timer sessions unavailable, keeping mirror lines as they are: {e}");
                let kept = existing
                    .map(|f| {
                        f.blocks
                            .iter()
                            .map(|b| {
                                let lines = mirror::split(&b.body).1;
                                (
                                    session_block(b),
                                    lines.iter().map(|l| (*l).to_string()).collect(),
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                MirrorSource::Keep(kept)
            }
        }
    }

    fn lines_for(&self, block: &Block) -> Vec<String> {
        let key = session_block(block);
        match self {
            MirrorSource::Sessions(all) => mirror::lines_for(&key, all),
            MirrorSource::Keep(kept) => kept
                .iter()
                .find(|(k, _)| k.same_block(&key))
                .map(|(_, lines)| lines.clone())
                .unwrap_or_default(),
        }
    }

    /// Replaces every block's trailing mirror group with the current lines.
    fn apply(&self, blocks: &mut [Block]) {
        for b in blocks {
            let lines = self.lines_for(b);
            b.body = mirror::append(&mirror::strip(&b.body), &lines);
        }
    }
}

/// Blocks in file order plus the file's rev. A missing file is an empty day with rev `""`.
pub fn read_day(root: &Path, date: &str) -> Result<LoadedDay, String> {
    let path = day_path(root, date)?;
    Ok(match read_existing(&path)? {
        Some((file, rev)) => LoadedDay {
            blocks: stripped(file.blocks),
            rev,
        },
        None => LoadedDay {
            blocks: Vec::new(),
            rev: String::new(),
        },
    })
}

/// Replaces the blocks of the day file, keeping its preamble and line endings.
/// If the file's rev is not `base_rev` (edited outside the app since the last
/// load/save) nothing is written and the on-disk state is returned instead.
///
/// Callers serialise saves (see `DayStore`). An external write landing between
/// the read below and the rename is overwritten; the atomic write at least
/// guarantees readers never see a torn file.
pub fn save_day(
    root: &Path,
    date: &str,
    blocks: &[Block],
    base_rev: &str,
) -> Result<SaveOutcome, String> {
    let path = day_path(root, date)?;
    let blocks = blocks
        .iter()
        .map(Block::normalized)
        .collect::<Result<Vec<_>, _>>()?;
    let existing = read_existing(&path)?;
    let cur_rev = existing
        .as_ref()
        .map(|(_, rev)| rev.clone())
        .unwrap_or_default();
    if cur_rev != base_rev {
        return Ok(SaveOutcome::Conflict {
            blocks: stripped(existing.map(|(f, _)| f.blocks).unwrap_or_default()),
            rev: cur_rev,
        });
    }
    if blocks.is_empty() && existing.is_none() {
        return Ok(SaveOutcome::Saved { rev: cur_rev });
    }
    let mut file = existing.map(|(f, _)| f).unwrap_or_default();
    let source = MirrorSource::load(root, date, Some(&file));
    file.blocks = blocks;
    file.sort_blocks();
    file.guard_bodies();
    source.apply(&mut file.blocks);
    file.final_newline = true;
    let bytes = file.serialize().into_bytes();
    atomic::write_atomic(&path, &bytes)?;
    Ok(SaveOutcome::Saved {
        rev: rev_of(&bytes),
    })
}

/// Rewrites only the mirror lines of the day file from the sidecar, keeping
/// everything else as parsed. Returns `(old_rev, new_rev)`, or `None` when the
/// file is missing or already up to date. Errors (and writes nothing) when the
/// sidecar is invalid.
fn rewrite_mirror_file(root: &Path, date: &str) -> Result<Option<(String, String)>, String> {
    let path = day_path(root, date)?;
    let Some((mut file, old_rev)) = read_existing(&path)? else {
        return Ok(None);
    };
    let source = MirrorSource::Sessions(sessions::read(root, date)?);
    let before = file.blocks.clone();
    source.apply(&mut file.blocks);
    if file.blocks == before {
        // Nothing to mirror: leave the file byte for byte as it is.
        return Ok(None);
    }
    let bytes = file.serialize().into_bytes();
    let new_rev = rev_of(&bytes);
    if new_rev == old_rev {
        return Ok(None);
    }
    atomic::write_atomic(&path, &bytes)?;
    Ok(Some((old_rev, new_rev)))
}

/// `YYYY-MM-DD` of a day file path `root/YYYY/MM/YYYY-MM-DD.md`; `None` for
/// anything else (config, temp files, editor swap/backup files, other depths).
/// `root` must be canonical so it is a prefix of watcher-reported paths.
pub fn date_from_path(root: &Path, path: &Path) -> Option<String> {
    date_from_path_suffix(root, path, ".md")
}

/// Like `date_from_path` for a timer sidecar `root/YYYY/MM/YYYY-MM-DD.sessions.json`.
pub fn sidecar_date_from_path(root: &Path, path: &Path) -> Option<String> {
    date_from_path_suffix(root, path, ".sessions.json")
}

fn date_from_path_suffix(root: &Path, path: &Path, suffix: &str) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let parts = rel
        .components()
        .map(|c| match c {
            Component::Normal(s) => s.to_str(),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    let [year, month, file] = parts[..] else {
        return None;
    };
    let date = file.strip_suffix(suffix)?;
    validate_date(date).ok()?;
    (year == &date[0..4] && month == &date[5..7]).then(|| date.to_string())
}

/// What `DayStore::replace_synced` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Replaced {
    /// Done; `Some` when the frontend must be told the day changed.
    Done(Option<DayChanged>),
    /// The file is not what the caller expected (edited meanwhile): nothing was touched.
    Changed,
}

/// Serialises all reads/writes of day files and remembers the last rev the
/// frontend was told about per day, so watcher events caused by our own writes
/// (or no-op touches) can be told apart from foreign edits.
pub struct DayStore {
    root: PathBuf,
    state: Mutex<State>,
}

/// Revs the frontend may still hold for a day after the app rewrote only its
/// mirror lines: saves based on any of `olds` are accepted while the file is at `target`.
struct Alias {
    target: String,
    olds: Vec<String>,
}

const MAX_ALIAS_REVS: usize = 16;

#[derive(Default)]
struct State {
    known: HashMap<String, String>,
    aliases: HashMap<String, Alias>,
}

impl DayStore {
    pub fn new(root: PathBuf) -> DayStore {
        DayStore {
            root,
            state: Mutex::new(State::default()),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn load(&self, date: &str) -> Result<LoadedDay, String> {
        let mut state = self.state();
        let loaded = read_day(&self.root, date)?;
        state.known.insert(date.to_string(), loaded.rev.clone());
        Ok(loaded)
    }

    pub fn save(
        &self,
        date: &str,
        blocks: &[Block],
        base_rev: &str,
    ) -> Result<SaveOutcome, String> {
        let mut state = self.state();
        let base_rev = self.effective_base(&state, date, base_rev)?;
        let outcome = save_day(&self.root, date, blocks, &base_rev)?;
        let rev = match &outcome {
            SaveOutcome::Saved { rev } => {
                state.aliases.remove(date);
                rev.clone()
            }
            SaveOutcome::Conflict { rev, .. } => rev.clone(),
        };
        state.known.insert(date.to_string(), rev);
        Ok(outcome)
    }

    /// `base_rev`, or the file's current rev when `base_rev` is one the app
    /// itself moved the file away from by rewriting only its mirror lines.
    fn effective_base(&self, state: &State, date: &str, base_rev: &str) -> Result<String, String> {
        let Some(alias) = state.aliases.get(date) else {
            return Ok(base_rev.to_string());
        };
        if !alias.olds.iter().any(|old| old == base_rev) {
            return Ok(base_rev.to_string());
        }
        let current = match fs::read(day_path(&self.root, date)?) {
            Ok(bytes) => rev_of(&bytes),
            Err(e) if e.kind() == ErrorKind::NotFound => String::new(),
            Err(e) => return Err(format!("reading day file: {e}")),
        };
        Ok(if current == alias.target {
            current
        } else {
            base_rev.to_string()
        })
    }

    /// Brings the day file's mirror lines in line with the sidecar (same lock as
    /// `save`). `Some` when the file changed; the caller emits `day-changed`
    /// with it, since the watcher echo of this write is suppressed.
    pub fn rewrite_mirror(&self, date: &str) -> Result<Option<DayChanged>, String> {
        let mut state = self.state();
        let Some((old, new)) = rewrite_mirror_file(&self.root, date)? else {
            return Ok(None);
        };
        let mut olds = match state.aliases.get(date) {
            Some(a) if a.target == old => a.olds.clone(),
            _ => Vec::new(),
        };
        olds.push(old);
        if olds.len() > MAX_ALIAS_REVS {
            olds.remove(0);
        }
        state.aliases.insert(
            date.to_string(),
            Alias {
                target: new.clone(),
                olds,
            },
        );
        state.known.insert(date.to_string(), new.clone());
        Ok(Some(DayChanged {
            date: date.to_string(),
            rev: new,
        }))
    }

    /// Notes sync (M9b): replaces (`Some(bytes)`) or deletes (`None`) a day file, but only
    /// while its current content is the one the caller looked at (`expect` is the SHA-256
    /// rev of it, `None` for "no file"). Same lock and atomic write as `save`, so the
    /// existing conflict handling applies to anything the frontend still holds. The
    /// returned `DayChanged` is for the caller to emit: the watcher echo is suppressed.
    pub fn replace_synced(
        &self,
        date: &str,
        expect: Option<&str>,
        bytes: Option<&[u8]>,
    ) -> Result<Replaced, String> {
        let mut state = self.state();
        let path = day_path(&self.root, date)?;
        let current = match fs::read(&path) {
            Ok(b) => Some(b),
            Err(e) if e.kind() == ErrorKind::NotFound => None,
            Err(e) => return Err(format!("reading {}: {e}", path.display())),
        };
        if current.as_deref().map(notes_crypto::rev_of).as_deref() != expect {
            return Ok(Replaced::Changed);
        }
        let new_rev = match bytes {
            Some(b) => {
                atomic::write_atomic(&path, b)?;
                rev_of(b)
            }
            None => {
                if current.is_none() {
                    return Ok(Replaced::Done(None));
                }
                fs::remove_file(&path).map_err(|e| format!("deleting {}: {e}", path.display()))?;
                String::new()
            }
        };
        state.aliases.remove(date);
        state.known.insert(date.to_string(), new_rev.clone());
        Ok(Replaced::Done(Some(DayChanged {
            date: date.to_string(),
            rev: new_rev,
        })))
    }

    /// Called for each path the watcher reports. `Some` only when the file's
    /// current content differs from what the frontend last saw.
    pub fn check_external(&self, path: &Path) -> Option<DayChanged> {
        let date = date_from_path(&self.root, path)?;
        let state = self.state();
        let rev = match fs::read(path) {
            Ok(bytes) => rev_of(&bytes),
            Err(e) if e.kind() == ErrorKind::NotFound => String::new(),
            Err(_) => return None,
        };
        if state.known.get(&date) == Some(&rev) {
            return None;
        }
        Some(DayChanged { date, rev })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_util::TempDir;

    fn blk(start: u16, end: u16, title: &str, body: &str) -> Block {
        Block {
            start,
            end,
            title: title.into(),
            tags: vec![],
            body: body.into(),
        }
    }

    fn save(root: &Path, date: &str, blocks: &[Block]) -> Result<(), String> {
        let rev = read_day(root, date)?.rev;
        match save_day(root, date, blocks, &rev)? {
            SaveOutcome::Saved { .. } => Ok(()),
            SaveOutcome::Conflict { .. } => Err("unexpected conflict".into()),
        }
    }

    const DATE: &str = "2026-10-20";

    fn path(tmp: &TempDir) -> PathBuf {
        day_path(&tmp.0, DATE).unwrap()
    }

    #[test]
    fn validate_date_accepts_valid() {
        assert_eq!(validate_date("2026-10-20"), Ok((2026, 10, 20)));
        assert!(validate_date("2026-12-31").is_ok());
        assert!(validate_date("2026-01-01").is_ok());
    }

    #[test]
    fn validate_date_accepts_leap_day() {
        assert!(validate_date("2024-02-29").is_ok());
        assert!(validate_date("2000-02-29").is_ok());
    }

    #[test]
    fn validate_date_rejects_feb_29_non_leap() {
        assert!(validate_date("2026-02-29").is_err());
        assert!(validate_date("1900-02-29").is_err());
    }

    #[test]
    fn validate_date_rejects_bad_month_day() {
        for d in [
            "2026-00-10",
            "2026-13-10",
            "2026-04-31",
            "2026-01-00",
            "2026-01-32",
        ] {
            assert!(validate_date(d).is_err(), "{d}");
        }
    }

    #[test]
    fn validate_date_rejects_short_forms_and_traversal() {
        for d in [
            "2026-1-1",
            "26-10-20",
            "../../etc/x",
            "2026/10/20",
            "2026-10-2a",
            "2026-10-20 ",
            "+026-10-20",
            "",
        ] {
            assert!(validate_date(d).is_err(), "{d:?}");
        }
    }

    #[test]
    fn day_path_layout() {
        let p = day_path(Path::new("/r"), "2026-10-20").unwrap();
        assert_eq!(p, Path::new("/r/2026/10/2026-10-20.md"));
    }

    #[test]
    fn load_day_missing_file_is_empty() {
        let tmp = TempDir::new();
        assert!(read_day(&tmp.0, DATE).unwrap().blocks.is_empty());
    }

    #[test]
    fn load_day_returns_file_order() {
        let tmp = TempDir::new();
        fs::create_dir_all(path(&tmp).parent().unwrap()).unwrap();
        fs::write(path(&tmp), "## 14:00–15:00 · B\nb\n## 09:00–10:00 · A\na\n").unwrap();
        let blocks = read_day(&tmp.0, DATE).unwrap().blocks;
        assert_eq!(blocks[0].title, "B");
        assert_eq!(blocks[1].title, "A");
    }

    #[test]
    fn save_day_creates_dirs_and_file() {
        let tmp = TempDir::new();
        save(&tmp.0, DATE, &[blk(540, 630, "Work", "")]).unwrap();
        assert_eq!(
            fs::read_to_string(path(&tmp)).unwrap(),
            "## 09:00–10:30 · Work\n"
        );
    }

    #[test]
    fn save_day_sorts_blocks() {
        let tmp = TempDir::new();
        save(
            &tmp.0,
            DATE,
            &[blk(840, 900, "B", ""), blk(540, 600, "A", "")],
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(path(&tmp)).unwrap(),
            "## 09:00–10:00 · A\n## 14:00–15:00 · B\n"
        );
    }

    #[test]
    fn save_day_keeps_preamble_and_crlf() {
        let tmp = TempDir::new();
        fs::create_dir_all(path(&tmp).parent().unwrap()).unwrap();
        fs::write(path(&tmp), "intro\r\n## 09:00-10:00 · A\r\nnote").unwrap();
        let mut blocks = read_day(&tmp.0, DATE).unwrap().blocks;
        blocks[0].start = 600;
        blocks[0].end = 660;
        save(&tmp.0, DATE, &blocks).unwrap();
        assert_eq!(
            fs::read_to_string(path(&tmp)).unwrap(),
            "intro\r\n## 10:00–11:00 · A\r\nnote\r\n"
        );
    }

    #[test]
    fn save_day_keeps_bodies() {
        let tmp = TempDir::new();
        let body = "line\n```\n## 10:00–11:00 · Fake\n```\n\n";
        save(&tmp.0, DATE, &[blk(540, 600, "A", body)]).unwrap();
        assert_eq!(read_day(&tmp.0, DATE).unwrap().blocks[0].body, body);
    }

    #[test]
    fn save_day_empty_blocks_missing_file_writes_nothing() {
        let tmp = TempDir::new();
        save(&tmp.0, DATE, &[]).unwrap();
        assert!(!path(&tmp).exists());
        assert!(!path(&tmp).parent().unwrap().exists());
    }

    #[test]
    fn save_day_empty_blocks_existing_file_keeps_preamble() {
        let tmp = TempDir::new();
        fs::create_dir_all(path(&tmp).parent().unwrap()).unwrap();
        fs::write(path(&tmp), "intro\n## 09:00–10:00 · A\nnote\n").unwrap();
        save(&tmp.0, DATE, &[]).unwrap();
        assert_eq!(fs::read_to_string(path(&tmp)).unwrap(), "intro\n");
    }

    #[test]
    fn save_day_non_utf8_existing_file_errors_without_writing() {
        let tmp = TempDir::new();
        fs::create_dir_all(path(&tmp).parent().unwrap()).unwrap();
        fs::write(path(&tmp), [0xff, 0xfe, 0x00]).unwrap();
        assert!(save(&tmp.0, DATE, &[blk(540, 600, "A", "")]).is_err());
        assert_eq!(fs::read(path(&tmp)).unwrap(), vec![0xff, 0xfe, 0x00]);
    }

    #[cfg(unix)]
    #[test]
    fn save_day_read_only_file_errors() {
        let tmp = TempDir::new();
        save(&tmp.0, DATE, &[blk(540, 600, "A", "note\n")]).unwrap();
        let mut perms = fs::metadata(path(&tmp)).unwrap().permissions();
        perms.set_readonly(true);
        fs::set_permissions(path(&tmp), perms).unwrap();
        let err = save(&tmp.0, DATE, &[blk(540, 600, "B", "")]).unwrap_err();
        assert!(err.contains("read-only"), "{err}");
        assert_eq!(
            fs::read_to_string(path(&tmp)).unwrap(),
            "## 09:00–10:00 · A\nnote\n"
        );
    }

    #[test]
    fn save_day_invalid_block_errors_without_writing() {
        let tmp = TempDir::new();
        assert!(save(&tmp.0, DATE, &[blk(600, 600, "A", "")]).is_err());
        assert!(!path(&tmp).exists());
    }

    // ---- revisions and conflicts ----

    fn write_raw(tmp: &TempDir, content: &str) {
        fs::create_dir_all(path(tmp).parent().unwrap()).unwrap();
        fs::write(path(tmp), content).unwrap();
    }

    fn saved_rev(outcome: SaveOutcome) -> String {
        match outcome {
            SaveOutcome::Saved { rev } => rev,
            other => panic!("expected saved, got {other:?}"),
        }
    }

    #[test]
    fn rev_of_is_deterministic_and_content_sensitive() {
        assert_eq!(rev_of(b"abc"), rev_of(b"abc"));
        assert_ne!(rev_of(b"abc"), rev_of(b"abd"));
        assert_ne!(rev_of(b""), "");
    }

    #[test]
    fn load_day_returns_rev() {
        let tmp = TempDir::new();
        write_raw(&tmp, "## 09:00–10:00 · A\n");
        let loaded = read_day(&tmp.0, DATE).unwrap();
        assert_eq!(loaded.rev, rev_of("## 09:00–10:00 · A\n".as_bytes()));
        assert_eq!(loaded.blocks.len(), 1);
    }

    #[test]
    fn load_day_missing_file_has_empty_rev() {
        let tmp = TempDir::new();
        assert_eq!(read_day(&tmp.0, DATE).unwrap().rev, "");
    }

    #[test]
    fn save_day_matching_rev_saves_and_returns_new_rev() {
        let tmp = TempDir::new();
        write_raw(&tmp, "## 09:00–10:00 · A\n");
        let base = read_day(&tmp.0, DATE).unwrap().rev;
        let out = save_day(&tmp.0, DATE, &[blk(540, 600, "B", "")], &base).unwrap();
        let rev = saved_rev(out);
        assert_ne!(rev, base);
        assert_eq!(
            fs::read_to_string(path(&tmp)).unwrap(),
            "## 09:00–10:00 · B\n"
        );
    }

    #[test]
    fn save_day_returned_rev_equals_next_load_rev() {
        let tmp = TempDir::new();
        let rev = saved_rev(save_day(&tmp.0, DATE, &[blk(540, 600, "A", "n\n")], "").unwrap());
        assert_eq!(read_day(&tmp.0, DATE).unwrap().rev, rev);
    }

    #[test]
    fn save_day_stale_rev_conflicts_and_does_not_write() {
        let tmp = TempDir::new();
        write_raw(&tmp, "## 09:00–10:00 · A\n");
        let stale = read_day(&tmp.0, DATE).unwrap().rev;
        write_raw(&tmp, "## 09:00–10:00 · A\nexternal\n");
        let out = save_day(&tmp.0, DATE, &[blk(540, 600, "Mine", "")], &stale).unwrap();
        assert!(matches!(out, SaveOutcome::Conflict { .. }));
        assert_eq!(
            fs::read_to_string(path(&tmp)).unwrap(),
            "## 09:00–10:00 · A\nexternal\n"
        );
    }

    #[test]
    fn save_day_conflict_returns_disk_blocks_and_rev() {
        let tmp = TempDir::new();
        write_raw(&tmp, "## 09:00–10:00 · A\nexternal\n");
        let out = save_day(&tmp.0, DATE, &[blk(540, 600, "Mine", "")], "stale").unwrap();
        assert_eq!(
            out,
            SaveOutcome::Conflict {
                blocks: vec![blk(540, 600, "A", "external\n")],
                rev: rev_of("## 09:00–10:00 · A\nexternal\n".as_bytes()),
            }
        );
    }

    #[test]
    fn save_day_missing_file_with_empty_base_rev_ok() {
        let tmp = TempDir::new();
        let out = save_day(&tmp.0, DATE, &[blk(540, 600, "A", "")], "").unwrap();
        assert!(!saved_rev(out).is_empty());
        assert!(path(&tmp).exists());
    }

    #[test]
    fn save_day_file_deleted_externally_conflicts() {
        let tmp = TempDir::new();
        write_raw(&tmp, "## 09:00–10:00 · A\n");
        let base = read_day(&tmp.0, DATE).unwrap().rev;
        fs::remove_file(path(&tmp)).unwrap();
        let out = save_day(&tmp.0, DATE, &[blk(540, 600, "A", "n\n")], &base).unwrap();
        assert_eq!(
            out,
            SaveOutcome::Conflict {
                blocks: vec![],
                rev: String::new()
            }
        );
        assert!(!path(&tmp).exists());
    }

    #[test]
    fn save_day_created_externally_while_base_empty_conflicts() {
        let tmp = TempDir::new();
        write_raw(&tmp, "## 09:00–10:00 · Other\n");
        let out = save_day(&tmp.0, DATE, &[blk(540, 600, "Mine", "")], "").unwrap();
        assert!(matches!(out, SaveOutcome::Conflict { .. }));
        assert_eq!(
            fs::read_to_string(path(&tmp)).unwrap(),
            "## 09:00–10:00 · Other\n"
        );
    }

    #[test]
    fn save_day_escapes_lookalike_heading_and_keeps_block_count() {
        let tmp = TempDir::new();
        save(
            &tmp.0,
            DATE,
            &[blk(540, 600, "A", "## 10:00–11:00 · Fake\n")],
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(path(&tmp)).unwrap(),
            "## 09:00–10:00 · A\n\\## 10:00–11:00 · Fake\n"
        );
        assert_eq!(read_day(&tmp.0, DATE).unwrap().blocks.len(), 1);
    }

    #[test]
    fn save_day_unclosed_fence_does_not_swallow_next_block() {
        let tmp = TempDir::new();
        save(
            &tmp.0,
            DATE,
            &[blk(540, 600, "A", "```\ncode\n"), blk(600, 660, "B", "b\n")],
        )
        .unwrap();
        let blocks = read_day(&tmp.0, DATE).unwrap().blocks;
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[1].title, "B");
    }

    #[test]
    fn save_day_note_edit_keeps_other_blocks_and_preamble_byte_identical() {
        let tmp = TempDir::new();
        let original = "intro\r\n## 09:00-10:00 · A\r\nold\r\n## 9-10 malformed\r\n\r\n## 11:00–12:00 · C #x\r\nkeep me\r\n";
        write_raw(&tmp, original);
        let loaded = read_day(&tmp.0, DATE).unwrap();
        let mut blocks = loaded.blocks.clone();
        blocks[0].body = "new\n\n".into();
        save_day(&tmp.0, DATE, &blocks, &loaded.rev).unwrap();
        // The hand-written `-` heading is canonicalised to an en dash; everything else is untouched.
        assert_eq!(
            fs::read_to_string(path(&tmp)).unwrap(),
            "intro\r\n## 09:00–10:00 · A\r\nnew\r\n\r\n## 11:00–12:00 · C #x\r\nkeep me\r\n"
        );
    }

    // ---- date_from_path ----

    fn root() -> PathBuf {
        PathBuf::from("/r/notes")
    }

    #[test]
    fn date_from_path_accepts_day_file() {
        let p = root().join("2026/10/2026-10-20.md");
        assert_eq!(date_from_path(&root(), &p).as_deref(), Some("2026-10-20"));
    }

    #[test]
    fn date_from_path_rejects_config_temp_swap_and_ds_store() {
        for rel in [
            "config.json",
            "2026/10/.2026-10-20.md.catenga-tmp-1-1",
            "2026/10/.2026-10-20.md.swp",
            "2026/10/2026-10-20.md~",
            "2026/10/2026-10-20 (1).md",
            "2026/10/.2026-10-20.md",
            "2026/10/2026-10-20.sessions.json",
            "2026/10/.2026-10-20.sessions.json.catenga-tmp-1-1",
            "2026/10/.DS_Store",
            ".obsidian/workspace.md",
        ] {
            assert_eq!(date_from_path(&root(), &root().join(rel)), None, "{rel}");
        }
    }

    #[test]
    fn date_from_path_rejects_wrong_depth_and_mismatched_dirs() {
        for rel in [
            "2026-10-20.md",
            "2026/2026-10-20.md",
            "2026/10/sub/2026-10-20.md",
            "2025/10/2026-10-20.md",
            "2026/09/2026-10-20.md",
        ] {
            assert_eq!(date_from_path(&root(), &root().join(rel)), None, "{rel}");
        }
    }

    #[test]
    fn sidecar_date_from_path_accepts_only_sessions_json() {
        let ok = root().join("2026/10/2026-10-20.sessions.json");
        assert_eq!(
            sidecar_date_from_path(&root(), &ok).as_deref(),
            Some("2026-10-20")
        );
        for rel in [
            "2026/10/2026-10-20.md",
            "2026/10/2026-10-20.sessions.json~",
            "2026/10/.2026-10-20.sessions.json.catenga-tmp-1-1",
            "2026/09/2026-10-20.sessions.json",
            "2026/02/2026-02-30.sessions.json",
        ] {
            assert_eq!(
                sidecar_date_from_path(&root(), &root().join(rel)),
                None,
                "{rel}"
            );
        }
        assert_eq!(
            date_from_path(&root(), &ok),
            None,
            "a sidecar is not a day file"
        );
    }

    #[test]
    fn date_from_path_rejects_invalid_date() {
        let p = root().join("2026/02/2026-02-30.md");
        assert_eq!(date_from_path(&root(), &p), None);
    }

    #[test]
    fn date_from_path_rejects_outside_root() {
        let p = PathBuf::from("/elsewhere/2026/10/2026-10-20.md");
        assert_eq!(date_from_path(&root(), &p), None);
        assert_eq!(
            date_from_path(&root(), Path::new("/r/2026/10/2026-10-20.md")),
            None
        );
    }

    // ---- DayStore ----

    fn store(tmp: &TempDir) -> DayStore {
        DayStore::new(fs::canonicalize(&tmp.0).unwrap())
    }

    fn day_file_path(store: &DayStore) -> PathBuf {
        day_path(store.root(), DATE).unwrap()
    }

    fn write_external(store: &DayStore, content: &str) {
        let p = day_file_path(store);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    #[test]
    fn store_check_external_ignores_own_write() {
        let tmp = TempDir::new();
        let s = store(&tmp);
        let rev = s.load(DATE).unwrap().rev;
        s.save(DATE, &[blk(540, 600, "A", "")], &rev).unwrap();
        assert_eq!(s.check_external(&day_file_path(&s)), None);
    }

    #[test]
    fn store_check_external_ignores_unchanged_touch() {
        let tmp = TempDir::new();
        let s = store(&tmp);
        write_external(&s, "## 09:00–10:00 · A\n");
        s.load(DATE).unwrap();
        write_external(&s, "## 09:00–10:00 · A\n");
        assert_eq!(s.check_external(&day_file_path(&s)), None);
    }

    #[test]
    fn store_check_external_reports_foreign_edit() {
        let tmp = TempDir::new();
        let s = store(&tmp);
        write_external(&s, "## 09:00–10:00 · A\n");
        s.load(DATE).unwrap();
        write_external(&s, "## 09:00–10:00 · A\nedited\n");
        assert_eq!(
            s.check_external(&day_file_path(&s)),
            Some(DayChanged {
                date: DATE.into(),
                rev: rev_of("## 09:00–10:00 · A\nedited\n".as_bytes())
            })
        );
    }

    #[test]
    fn store_check_external_reports_delete_with_empty_rev() {
        let tmp = TempDir::new();
        let s = store(&tmp);
        write_external(&s, "## 09:00–10:00 · A\n");
        s.load(DATE).unwrap();
        fs::remove_file(day_file_path(&s)).unwrap();
        assert_eq!(
            s.check_external(&day_file_path(&s)),
            Some(DayChanged {
                date: DATE.into(),
                rev: String::new()
            })
        );
    }

    #[test]
    fn store_check_external_reports_never_loaded_day() {
        let tmp = TempDir::new();
        let s = store(&tmp);
        write_external(&s, "## 09:00–10:00 · A\n");
        assert!(s.check_external(&day_file_path(&s)).is_some());
    }

    #[test]
    fn store_check_external_ignores_non_day_paths() {
        let tmp = TempDir::new();
        let s = store(&tmp);
        fs::write(s.root().join("config.json"), "{}").unwrap();
        assert_eq!(s.check_external(&s.root().join("config.json")), None);
        assert_eq!(
            s.check_external(&s.root().join("2026/10/.x.md.catenga-tmp-1-1")),
            None
        );
    }

    #[test]
    fn store_conflict_marks_rev_known() {
        let tmp = TempDir::new();
        let s = store(&tmp);
        write_external(&s, "## 09:00–10:00 · A\nexternal\n");
        let out = s.save(DATE, &[blk(540, 600, "Mine", "")], "stale").unwrap();
        assert!(matches!(out, SaveOutcome::Conflict { .. }));
        assert_eq!(s.check_external(&day_file_path(&s)), None);
    }

    #[test]
    fn store_concurrent_saves_are_serialized() {
        let tmp = TempDir::new();
        let s = std::sync::Arc::new(store(&tmp));
        let handles: Vec<_> = ["A", "B"]
            .into_iter()
            .map(|title| {
                let s = s.clone();
                std::thread::spawn(move || s.save(DATE, &[blk(540, 600, title, "")], "").unwrap())
            })
            .collect();
        let outcomes: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let saved = outcomes
            .iter()
            .filter(|o| matches!(o, SaveOutcome::Saved { .. }))
            .count();
        assert_eq!(saved, 1);
        assert_eq!(outcomes.len() - saved, 1);
    }

    // ---- timer mirror lines (M6) ----

    use crate::sessions::{self, Mode, Session, SessionBlock};

    const S0: &str = "2026-10-20T09:05:00-03:00";
    const S1: &str = "2026-10-20T09:50:00-03:00";
    const MIRROR_A: &str = "⏱ 09:05–09:50 · 45 min · free";

    fn done_session(
        start: &str,
        end: &str,
        block_start: &str,
        block_end: &str,
        title: &str,
    ) -> Session {
        Session {
            start: start.into(),
            end: Some(end.into()),
            mode: Mode::Free,
            block: SessionBlock {
                start: block_start.into(),
                end: block_end.into(),
                title: title.into(),
                tags: vec![],
                project: None,
            },
            last_seen: None,
        }
    }

    fn add_session(root: &Path, s: Session) {
        sessions::upsert(root, DATE, s).unwrap();
    }

    #[test]
    fn save_day_appends_mirror_and_load_strips_it() {
        let tmp = TempDir::new();
        add_session(&tmp.0, done_session(S0, S1, "09:00", "10:00", "A"));
        save(&tmp.0, DATE, &[blk(540, 600, "A", "note\n")]).unwrap();
        assert_eq!(
            fs::read_to_string(path(&tmp)).unwrap(),
            format!("## 09:00–10:00 · A\nnote\n\n{MIRROR_A}\n")
        );
        assert_eq!(read_day(&tmp.0, DATE).unwrap().blocks[0].body, "note\n");
    }

    #[test]
    fn save_day_mirror_only_goes_to_the_matching_block() {
        let tmp = TempDir::new();
        add_session(&tmp.0, done_session(S0, S1, "09:00", "10:00", "A"));
        add_session(
            &tmp.0,
            done_session(
                "2026-10-20T11:00:00-03:00",
                "2026-10-20T11:30:00-03:00",
                "11:00",
                "12:00",
                "Gone",
            ),
        );
        save(
            &tmp.0,
            DATE,
            &[blk(540, 600, "A", ""), blk(840, 900, "B", "b\n")],
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(path(&tmp)).unwrap(),
            format!("## 09:00–10:00 · A\n{MIRROR_A}\n## 14:00–15:00 · B\nb\n")
        );
    }

    #[test]
    fn save_day_rewrites_stale_mirror_lines_without_duplicating() {
        let tmp = TempDir::new();
        add_session(&tmp.0, done_session(S0, S1, "09:00", "10:00", "A"));
        save(&tmp.0, DATE, &[blk(540, 600, "A", "note\n")]).unwrap();
        save(&tmp.0, DATE, &[blk(540, 600, "A", "note more\n")]).unwrap();
        assert_eq!(
            fs::read_to_string(path(&tmp)).unwrap(),
            format!("## 09:00–10:00 · A\nnote more\n\n{MIRROR_A}\n")
        );
    }

    #[test]
    fn mirror_round_trips_with_crlf_and_preamble() {
        let tmp = TempDir::new();
        write_raw(
            &tmp,
            "intro\r\n## 09:00–10:00 · A\r\nnote\r\n\r\n⏱ 09:05–09:50 · 45 min · free\r\n",
        );
        add_session(&tmp.0, done_session(S0, S1, "09:00", "10:00", "A"));
        let loaded = read_day(&tmp.0, DATE).unwrap();
        assert_eq!(loaded.blocks[0].body, "note\n");
        save_day(&tmp.0, DATE, &loaded.blocks, &loaded.rev).unwrap();
        assert_eq!(
            fs::read_to_string(path(&tmp)).unwrap(),
            "intro\r\n## 09:00–10:00 · A\r\nnote\r\n\r\n⏱ 09:05–09:50 · 45 min · free\r\n"
        );
    }

    #[test]
    fn conflict_payload_has_no_mirror_lines() {
        let tmp = TempDir::new();
        write_raw(
            &tmp,
            &format!("## 09:00–10:00 · A\nexternal\n\n{MIRROR_A}\n"),
        );
        let out = save_day(&tmp.0, DATE, &[blk(540, 600, "Mine", "")], "stale").unwrap();
        match out {
            SaveOutcome::Conflict { blocks, .. } => assert_eq!(blocks[0].body, "external\n"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn invalid_sidecar_keeps_existing_mirror_lines_on_save() {
        let tmp = TempDir::new();
        write_raw(&tmp, &format!("## 09:00–10:00 · A\nnote\n\n{MIRROR_A}\n"));
        let sp = sessions::sidecar_path(&tmp.0, DATE).unwrap();
        fs::write(&sp, "{ nope").unwrap();
        let loaded = read_day(&tmp.0, DATE).unwrap();
        let mut blocks = loaded.blocks.clone();
        blocks[0].body = "edited\n".into();
        save_day(&tmp.0, DATE, &blocks, &loaded.rev).unwrap();
        assert_eq!(
            fs::read_to_string(path(&tmp)).unwrap(),
            format!("## 09:00–10:00 · A\nedited\n\n{MIRROR_A}\n")
        );
        assert_eq!(fs::read_to_string(&sp).unwrap(), "{ nope");
    }

    fn mirror_store(tmp: &TempDir) -> DayStore {
        let s = store(tmp);
        write_external(&s, "## 09:00–10:00 · A\nnote\n");
        s
    }

    #[test]
    fn rewrite_mirror_adds_lines_and_reports_new_rev() {
        let tmp = TempDir::new();
        let s = mirror_store(&tmp);
        s.load(DATE).unwrap();
        add_session(s.root(), done_session(S0, S1, "09:00", "10:00", "A"));
        let changed = s.rewrite_mirror(DATE).unwrap().unwrap();
        let text = fs::read_to_string(day_file_path(&s)).unwrap();
        assert_eq!(text, format!("## 09:00–10:00 · A\nnote\n\n{MIRROR_A}\n"));
        assert_eq!(
            changed,
            DayChanged {
                date: DATE.into(),
                rev: rev_of(text.as_bytes())
            }
        );
        // The watcher echo of our own rewrite is suppressed.
        assert_eq!(s.check_external(&day_file_path(&s)), None);
        // The frontend sees no difference in the blocks.
        assert_eq!(s.load(DATE).unwrap().blocks[0].body, "note\n");
    }

    #[test]
    fn rewrite_mirror_without_change_or_file_is_none() {
        let tmp = TempDir::new();
        let s = store(&tmp);
        assert_eq!(s.rewrite_mirror(DATE).unwrap(), None);
        write_external(&s, "## 09:00–10:00 · A\nnote\n");
        assert_eq!(s.rewrite_mirror(DATE).unwrap(), None);
        assert_eq!(
            fs::read_to_string(day_file_path(&s)).unwrap(),
            "## 09:00–10:00 · A\nnote\n"
        );
    }

    #[test]
    fn rewrite_mirror_leaves_non_canonical_files_alone_when_nothing_changes() {
        let tmp = TempDir::new();
        let s = store(&tmp);
        write_external(&s, "## 09:00-10:00 · A\nnote\n");
        assert_eq!(s.rewrite_mirror(DATE).unwrap(), None);
        assert_eq!(
            fs::read_to_string(day_file_path(&s)).unwrap(),
            "## 09:00-10:00 · A\nnote\n"
        );
    }

    #[test]
    fn rewrite_mirror_removes_lines_of_deleted_sessions() {
        let tmp = TempDir::new();
        let s = mirror_store(&tmp);
        add_session(s.root(), done_session(S0, S1, "09:00", "10:00", "A"));
        s.rewrite_mirror(DATE).unwrap();
        sessions::delete(s.root(), DATE, S0).unwrap();
        s.rewrite_mirror(DATE).unwrap();
        assert_eq!(
            fs::read_to_string(day_file_path(&s)).unwrap(),
            "## 09:00–10:00 · A\nnote\n"
        );
    }

    #[test]
    fn save_with_pre_rewrite_rev_is_accepted_and_keeps_text_and_mirror() {
        let tmp = TempDir::new();
        let s = mirror_store(&tmp);
        let old = s.load(DATE).unwrap();
        add_session(s.root(), done_session(S0, S1, "09:00", "10:00", "A"));
        s.rewrite_mirror(DATE).unwrap();
        let mut blocks = old.blocks.clone();
        blocks[0].body = "note typed meanwhile\n".into();
        let out = s.save(DATE, &blocks, &old.rev).unwrap();
        assert!(matches!(out, SaveOutcome::Saved { .. }), "{out:?}");
        assert_eq!(
            fs::read_to_string(day_file_path(&s)).unwrap(),
            format!("## 09:00–10:00 · A\nnote typed meanwhile\n\n{MIRROR_A}\n")
        );
    }

    #[test]
    fn alias_survives_two_rewrites_in_a_row() {
        let tmp = TempDir::new();
        let s = mirror_store(&tmp);
        let old = s.load(DATE).unwrap();
        add_session(s.root(), done_session(S0, S1, "09:00", "10:00", "A"));
        s.rewrite_mirror(DATE).unwrap();
        add_session(
            s.root(),
            done_session(
                "2026-10-20T09:55:00-03:00",
                "2026-10-20T10:00:00-03:00",
                "09:00",
                "10:00",
                "A",
            ),
        );
        s.rewrite_mirror(DATE).unwrap();
        let out = s.save(DATE, &old.blocks, &old.rev).unwrap();
        assert!(matches!(out, SaveOutcome::Saved { .. }), "{out:?}");
    }

    #[test]
    fn alias_is_used_once_the_user_saved() {
        let tmp = TempDir::new();
        let s = mirror_store(&tmp);
        let old = s.load(DATE).unwrap();
        add_session(s.root(), done_session(S0, S1, "09:00", "10:00", "A"));
        s.rewrite_mirror(DATE).unwrap();
        s.save(DATE, &old.blocks, &old.rev).unwrap();
        // A second save with the same stale base is a real conflict now.
        let out = s.save(DATE, &old.blocks, &old.rev).unwrap();
        assert!(matches!(out, SaveOutcome::Conflict { .. }), "{out:?}");
    }

    #[test]
    fn foreign_edit_after_a_rewrite_still_conflicts() {
        let tmp = TempDir::new();
        let s = mirror_store(&tmp);
        let old = s.load(DATE).unwrap();
        add_session(s.root(), done_session(S0, S1, "09:00", "10:00", "A"));
        s.rewrite_mirror(DATE).unwrap();
        write_external(&s, "## 09:00–10:00 · A\nforeign\n");
        let out = s.save(DATE, &old.blocks, &old.rev).unwrap();
        assert!(matches!(out, SaveOutcome::Conflict { .. }), "{out:?}");
        assert_eq!(
            fs::read_to_string(day_file_path(&s)).unwrap(),
            "## 09:00–10:00 · A\nforeign\n"
        );
    }

    #[test]
    fn foreign_edit_before_a_rewrite_still_conflicts() {
        let tmp = TempDir::new();
        let s = mirror_store(&tmp);
        let old = s.load(DATE).unwrap();
        write_external(&s, "## 09:00–10:00 · A\nforeign\n");
        add_session(s.root(), done_session(S0, S1, "09:00", "10:00", "A"));
        s.rewrite_mirror(DATE).unwrap();
        // The foreign text is kept by the rewrite...
        assert_eq!(
            fs::read_to_string(day_file_path(&s)).unwrap(),
            format!("## 09:00–10:00 · A\nforeign\n\n{MIRROR_A}\n")
        );
        // ...and the frontend's stale base is not accepted.
        let out = s.save(DATE, &old.blocks, &old.rev).unwrap();
        assert!(matches!(out, SaveOutcome::Conflict { .. }), "{out:?}");
    }

    #[test]
    fn rewrite_mirror_with_invalid_sidecar_errors_and_leaves_the_file() {
        let tmp = TempDir::new();
        let s = mirror_store(&tmp);
        fs::create_dir_all(
            sessions::sidecar_path(s.root(), DATE)
                .unwrap()
                .parent()
                .unwrap(),
        )
        .unwrap();
        fs::write(sessions::sidecar_path(s.root(), DATE).unwrap(), "{ nope").unwrap();
        assert!(s.rewrite_mirror(DATE).is_err());
        assert_eq!(
            fs::read_to_string(day_file_path(&s)).unwrap(),
            "## 09:00–10:00 · A\nnote\n"
        );
    }

    fn plain_store(tmp: &TempDir) -> DayStore {
        DayStore::new(fs::canonicalize(&tmp.0).unwrap())
    }

    #[test]
    fn replace_synced_writes_only_over_the_expected_content_and_tells_the_frontend() {
        let tmp = TempDir::new();
        let s = plain_store(&tmp);
        let path = day_path(s.root(), DATE).unwrap();
        // No file yet: expecting "no file" works and the watcher echo is suppressed.
        let out = s
            .replace_synced(
                DATE,
                None,
                Some(b"## 09:00\xE2\x80\x9310:00 \xC2\xB7 A\nremote\n"),
            )
            .unwrap();
        let Replaced::Done(Some(changed)) = out else {
            panic!("{out:?}")
        };
        assert_eq!(changed.date, DATE);
        assert_eq!(changed.rev, s.load(DATE).unwrap().rev);
        assert!(
            s.check_external(&path).is_none(),
            "own write is not external"
        );
        let current = fs::read(&path).unwrap();
        // Edited meanwhile: expecting the old content is refused and nothing is touched.
        let stale = notes_crypto::rev_of(b"something else");
        assert_eq!(
            s.replace_synced(DATE, Some(&stale), Some(b"x")).unwrap(),
            Replaced::Changed
        );
        assert_eq!(
            s.replace_synced(DATE, None, Some(b"x")).unwrap(),
            Replaced::Changed
        );
        assert_eq!(fs::read(&path).unwrap(), current);
        // The frontend's next save is based on the rev it was told about.
        let loaded = s.load(DATE).unwrap();
        assert_eq!(loaded.blocks[0].body, "remote\n");
        let out = s.save(DATE, &loaded.blocks, &loaded.rev).unwrap();
        assert!(matches!(out, SaveOutcome::Saved { .. }), "{out:?}");
    }

    #[test]
    fn replace_synced_deletes_only_the_expected_content() {
        let tmp = TempDir::new();
        let s = plain_store(&tmp);
        let path = day_path(s.root(), DATE).unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "## 09:00–10:00 · A\nmine\n").unwrap();
        let wrong = notes_crypto::rev_of(b"other");
        assert_eq!(
            s.replace_synced(DATE, Some(&wrong), None).unwrap(),
            Replaced::Changed
        );
        assert!(path.exists());
        let right = notes_crypto::rev_of(&fs::read(&path).unwrap());
        let out = s.replace_synced(DATE, Some(&right), None).unwrap();
        assert_eq!(
            out,
            Replaced::Done(Some(DayChanged {
                date: DATE.into(),
                rev: String::new()
            }))
        );
        assert!(!path.exists());
        assert!(s.check_external(&path).is_none());
        // Already gone, expecting nothing: a no-op.
        assert_eq!(
            s.replace_synced(DATE, None, None).unwrap(),
            Replaced::Done(None)
        );
    }
}
