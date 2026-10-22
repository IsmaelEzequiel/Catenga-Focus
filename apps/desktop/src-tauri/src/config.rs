//! `config.json` inside the notes folder: `tagColors`, `calendarRefreshMinutes` and the
//! Pomodoro lengths and cycle count.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    /// tag name -> palette name (`sun|clay|leaf|sky`)
    #[serde(default)]
    pub tag_colors: BTreeMap<String, String>,
    /// How often the visible day is re-fetched from Google Calendar.
    #[serde(default = "default_refresh_minutes")]
    pub calendar_refresh_minutes: u32,
    /// Pomodoro focus length in minutes.
    #[serde(default = "default_focus_minutes")]
    pub pomodoro_focus_minutes: u32,
    /// Pomodoro break length in minutes.
    #[serde(default = "default_break_minutes")]
    pub pomodoro_break_minutes: u32,
    /// Pomodoros (focus + break) in one run.
    #[serde(default = "default_cycles")]
    pub pomodoro_cycles: u32,
}

fn default_cycles() -> u32 {
    4
}

fn default_focus_minutes() -> u32 {
    25
}

fn default_break_minutes() -> u32 {
    5
}

fn default_refresh_minutes() -> u32 {
    5
}

impl Default for Config {
    fn default() -> Self {
        Config {
            tag_colors: BTreeMap::new(),
            calendar_refresh_minutes: default_refresh_minutes(),
            pomodoro_focus_minutes: default_focus_minutes(),
            pomodoro_break_minutes: default_break_minutes(),
            pomodoro_cycles: default_cycles(),
        }
    }
}

/// Reads `<root>/config.json`. A missing file is created with defaults; an
/// existing file is never rewritten, and invalid JSON is an error.
pub fn load_or_create(root: &Path) -> Result<Config, String> {
    let path = root.join("config.json");
    match fs::read_to_string(&path) {
        Ok(text) => {
            serde_json::from_str(&text).map_err(|e| format!("parsing {}: {e}", path.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let config = Config::default();
            let json = serde_json::to_string_pretty(&config)
                .map_err(|e| format!("serializing config: {e}"))?;
            crate::atomic::write_atomic(&path, (json + "\n").as_bytes())?;
            Ok(config)
        }
        Err(e) => Err(format!("reading {}: {e}", path.display())),
    }
}

#[cfg(test)]
pub(crate) mod test_util {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    /// Unique empty directory under the OS temp dir; removed on drop.
    pub struct TempDir(pub PathBuf);

    impl TempDir {
        pub fn new() -> TempDir {
            let n = COUNTER.fetch_add(1, Ordering::SeqCst);
            let dir = std::env::temp_dir().join(format!(
                "catenga-test-{}-{n}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_util::TempDir;
    use super::*;

    #[test]
    fn load_creates_default_when_missing() {
        let tmp = TempDir::new();
        let root = tmp.0.join("notes");
        let config = load_or_create(&root).unwrap();
        assert_eq!(config, Config::default());
        let written = fs::read_to_string(root.join("config.json")).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&written).unwrap(),
            serde_json::json!({
                "tagColors": {},
                "calendarRefreshMinutes": 5,
                "pomodoroFocusMinutes": 25,
                "pomodoroBreakMinutes": 5,
                "pomodoroCycles": 4
            })
        );
    }

    #[test]
    fn load_creates_default_without_temp_leftovers() {
        let tmp = TempDir::new();
        load_or_create(&tmp.0).unwrap();
        let names: Vec<_> = fs::read_dir(&tmp.0)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["config.json"]);
    }

    #[test]
    fn load_reads_tag_colors() {
        let tmp = TempDir::new();
        fs::write(
            tmp.0.join("config.json"),
            r#"{"tagColors":{"headline":"sky","mestrado":"leaf"}}"#,
        )
        .unwrap();
        let config = load_or_create(&tmp.0).unwrap();
        assert_eq!(config.tag_colors["headline"], "sky");
        assert_eq!(config.tag_colors["mestrado"], "leaf");
    }

    #[test]
    fn load_ignores_unknown_keys() {
        let tmp = TempDir::new();
        fs::write(
            tmp.0.join("config.json"),
            r#"{"tagColors":{"a":"sun"},"refreshInterval":5}"#,
        )
        .unwrap();
        assert_eq!(load_or_create(&tmp.0).unwrap().tag_colors.len(), 1);
    }

    #[test]
    fn load_missing_tag_colors_defaults_empty() {
        let tmp = TempDir::new();
        fs::write(tmp.0.join("config.json"), "{}").unwrap();
        assert!(load_or_create(&tmp.0).unwrap().tag_colors.is_empty());
    }

    #[test]
    fn load_missing_refresh_minutes_defaults_to_five_without_rewriting() {
        let tmp = TempDir::new();
        fs::write(tmp.0.join("config.json"), r#"{"tagColors":{}}"#).unwrap();
        assert_eq!(load_or_create(&tmp.0).unwrap().calendar_refresh_minutes, 5);
        assert_eq!(
            fs::read_to_string(tmp.0.join("config.json")).unwrap(),
            r#"{"tagColors":{}}"#
        );
    }

    #[test]
    fn load_reads_refresh_minutes_and_serializes_camel_case() {
        let tmp = TempDir::new();
        fs::write(
            tmp.0.join("config.json"),
            r#"{"calendarRefreshMinutes":12}"#,
        )
        .unwrap();
        let config = load_or_create(&tmp.0).unwrap();
        assert_eq!(config.calendar_refresh_minutes, 12);
        assert_eq!(
            serde_json::to_value(&config).unwrap()["calendarRefreshMinutes"],
            12
        );
    }

    #[test]
    fn load_invalid_json_errors_and_does_not_overwrite() {
        let tmp = TempDir::new();
        fs::write(tmp.0.join("config.json"), "{ nope").unwrap();
        assert!(load_or_create(&tmp.0).is_err());
        assert_eq!(
            fs::read_to_string(tmp.0.join("config.json")).unwrap(),
            "{ nope"
        );
    }

    #[test]
    fn missing_pomodoro_lengths_default_without_rewriting() {
        let tmp = TempDir::new();
        fs::write(tmp.0.join("config.json"), r#"{"tagColors":{}}"#).unwrap();
        let config = load_or_create(&tmp.0).unwrap();
        assert_eq!(config.pomodoro_focus_minutes, 25);
        assert_eq!(config.pomodoro_break_minutes, 5);
        assert_eq!(config.pomodoro_cycles, 4);
        assert_eq!(
            fs::read_to_string(tmp.0.join("config.json")).unwrap(),
            r#"{"tagColors":{}}"#
        );
    }

    #[test]
    fn reads_pomodoro_lengths_and_serializes_camel_case() {
        let tmp = TempDir::new();
        fs::write(
            tmp.0.join("config.json"),
            r#"{"pomodoroFocusMinutes":1,"pomodoroBreakMinutes":2}"#,
        )
        .unwrap();
        let config = load_or_create(&tmp.0).unwrap();
        assert_eq!(
            (config.pomodoro_focus_minutes, config.pomodoro_break_minutes),
            (1, 2)
        );
        let v = serde_json::to_value(&config).unwrap();
        assert_eq!(v["pomodoroFocusMinutes"], 1);
        assert_eq!(v["pomodoroBreakMinutes"], 2);
    }

    #[test]
    fn reads_pomodoro_cycles_and_serializes_camel_case() {
        let tmp = TempDir::new();
        fs::write(tmp.0.join("config.json"), r#"{"pomodoroCycles":6}"#).unwrap();
        let config = load_or_create(&tmp.0).unwrap();
        assert_eq!(config.pomodoro_cycles, 6);
        assert_eq!(serde_json::to_value(&config).unwrap()["pomodoroCycles"], 6);
    }
}
