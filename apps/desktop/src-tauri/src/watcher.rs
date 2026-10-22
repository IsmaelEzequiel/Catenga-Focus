//! Watches the notes folder and reports day files changed by something other than this app.

use crate::index::IndexEvent;
use crate::storage::{DayChanged, DayStore, date_from_path, sidecar_date_from_path};
use notify_debouncer_mini::notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_mini::{DebounceEventResult, Debouncer, new_debouncer};
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

pub const DEBOUNCE: Duration = Duration::from_millis(300);

/// Starts one recursive watcher over the store's root. Keep the returned
/// handle alive; dropping it stops watching. `on_change` runs on the
/// debouncer's thread. `on_index` hears about every change to a day file (own
/// saves included) and about folder-level changes that need a rescan. `on_path` hears
/// about every changed path (the notes sync decides whether it is one of its files).
pub fn start(
    store: Arc<DayStore>,
    debounce: Duration,
    on_change: impl Fn(DayChanged) + Send + 'static,
    on_index: impl Fn(IndexEvent) + Send + 'static,
    on_path: impl Fn(&Path) + Send + 'static,
) -> Result<Debouncer<RecommendedWatcher>, String> {
    let root = store.root().to_path_buf();
    let handler_store = store.clone();
    let index_root = root.clone();
    let mut debouncer = new_debouncer(debounce, move |result: DebounceEventResult| match result {
        Ok(events) => {
            let mut seen = HashSet::new();
            let mut rescan = false;
            for event in events {
                if !seen.insert(event.path.clone()) {
                    continue;
                }
                on_path(&event.path);
                match index_event(&index_root, &event.path) {
                    Some(IndexEvent::Rescan) => rescan = true,
                    Some(day) => on_index(day),
                    None => {}
                }
                if let Some(changed) = handler_store.check_external(&event.path) {
                    on_change(changed);
                }
            }
            if rescan {
                on_index(IndexEvent::Rescan);
            }
        }
        Err(e) => {
            eprintln!("watcher error: {e}");
            on_index(IndexEvent::Rescan);
        }
    })
    .map_err(|e| format!("starting watcher: {e}"))?;
    debouncer
        .watcher()
        .watch(&root, RecursiveMode::Recursive)
        .map_err(|e| format!("watching {}: {e}", root.display()))?;
    Ok(debouncer)
}

/// What the index should do about a changed path: re-read one day, or rescan
/// everything when a year/month folder (or the root) appeared, moved or vanished.
fn index_event(root: &Path, path: &Path) -> Option<IndexEvent> {
    if let Some(date) = date_from_path(root, path) {
        return Some(IndexEvent::Day(date));
    }
    if let Some(date) = sidecar_date_from_path(root, path) {
        return Some(IndexEvent::Sessions(date));
    }
    let rel = path.strip_prefix(root).ok()?;
    let folder_like = rel.components().count() <= 2
        && rel.components().all(|c| {
            c.as_os_str()
                .to_str()
                .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        });
    (folder_like && (!path.exists() || path.is_dir())).then_some(IndexEvent::Rescan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_util::TempDir;
    use crate::day_file::Block;
    use crate::storage::day_path;
    use std::fs;
    use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};

    const DATE: &str = "2026-10-20";
    const TEST_DEBOUNCE: Duration = Duration::from_millis(100);

    struct Fixture {
        _tmp: TempDir,
        store: Arc<DayStore>,
        rx: Receiver<DayChanged>,
        rx_index: Receiver<IndexEvent>,
        rx_path: Receiver<std::path::PathBuf>,
        _handle: Debouncer<RecommendedWatcher>,
    }

    fn fixture() -> Fixture {
        let tmp = TempDir::new();
        let store = Arc::new(DayStore::new(fs::canonicalize(&tmp.0).unwrap()));
        // The month folder exists before watching starts: on a busy machine macOS can fold
        // "folder created" and "file written" into one event, which the watcher treats as a
        // rescan instead of a day change, and the tests would wait forever.
        fs::create_dir_all(day_path(store.root(), DATE).unwrap().parent().unwrap()).unwrap();
        let (tx, rx) = channel();
        let (tx_index, rx_index) = channel();
        let (tx_path, rx_path) = channel();
        let handle = start(
            store.clone(),
            TEST_DEBOUNCE,
            move |c| {
                let _ = tx.send(c);
            },
            move |e| {
                let _ = tx_index.send(e);
            },
            move |p| {
                let _ = tx_path.send(p.to_path_buf());
            },
        )
        .unwrap();
        Fixture {
            _tmp: tmp,
            store,
            rx,
            rx_index,
            rx_path,
            _handle: handle,
        }
    }

    fn day_file(f: &Fixture) -> std::path::PathBuf {
        let p = day_path(f.store.root(), DATE).unwrap();
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        p
    }

    #[test]
    fn watcher_reports_external_write() {
        let f = fixture();
        fs::write(day_file(&f), "## 09:00–10:00 · A\n").unwrap();
        let changed = f.rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(changed.date, DATE);
        assert!(!changed.rev.is_empty());
    }

    #[test]
    fn watcher_reports_external_delete() {
        let f = fixture();
        let path = day_file(&f);
        fs::write(&path, "## 09:00–10:00 · A\n").unwrap();
        f.store.load(DATE).unwrap();
        // Let the creation event settle (it matches the loaded rev and is ignored).
        std::thread::sleep(Duration::from_millis(500));
        while f.rx.try_recv().is_ok() {}
        fs::remove_file(&path).unwrap();
        let changed = f.rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(changed.date, DATE);
        assert_eq!(changed.rev, "");
    }

    #[test]
    fn watcher_ignores_own_save() {
        let f = fixture();
        let loaded = f.store.load(DATE).unwrap();
        let block = Block {
            start: 540,
            end: 600,
            title: "A".into(),
            tags: vec![],
            body: "note\n".into(),
        };
        f.store.save(DATE, &[block], &loaded.rev).unwrap();
        assert_eq!(
            f.rx.recv_timeout(Duration::from_millis(1500)),
            Err(RecvTimeoutError::Timeout)
        );
    }

    #[test]
    fn watcher_ignores_config_json() {
        let f = fixture();
        fs::write(f.store.root().join("config.json"), "{}").unwrap();
        assert_eq!(
            f.rx.recv_timeout(Duration::from_millis(1500)),
            Err(RecvTimeoutError::Timeout)
        );
    }

    /// Waits until the index channel yields `wanted`, ignoring other events.
    fn wait_for(f: &Fixture, wanted: &IndexEvent) -> bool {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) {
            match f.rx_index.recv_timeout(left) {
                Ok(e) if &e == wanted => return true,
                Ok(_) => {}
                Err(_) => return false,
            }
        }
        false
    }

    fn settle(f: &Fixture) {
        std::thread::sleep(Duration::from_millis(600));
        while f.rx_index.try_recv().is_ok() {}
        while f.rx.try_recv().is_ok() {}
    }

    #[test]
    fn watcher_notifies_index_for_own_save() {
        let f = fixture();
        let loaded = f.store.load(DATE).unwrap();
        let block = Block {
            start: 540,
            end: 600,
            title: "A".into(),
            tags: vec![],
            body: "note\n".into(),
        };
        f.store.save(DATE, &[block], &loaded.rev).unwrap();
        assert!(wait_for(&f, &IndexEvent::Day(DATE.into())));
        // ...while the frontend is still not told about its own write.
        assert_eq!(
            f.rx.recv_timeout(Duration::from_millis(800)),
            Err(RecvTimeoutError::Timeout)
        );
    }

    #[test]
    fn watcher_notifies_index_for_external_write_and_delete() {
        let f = fixture();
        let path = day_file(&f);
        fs::write(&path, "## 09:00–10:00 · A\n").unwrap();
        assert!(wait_for(&f, &IndexEvent::Day(DATE.into())));
        settle(&f);
        fs::remove_file(&path).unwrap();
        assert!(wait_for(&f, &IndexEvent::Day(DATE.into())));
    }

    #[test]
    fn watcher_ignores_non_day_files_for_index() {
        let f = fixture();
        fs::write(f.store.root().join("config.json"), "{}").unwrap();
        fs::write(f.store.root().join(".catenga-tmp-x"), "x").unwrap();
        fs::remove_file(f.store.root().join(".catenga-tmp-x")).unwrap();
        assert_eq!(
            f.rx_index.recv_timeout(Duration::from_millis(1500)),
            Err(RecvTimeoutError::Timeout)
        );
    }

    #[test]
    fn watcher_reports_sidecar_changes_to_index_only() {
        let f = fixture();
        let p = day_file(&f).with_file_name(format!("{DATE}.sessions.json"));
        settle(&f);
        fs::write(&p, r#"{"version":1,"sessions":[]}"#).unwrap();
        assert!(wait_for(&f, &IndexEvent::Sessions(DATE.into())));
        // The frontend is not told through day-changed, and no day reindex is requested.
        assert_eq!(
            f.rx.recv_timeout(Duration::from_millis(1000)),
            Err(RecvTimeoutError::Timeout)
        );
        fs::remove_file(&p).unwrap();
        assert!(wait_for(&f, &IndexEvent::Sessions(DATE.into())));
    }

    #[test]
    fn watcher_ignores_goals_json_now_that_goals_live_in_the_database() {
        let f = fixture();
        settle(&f);
        let p = f.store.root().join("goals.json");
        fs::write(&p, r#"{"version":1,"goals":[]}"#).unwrap();
        fs::rename(&p, f.store.root().join("goals.json.imported")).unwrap();
        assert_eq!(
            f.rx.recv_timeout(Duration::from_millis(1000)),
            Err(RecvTimeoutError::Timeout)
        );
        assert_eq!(
            f.rx_index.recv_timeout(Duration::from_millis(300)),
            Err(RecvTimeoutError::Timeout)
        );
    }

    #[test]
    fn watcher_requests_rescan_when_directory_removed() {
        let f = fixture();
        let path = day_file(&f);
        fs::write(&path, "## 09:00–10:00 · A\n").unwrap();
        settle(&f);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
        assert!(wait_for(&f, &IndexEvent::Rescan));
    }

    #[test]
    fn watcher_reports_config_json_and_sidecars_to_the_path_hook() {
        let f = fixture();
        settle(&f);
        while f.rx_path.try_recv().is_ok() {}
        let config = f.store.root().join("config.json");
        fs::write(&config, "{}").unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut seen = false;
        while let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) {
            match f.rx_path.recv_timeout(left) {
                Ok(p) if p == config => {
                    seen = true;
                    break;
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        assert!(
            seen,
            "config.json reaches the hook, which the day events never did"
        );
    }
}
