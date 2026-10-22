//! Atomic file replacement: write a temp file next to the target, fsync, rename over it.

use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, SystemTime};

static COUNTER: AtomicU32 = AtomicU32::new(0);
const TEMP_MARKER: &str = ".catenga-tmp-";
const STALE_AFTER: Duration = Duration::from_secs(60);
const RENAME_RETRY_MS: [u64; 5] = [20, 40, 80, 160, 320];

/// Hidden temp path in the same directory as `path`: `.{name}.catenga-tmp-{pid}-{n}`.
fn temp_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let tmp = format!(".{name}{TEMP_MARKER}{}-{n}", std::process::id());
    path.with_file_name(tmp)
}

/// Replaces `path` with `bytes` so readers never see a partial file.
/// A read-only existing target is refused (a plain rename would bypass it).
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let fail = |e: &dyn std::fmt::Display| format!("writing {}: {e}", path.display());
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    if let Some(dir) = parent {
        fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    }
    let existing = fs::metadata(path).ok();
    if existing
        .as_ref()
        .is_some_and(|m| m.permissions().readonly())
    {
        return Err(format!("{} is read-only", path.display()));
    }

    let tmp = temp_path(path);
    let result = write_and_rename(&tmp, path, bytes, existing.as_ref());
    if let Err(e) = result {
        let _ = fs::remove_file(&tmp);
        return Err(fail(&e));
    }

    #[cfg(unix)]
    if let Some(dir) = parent {
        // Best effort: the data is already in place.
        let _ = File::open(dir).and_then(|d| d.sync_all());
    }
    Ok(())
}

fn write_and_rename(
    tmp: &Path,
    path: &Path,
    bytes: &[u8],
    existing: Option<&fs::Metadata>,
) -> std::io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    if let Some(meta) = existing.filter(|m| m.is_file()) {
        fs::set_permissions(tmp, meta.permissions())?;
    }
    rename_with_retry(tmp, path)
}

/// On Windows a rename over a file another process has open fails with
/// `PermissionDenied`; retry briefly. Other errors return immediately.
fn rename_with_retry(from: &Path, to: &Path) -> std::io::Result<()> {
    for delay in RENAME_RETRY_MS {
        match fs::rename(from, to) {
            Err(e) if e.kind() == ErrorKind::PermissionDenied => {
                std::thread::sleep(Duration::from_millis(delay));
            }
            other => return other,
        }
    }
    fs::rename(from, to)
}

/// Deletes leftover temp files from a crashed write. Only looks at the
/// `root/YYYY/MM/` layout and only at files older than 60 s.
pub fn cleanup_stale_temps(root: &Path) {
    cleanup_older_than(root, STALE_AFTER);
}

fn cleanup_older_than(root: &Path, age: Duration) {
    let now = SystemTime::now();
    for month_dir in subdirs(root).iter().flat_map(|y| subdirs(y)) {
        let Ok(entries) = fs::read_dir(month_dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with('.') || !name.contains(TEMP_MARKER) {
                continue;
            }
            let old = entry
                .metadata()
                .and_then(|m| m.modified())
                .is_ok_and(|t| now.duration_since(t).is_ok_and(|d| d >= age));
            if old && let Err(e) = fs::remove_file(entry.path()) {
                eprintln!(
                    "could not remove stale temp {}: {e}",
                    entry.path().display()
                );
            }
        }
    }
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_util::TempDir;

    fn temps_in(dir: &Path) -> Vec<String> {
        fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(TEMP_MARKER))
            .collect()
    }

    #[test]
    fn write_atomic_creates_file_and_parent_dirs() {
        let tmp = TempDir::new();
        let path = tmp.0.join("2026/10/x.md");
        write_atomic(&path, b"hello").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"hello");
    }

    #[test]
    fn write_atomic_replaces_existing_content() {
        let tmp = TempDir::new();
        let path = tmp.0.join("x.md");
        fs::write(&path, "old content that is longer").unwrap();
        write_atomic(&path, b"new").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
    }

    #[test]
    fn write_atomic_leaves_no_temp_file() {
        let tmp = TempDir::new();
        let path = tmp.0.join("x.md");
        write_atomic(&path, b"a").unwrap();
        write_atomic(&path, b"b").unwrap();
        assert!(temps_in(&tmp.0).is_empty());
        assert_eq!(fs::read_dir(&tmp.0).unwrap().count(), 1);
    }

    #[test]
    fn temp_name_is_hidden_and_in_target_dir() {
        let target = Path::new("/r/2026/10/2026-10-20.md");
        let tmp = temp_path(target);
        assert_eq!(tmp.parent(), target.parent());
        let name = tmp.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with('.'));
        assert!(name.contains("2026-10-20.md.catenga-tmp-"));
    }

    #[test]
    fn temp_names_are_unique() {
        let target = Path::new("/r/x.md");
        assert_ne!(temp_path(target), temp_path(target));
    }

    #[test]
    fn write_atomic_readonly_target_errors_and_keeps_content() {
        let tmp = TempDir::new();
        let path = tmp.0.join("x.md");
        fs::write(&path, "keep").unwrap();
        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_readonly(true);
        fs::set_permissions(&path, perms).unwrap();

        let err = write_atomic(&path, b"nope").unwrap_err();
        assert!(err.contains("read-only"), "{err}");
        assert_eq!(fs::read(&path).unwrap(), b"keep");
        assert!(temps_in(&tmp.0).is_empty());

        let mut perms = fs::metadata(&path).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        fs::set_permissions(&path, perms).unwrap();
    }

    #[test]
    fn write_atomic_rename_failure_removes_temp() {
        let tmp = TempDir::new();
        let target = tmp.0.join("dir.md");
        fs::create_dir(&target).unwrap();
        assert!(write_atomic(&target, b"x").is_err());
        assert!(temps_in(&tmp.0).is_empty());
        assert!(target.is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn write_atomic_preserves_unix_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new();
        let path = tmp.0.join("x.md");
        fs::write(&path, "a").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        write_atomic(&path, b"b").unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn write_atomic_concurrent_writers_never_tear() {
        let tmp = TempDir::new();
        let path = tmp.0.join("x.md");
        let a = vec![b'a'; 256 * 1024];
        let b = vec![b'b'; 300 * 1024];
        let handles: Vec<_> = [a.clone(), b.clone()]
            .into_iter()
            .map(|payload| {
                let path = path.clone();
                std::thread::spawn(move || {
                    for _ in 0..50 {
                        write_atomic(&path, &payload).unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let content = fs::read(&path).unwrap();
        assert!(content == a || content == b);
        assert!(temps_in(&tmp.0).is_empty());
    }

    fn make_old(path: &Path) {
        let f = OpenOptions::new().write(true).open(path).unwrap();
        f.set_modified(SystemTime::now() - Duration::from_secs(3600))
            .unwrap();
    }

    #[test]
    fn cleanup_stale_temps_removes_old_temps_only() {
        let tmp = TempDir::new();
        let month = tmp.0.join("2026/10");
        fs::create_dir_all(&month).unwrap();
        let old = month.join(".2026-10-20.md.catenga-tmp-1-1");
        let young = month.join(".2026-10-21.md.catenga-tmp-1-2");
        let day = month.join("2026-10-20.md");
        for p in [&old, &young, &day] {
            fs::write(p, "x").unwrap();
        }
        fs::write(tmp.0.join("config.json"), "{}").unwrap();
        make_old(&old);
        make_old(&day);

        cleanup_stale_temps(&tmp.0);

        assert!(!old.exists());
        assert!(young.exists());
        assert!(day.exists());
        assert!(tmp.0.join("config.json").exists());
    }

    #[test]
    fn cleanup_stale_temps_missing_root_is_ok() {
        let tmp = TempDir::new();
        cleanup_stale_temps(&tmp.0.join("nope"));
    }

    #[test]
    fn cleanup_stale_temps_ignores_deeper_dirs() {
        let tmp = TempDir::new();
        let deep = tmp.0.join("2026/10/sub");
        let shallow = tmp.0.join("2026");
        fs::create_dir_all(&deep).unwrap();
        let a = deep.join(".x.md.catenga-tmp-1-1");
        let b = shallow.join(".x.md.catenga-tmp-1-2");
        for p in [&a, &b] {
            fs::write(p, "x").unwrap();
            make_old(p);
        }
        cleanup_stale_temps(&tmp.0);
        assert!(a.exists());
        assert!(b.exists());
    }
}
