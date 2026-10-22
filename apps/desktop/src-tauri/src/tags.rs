//! Tags as saved entities (name + palette color) in `catenga.sqlite`, merged
//! with the tags that only exist as `#text` in notes (the index).

use crate::app_db::{AppDb, flag_in, set_flag_in};
use crate::config::Config;
use crate::search::norm_tag;
use rusqlite::{Connection, params};
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::ErrorKind;
use std::path::Path;

/// The theme palette names a tag may have.
pub const COLORS: [&str; 4] = ["sun", "clay", "leaf", "sky"];
pub const DEFAULT_COLOR: &str = "sun";
const CONFIG_IMPORTED: &str = "imported_config_tag_colors";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Tag {
    /// `None` for a tag that is only text in notes (not saved).
    pub id: Option<i64>,
    pub name: String,
    pub color: String,
    pub saved: bool,
    pub goal_count: i64,
}

/// Display name of a tag: no `#`, no surrounding space, no inner space.
pub fn clean_name(name: &str) -> Result<String, String> {
    let name = name.trim().trim_start_matches('#').trim();
    if name.is_empty() {
        return Err("Tag name is empty".into());
    }
    if name.chars().any(char::is_whitespace) {
        return Err("Tag names can't contain spaces".into());
    }
    Ok(name.to_string())
}

fn check_color(color: &str) -> Result<(), String> {
    COLORS
        .contains(&color)
        .then_some(())
        .ok_or_else(|| format!("Tag color must be one of {}", COLORS.join(", ")))
}

/// Saved tags plus every index-only tag (color `sun`, `saved: false`), by name.
pub fn merge_tags(saved: Vec<Tag>, index_tags: &[String]) -> Vec<Tag> {
    let mut seen: HashSet<String> = saved.iter().map(|t| norm_tag(&t.name)).collect();
    let mut all = saved;
    for name in index_tags {
        let key = norm_tag(name);
        if !key.is_empty() && seen.insert(key) {
            all.push(Tag {
                id: None,
                name: name.trim().trim_start_matches('#').to_string(),
                color: DEFAULT_COLOR.into(),
                saved: false,
                goal_count: 0,
            });
        }
    }
    all.sort_by_cached_key(|t| (norm_tag(&t.name), t.id));
    all
}

/// Inserts the tag unless one with the same normalised name exists; returns
/// its id and whether it was created.
pub fn ensure_tag(
    conn: &Connection,
    name: &str,
    color: &str,
    now: i64,
) -> rusqlite::Result<(i64, bool)> {
    let key = norm_tag(name);
    let existing = conn.query_row("SELECT id FROM tags WHERE name_norm = ?1", [&key], |r| {
        r.get(0)
    });
    match existing {
        Ok(id) => Ok((id, false)),
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            conn.execute(
                "INSERT INTO tags(name, name_norm, color, created_at) VALUES (?1, ?2, ?3, ?4)",
                params![name, key, color, now],
            )?;
            Ok((conn.last_insert_rowid(), true))
        }
        Err(e) => Err(e),
    }
}

pub fn saved_tags_in(conn: &Connection) -> rusqlite::Result<Vec<Tag>> {
    let mut stmt = conn.prepare(
        "SELECT t.id, t.name, t.color,
                (SELECT count(*) FROM goal_tags g WHERE g.tag_id = t.id)
         FROM tags t ORDER BY t.name_norm",
    )?;
    stmt.query_map([], |r| {
        Ok(Tag {
            id: Some(r.get(0)?),
            name: r.get(1)?,
            color: r.get(2)?,
            saved: true,
            goal_count: r.get(3)?,
        })
    })?
    .collect()
}

impl AppDb {
    pub fn saved_tags(&self) -> Result<Vec<Tag>, String> {
        self.with_conn("reading tags", |conn| saved_tags_in(conn))
    }

    /// Saved tags merged with `index_tags` (tags seen in notes and sessions).
    pub fn tags(&self, index_tags: &[String]) -> Result<Vec<Tag>, String> {
        Ok(merge_tags(self.saved_tags()?, index_tags))
    }

    /// Creates the tag or, when it exists, changes its color.
    pub fn tag_save(&self, name: &str, color: &str, now: i64) -> Result<(), String> {
        let name = clean_name(name)?;
        check_color(color)?;
        self.with_conn("saving tag", |conn| {
            let tx = conn.transaction()?;
            let (id, created) = ensure_tag(&tx, &name, color, now)?;
            if !created {
                tx.execute(
                    "UPDATE tags SET color = ?2 WHERE id = ?1",
                    params![id, color],
                )?;
            }
            tx.commit()
        })
    }

    /// Only a saved tag no goal uses; tags inside notes are not touched.
    pub fn tag_delete(&self, id: i64) -> Result<(), String> {
        self.with_conn("deleting tag", |conn| {
            let tx = conn.transaction()?;
            let used: Option<i64> = tx
                .query_row(
                    "SELECT (SELECT count(*) FROM goal_tags WHERE tag_id = ?1)
                     FROM tags WHERE id = ?1",
                    [id],
                    |r| r.get(0),
                )
                .map(Some)
                .or_else(|e| match e {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    e => Err(e),
                })?;
            let result = match used {
                None => Err("Tag not found"),
                Some(n) if n > 0 => Err("A goal uses this tag"),
                Some(_) => {
                    tx.execute("DELETE FROM tags WHERE id = ?1", [id])?;
                    Ok(())
                }
            };
            tx.commit()?;
            Ok(result)
        })?
        .map_err(str::to_string)
    }

    /// `name -> color` of the saved tags, the shape `config.json` used to have.
    pub fn tag_colors(&self) -> Result<BTreeMap<String, String>, String> {
        Ok(self
            .saved_tags()?
            .into_iter()
            .map(|t| (t.name, t.color))
            .collect())
    }

    /// One-time import of `config.json`'s `tagColors`; the file is not rewritten.
    /// A missing file imports nothing (and will be tried again); an invalid one
    /// is an error.
    pub fn import_config_tag_colors(&self, root: &Path, now: i64) -> Result<(), String> {
        if self.flag(CONFIG_IMPORTED)? {
            return Ok(());
        }
        let path = root.join("config.json");
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(format!("reading {}: {e}", path.display())),
        };
        let config: Config =
            serde_json::from_str(&text).map_err(|e| format!("parsing {}: {e}", path.display()))?;
        self.with_conn("importing tag colors", |conn| {
            let tx = conn.transaction()?;
            if flag_in(&tx, CONFIG_IMPORTED)? {
                return Ok(());
            }
            for (name, color) in &config.tag_colors {
                let Ok(name) = clean_name(name) else { continue };
                let color = if COLORS.contains(&color.as_str()) {
                    color.as_str()
                } else {
                    DEFAULT_COLOR
                };
                ensure_tag(&tx, &name, color, now)?;
            }
            set_flag_in(&tx, CONFIG_IMPORTED)?;
            tx.commit()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_db::test_util::fixture;

    fn names(tags: &[Tag]) -> Vec<(&str, bool)> {
        tags.iter().map(|t| (t.name.as_str(), t.saved)).collect()
    }

    #[test]
    fn names_are_cleaned_and_validated() {
        assert_eq!(clean_name("  #Rust "), Ok("Rust".into()));
        assert_eq!(clean_name("##x"), Ok("x".into()));
        assert_eq!(clean_name("  "), Err("Tag name is empty".into()));
        assert_eq!(clean_name("#"), Err("Tag name is empty".into()));
        assert_eq!(
            clean_name("two words"),
            Err("Tag names can't contain spaces".into())
        );
    }

    #[test]
    fn save_creates_then_recolors_keeping_the_display_name() {
        let f = fixture();
        f.db.tag_save("#Mestrado", "leaf", 1).unwrap();
        f.db.tag_save("mestrado", "clay", 2).unwrap();
        let tags = f.db.saved_tags().unwrap();
        assert_eq!(tags.len(), 1);
        assert_eq!(
            (tags[0].name.as_str(), tags[0].color.as_str(), tags[0].saved),
            ("Mestrado", "clay", true)
        );
        assert!(tags[0].id.is_some());
    }

    #[test]
    fn save_rejects_bad_colors_and_names_without_writing() {
        let f = fixture();
        assert!(
            f.db.tag_save("x", "#ff0000", 1)
                .unwrap_err()
                .contains("sun, clay, leaf, sky")
        );
        assert!(f.db.tag_save("  ", "sun", 1).is_err());
        assert!(f.db.saved_tags().unwrap().is_empty());
    }

    #[test]
    fn union_adds_index_only_tags_as_unsaved_sun() {
        let f = fixture();
        f.db.tag_save("rust", "sky", 1).unwrap();
        let tags =
            f.db.tags(&["Rust".into(), "go".into(), "Alpha".into(), "go".into()])
                .unwrap();
        assert_eq!(
            names(&tags),
            [("Alpha", false), ("go", false), ("rust", true)]
        );
        let go = &tags[1];
        assert_eq!((go.id, go.color.as_str(), go.goal_count), (None, "sun", 0));
        assert_eq!(
            tags[2].color, "sky",
            "the saved tag wins over the index twin"
        );
    }

    #[test]
    fn tag_json_shape() {
        let f = fixture();
        f.db.tag_save("rust", "sky", 1).unwrap();
        let tags = f.db.tags(&["go".into()]).unwrap();
        assert_eq!(
            serde_json::to_value(&tags).unwrap(),
            serde_json::json!([
                {"id": null, "name": "go", "color": "sun", "saved": false, "goalCount": 0},
                {"id": 1, "name": "rust", "color": "sky", "saved": true, "goalCount": 0}
            ])
        );
    }

    #[test]
    fn delete_removes_a_free_tag_and_reports_unknown_ones() {
        let f = fixture();
        f.db.tag_save("a", "sun", 1).unwrap();
        let id = f.db.saved_tags().unwrap()[0].id.unwrap();
        f.db.tag_delete(id).unwrap();
        assert!(f.db.saved_tags().unwrap().is_empty());
        assert_eq!(f.db.tag_delete(id), Err("Tag not found".into()));
    }

    #[test]
    fn delete_is_refused_while_a_goal_uses_the_tag() {
        let f = fixture();
        f.db.tag_save("a", "sun", 1).unwrap();
        let id = f.db.saved_tags().unwrap()[0].id.unwrap();
        f.db.with_conn("t", |c| {
            c.execute(
                "INSERT INTO goals(minutes_per_day, start, created_at, updated_at) VALUES (60,'2026-10-21',1,1)",
                [],
            )?;
            c.execute("INSERT INTO goal_tags(goal_id, tag_id) VALUES (1, ?1)", [id])
        })
        .unwrap();
        assert_eq!(f.db.tag_delete(id), Err("A goal uses this tag".into()));
        let tags = f.db.saved_tags().unwrap();
        assert_eq!((tags.len(), tags[0].goal_count), (1, 1));
    }

    #[test]
    fn restrict_also_protects_at_the_database_level() {
        let f = fixture();
        f.db.tag_save("a", "sun", 1).unwrap();
        let r = f.db.with_conn("t", |c| {
            c.execute(
                "INSERT INTO goals(minutes_per_day, start, created_at, updated_at) VALUES (60,'2026-10-21',1,1)",
                [],
            )?;
            c.execute("INSERT INTO goal_tags(goal_id, tag_id) VALUES (1, 1)", [])?;
            c.execute("DELETE FROM tags WHERE id = 1", [])
        });
        assert!(r.is_err());
    }

    #[test]
    fn tag_colors_are_name_to_color() {
        let f = fixture();
        f.db.tag_save("b", "leaf", 1).unwrap();
        f.db.tag_save("a", "clay", 1).unwrap();
        let colors = f.db.tag_colors().unwrap();
        assert_eq!(colors["a"], "clay");
        assert_eq!(colors["b"], "leaf");
    }

    fn write_config(f: &crate::app_db::test_util::Fixture, json: &str) -> std::path::PathBuf {
        let root = f.tmp.0.join("notes");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("config.json"), json).unwrap();
        root
    }

    #[test]
    fn config_import_is_one_time_and_leaves_the_file_alone() {
        let f = fixture();
        let json =
            r##"{"tagColors":{"headline":"sky","#mestrado":"leaf","odd":"magenta","  ":"sun"}}"##;
        let root = write_config(&f, json);
        f.db.import_config_tag_colors(&root, 1).unwrap();
        let colors = f.db.tag_colors().unwrap();
        assert_eq!(colors.len(), 3, "{colors:?}");
        assert_eq!(colors["headline"], "sky");
        assert_eq!(colors["mestrado"], "leaf");
        assert_eq!(colors["odd"], "sun", "unknown colors fall back to sun");
        assert_eq!(fs::read_to_string(root.join("config.json")).unwrap(), json);
        // The DB is authoritative now: a later import changes nothing.
        f.db.tag_save("headline", "clay", 2).unwrap();
        fs::write(
            root.join("config.json"),
            r#"{"tagColors":{"headline":"leaf","new":"sky"}}"#,
        )
        .unwrap();
        f.db.import_config_tag_colors(&root, 3).unwrap();
        let colors = f.db.tag_colors().unwrap();
        assert_eq!((colors["headline"].as_str(), colors.len()), ("clay", 3));
    }

    #[test]
    fn config_import_never_overwrites_an_existing_tag() {
        let f = fixture();
        f.db.tag_save("Rust", "clay", 1).unwrap();
        let root = write_config(&f, r#"{"tagColors":{"rust":"sky"}}"#);
        f.db.import_config_tag_colors(&root, 2).unwrap();
        assert_eq!(f.db.tag_colors().unwrap()["Rust"], "clay");
    }

    #[test]
    fn config_import_missing_file_waits_and_invalid_file_errors() {
        let f = fixture();
        let root = f.tmp.0.join("notes");
        fs::create_dir_all(&root).unwrap();
        f.db.import_config_tag_colors(&root, 1).unwrap();
        assert!(!f.db.flag(CONFIG_IMPORTED).unwrap(), "retried later");
        fs::write(root.join("config.json"), "{ nope").unwrap();
        assert!(f.db.import_config_tag_colors(&root, 1).is_err());
        assert!(!f.db.flag(CONFIG_IMPORTED).unwrap());
        assert_eq!(
            fs::read_to_string(root.join("config.json")).unwrap(),
            "{ nope"
        );
        fs::write(root.join("config.json"), r#"{"tagColors":{"a":"leaf"}}"#).unwrap();
        f.db.import_config_tag_colors(&root, 1).unwrap();
        assert_eq!(f.db.tag_colors().unwrap().len(), 1);
    }
}
