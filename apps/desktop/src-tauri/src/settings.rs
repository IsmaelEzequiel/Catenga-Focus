//! The notes-folder choice. `config.json` lives inside the notes folder, so it cannot say
//! where that is: a bootstrap `settings.json` in the app data dir does.
//! Order at startup: `CATENGA_NOTES_DIR` (dev) -> `settings.json` -> `~/Catenga`.

use crate::atomic;
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager};

const FILE: &str = "settings.json";
const PROBE: &str = ".catenga-write-probe";

#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Settings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    notes_dir: Option<String>,
}

/// Where the notes folder came from.
#[derive(Debug, PartialEq, Eq)]
pub enum Source {
    Env,
    Settings,
    Default,
}

fn saved(data_dir: &Path) -> Option<PathBuf> {
    let text = fs::read_to_string(data_dir.join(FILE)).ok()?;
    let dir = serde_json::from_str::<Settings>(&text).ok()?.notes_dir?;
    let path = PathBuf::from(dir);
    path.is_absolute().then_some(path)
}

/// The notes folder and why. Missing or invalid `settings.json` means the default.
pub fn resolve(env: Option<OsString>, data_dir: &Path, home: &Path) -> (PathBuf, Source) {
    match env {
        Some(dir) if !dir.is_empty() => (PathBuf::from(dir), Source::Env),
        _ => match saved(data_dir) {
            Some(path) => (path, Source::Settings),
            None => (home.join("Catenga"), Source::Default),
        },
    }
}

/// Checks a folder the owner picked and returns it ready to save.
/// Absolute, a directory (created when missing), writable, and not inside `forbidden`.
pub fn validate(path: &str, forbidden: &[PathBuf]) -> Result<PathBuf, String> {
    let path = PathBuf::from(path.trim());
    if path.as_os_str().is_empty() || !path.is_absolute() {
        return Err("Choose an absolute folder path.".into());
    }
    if path.exists() && !path.is_dir() {
        return Err(format!("{} is not a folder.", path.display()));
    }
    // Check before creating anything: resolve the deepest existing ancestor, keep the rest.
    let path = resolve_future(&path)?;
    for dir in forbidden {
        let dir = fs::canonicalize(dir).unwrap_or_else(|_| dir.clone());
        if path.starts_with(&dir) {
            return Err(format!(
                "Choose a folder outside the app's own data ({}).",
                dir.display()
            ));
        }
    }
    fs::create_dir_all(&path).map_err(|e| format!("Can't use {}: {e}", path.display()))?;
    let path = fs::canonicalize(&path).map_err(|e| format!("Can't use {}: {e}", path.display()))?;
    let probe = path.join(PROBE);
    fs::write(&probe, b"")
        .and_then(|()| fs::remove_file(&probe))
        .map_err(|e| format!("{} isn't writable: {e}", path.display()))?;
    Ok(path)
}

/// `path` with its deepest existing ancestor canonicalized (symlinks resolved).
fn resolve_future(path: &Path) -> Result<PathBuf, String> {
    let mut tail = Vec::new();
    let mut base = path;
    loop {
        match fs::canonicalize(base) {
            Ok(real) => return Ok(tail.iter().rev().fold(real, |p, c| p.join(c))),
            Err(_) => {
                let (Some(name), Some(parent)) = (base.file_name(), base.parent()) else {
                    return Err(format!("Can't use {}.", path.display()));
                };
                tail.push(name.to_owned());
                base = parent;
            }
        }
    }
}

/// Saves the choice (`None` = back to the default). Atomic; nothing in any notes folder is touched.
pub fn save(data_dir: &Path, dir: Option<&Path>) -> Result<(), String> {
    let settings = Settings {
        notes_dir: dir.map(|d| d.to_string_lossy().into_owned()),
    };
    let json = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())? + "\n";
    atomic::write_atomic(&data_dir.join(FILE), json.as_bytes())
}

/// `~/...` for display.
fn tilde(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotesDirInfo {
    /// Absolute path.
    pub path: String,
    /// The same with `~` for the home folder.
    pub display: String,
    pub is_default: bool,
    pub from_env: bool,
}

fn data_dir(app: &AppHandle) -> Result<PathBuf, String> {
    app.path().app_data_dir().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn notes_dir_info(app: AppHandle) -> Result<NotesDirInfo, String> {
    let home = app.path().home_dir().map_err(|e| e.to_string())?;
    let (path, source) = resolve(
        std::env::var_os("CATENGA_NOTES_DIR"),
        &data_dir(&app)?,
        &home,
    );
    Ok(NotesDirInfo {
        display: tilde(&path, &home),
        path: path.to_string_lossy().into_owned(),
        is_default: source == Source::Default,
        from_env: source == Source::Env,
    })
}

/// Saves the folder for the next start. The frontend flushes its notes first and then
/// calls `app_restart`.
#[tauri::command]
pub fn set_notes_dir(app: AppHandle, path: Option<String>) -> Result<(), String> {
    if std::env::var_os("CATENGA_NOTES_DIR").is_some_and(|v| !v.is_empty()) {
        return Err("The folder is set by CATENGA_NOTES_DIR (development).".into());
    }
    let data = data_dir(&app)?;
    let chosen = match path {
        Some(p) => {
            let mut forbidden = vec![data.clone()];
            if let Ok(cache) = app.path().app_cache_dir() {
                forbidden.push(cache);
            }
            Some(validate(&p, &forbidden)?)
        }
        None => None,
    };
    save(&data, chosen.as_deref())
}

#[tauri::command]
pub fn app_restart(app: AppHandle) {
    app.restart();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_util::TempDir;

    fn os(s: &str) -> Option<OsString> {
        Some(OsString::from(s))
    }

    #[test]
    fn resolves_env_then_settings_then_default() {
        let tmp = TempDir::new();
        let (data, home) = (tmp.0.join("data"), tmp.0.join("home"));
        assert_eq!(
            resolve(None, &data, &home),
            (home.join("Catenga"), Source::Default)
        );
        save(&data, Some(&tmp.0.join("mine"))).unwrap();
        assert_eq!(
            resolve(None, &data, &home),
            (tmp.0.join("mine"), Source::Settings)
        );
        assert_eq!(resolve(os(""), &data, &home).1, Source::Settings);
        assert_eq!(
            resolve(os("/dev/notes"), &data, &home),
            (PathBuf::from("/dev/notes"), Source::Env)
        );
        save(&data, None).unwrap();
        assert_eq!(resolve(None, &data, &home).1, Source::Default);
    }

    #[test]
    fn missing_or_invalid_settings_mean_the_default() {
        let tmp = TempDir::new();
        let home = tmp.0.join("home");
        for bad in [
            "{ nope",
            "{}",
            "{\"notesDir\":\"relative/x\"}",
            "{\"notesDir\":3}",
        ] {
            fs::write(tmp.0.join(FILE), bad).unwrap();
            assert_eq!(resolve(None, &tmp.0, &home).1, Source::Default, "{bad}");
        }
    }

    #[test]
    fn validate_accepts_creates_and_leaves_no_probe() {
        let tmp = TempDir::new();
        let made = tmp.0.join("a").join("b");
        let ok = validate(made.to_str().unwrap(), &[]).unwrap();
        assert!(ok.is_dir());
        assert_eq!(fs::read_dir(&ok).unwrap().count(), 0);
    }

    #[test]
    fn validate_refuses_bad_folders() {
        let tmp = TempDir::new();
        let file = tmp.0.join("f");
        fs::write(&file, "x").unwrap();
        let data = tmp.0.join("data");
        fs::create_dir_all(data.join("inner")).unwrap();
        for (bad, forbidden) in [
            ("", vec![]),
            ("relative/dir", vec![]),
            (file.to_str().unwrap(), vec![]),
            (data.to_str().unwrap(), vec![data.clone()]),
            (data.join("inner").to_str().unwrap(), vec![data.clone()]),
            (data.join("new").to_str().unwrap(), vec![data.clone()]),
        ] {
            assert!(validate(bad, &forbidden).is_err(), "{bad:?}");
        }
        assert!(!data.join("new").exists(), "refused before creating");
    }

    #[cfg(unix)]
    #[test]
    fn validate_refuses_an_unwritable_folder() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new();
        let ro = tmp.0.join("ro");
        fs::create_dir_all(&ro).unwrap();
        fs::set_permissions(&ro, fs::Permissions::from_mode(0o555)).unwrap();
        let result = validate(ro.to_str().unwrap(), &[]);
        fs::set_permissions(&ro, fs::Permissions::from_mode(0o755)).unwrap();
        // Root can write anywhere; only assert when the OS enforced it.
        if fs::write(ro.join("x"), "").is_ok() {
            return;
        }
        assert!(result.is_err());
    }

    #[test]
    fn save_is_atomic_json_and_never_touches_notes() {
        let tmp = TempDir::new();
        let notes = tmp.0.join("notes");
        fs::create_dir_all(&notes).unwrap();
        fs::write(notes.join("keep.md"), "mine").unwrap();
        save(&tmp.0.join("data"), Some(&notes)).unwrap();
        let text = fs::read_to_string(tmp.0.join("data").join(FILE)).unwrap();
        assert!(text.contains("\"notesDir\""));
        assert_eq!(fs::read_to_string(notes.join("keep.md")).unwrap(), "mine");
    }

    #[test]
    fn tilde_abbreviates_the_home_folder() {
        let home = Path::new("/Users/me");
        assert_eq!(tilde(Path::new("/Users/me/Catenga"), home), "~/Catenga");
        assert_eq!(tilde(Path::new("/Users/me"), home), "~");
        assert_eq!(tilde(Path::new("/Volumes/x"), home), "/Volumes/x");
    }
}
