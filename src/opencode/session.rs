//! Persistent mapping from ZeroClaw `history_key` to OpenCode session IDs.
//!
//! Backed by a JSON file on disk. Reads and writes synchronously (the file is
//! small and written infrequently). Mirrors the pattern of `src/pi/session.rs`.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};

// ── Types ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
struct SessionEntry {
    session_id: String,
    created_at: String,  // RFC3339
    last_active: String, // RFC3339
}

/// Persistent store mapping `history_key → opencode_session_id`.
pub struct OpenCodeSessionStore {
    path: PathBuf,
}

// ── Implementation ────────────────────────────────────────────────────────────

impl OpenCodeSessionStore {
    /// Create a new store backed by `path`.
    ///
    /// The file is created lazily on the first write.
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Return the path to the backing JSON file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Retrieve the OpenCode session ID for the given history key.
    pub fn get(&self, history_key: &str) -> Option<String> {
        self.read_map()
            .get(history_key)
            .map(|e| e.session_id.clone())
    }

    /// Store or update the OpenCode session ID for the given history key.
    pub fn set(&self, history_key: &str, session_id: &str) {
        let mut map = self.read_map();
        let now = Utc::now().to_rfc3339();
        if let Some(entry) = map.get_mut(history_key) {
            entry.session_id = session_id.to_string();
            entry.last_active = now;
        } else {
            map.insert(
                history_key.to_string(),
                SessionEntry {
                    session_id: session_id.to_string(),
                    created_at: now.clone(),
                    last_active: now,
                },
            );
        }
        self.write_map(&map);
    }

    /// Remove the entry for `history_key`. No-op if not present.
    pub fn remove(&self, history_key: &str) {
        let mut map = self.read_map();
        if map.remove(history_key).is_some() {
            self.write_map(&map);
        }
    }

    /// Load a snapshot of all `history_key → session_id` pairs.
    pub fn load_from_disk(&self) -> HashMap<String, String> {
        self.read_map()
            .into_iter()
            .map(|(k, v)| (k, v.session_id))
            .collect()
    }

    /// Persist a `history_key → session_id` map, overwriting the store.
    pub fn save_to_disk(&self, map: &HashMap<String, String>) {
        let entries: HashMap<String, SessionEntry> = map
            .iter()
            .map(|(k, v)| {
                let now = Utc::now().to_rfc3339();
                (
                    k.clone(),
                    SessionEntry {
                        session_id: v.clone(),
                        created_at: now.clone(),
                        last_active: now,
                    },
                )
            })
            .collect();
        self.write_map(&entries);
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    fn read_map(&self) -> HashMap<String, SessionEntry> {
        match std::fs::read_to_string(&self.path) {
            Ok(data) => match serde_json::from_str(&data) {
                Ok(map) => map,
                Err(e) => {
                    tracing::error!(
                        path = %self.path.display(),
                        error = %e,
                        "opencode sessions.json corrupted, starting empty"
                    );
                    HashMap::default()
                }
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => HashMap::default(),
            Err(e) => {
                tracing::warn!(
                    path = %self.path.display(),
                    error = %e,
                    "could not read opencode sessions.json"
                );
                HashMap::default()
            }
        }
    }

    fn write_map(&self, map: &HashMap<String, SessionEntry>) {
        if let Some(parent) = self.path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::warn!(path = %parent.display(), error = %e, "create_dir_all failed");
                return;
            }
        }
        match serde_json::to_string_pretty(map) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&self.path, json) {
                    tracing::warn!(path = %self.path.display(), error = %e, "write sessions.json failed");
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "serialize sessions.json failed");
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_store() -> (OpenCodeSessionStore, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let store = OpenCodeSessionStore::new(dir.path().join("sessions.json"));
        (store, dir)
    }

    #[test]
    fn set_and_get() {
        let (store, _dir) = make_store();
        store.set("key1", "ses_abc");
        assert_eq!(store.get("key1").as_deref(), Some("ses_abc"));
    }

    #[test]
    fn get_missing_returns_none() {
        let (store, _dir) = make_store();
        assert!(store.get("nonexistent").is_none());
    }

    #[test]
    fn set_updates_existing() {
        let (store, _dir) = make_store();
        store.set("key1", "ses_old");
        store.set("key1", "ses_new");
        assert_eq!(store.get("key1").as_deref(), Some("ses_new"));
    }

    #[test]
    fn remove_existing_key() {
        let (store, _dir) = make_store();
        store.set("key1", "ses_abc");
        store.remove("key1");
        assert!(store.get("key1").is_none());
    }

    #[test]
    fn remove_nonexistent_noop() {
        let (store, _dir) = make_store();
        store.remove("nonexistent"); // must not panic
    }

    #[test]
    fn load_nonexistent_file_returns_empty() {
        let dir = tempdir().unwrap();
        let store = OpenCodeSessionStore::new(dir.path().join("does_not_exist.json"));
        assert!(store.load_from_disk().is_empty());
    }

    #[test]
    fn corrupted_json_returns_empty_no_panic() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sessions.json");
        std::fs::write(&path, "not json at all").unwrap();
        let store = OpenCodeSessionStore::new(path);
        assert!(store.get("any").is_none()); // must not panic
    }

    #[test]
    fn load_from_disk_returns_all_keys() {
        let (store, _dir) = make_store();
        store.set("k1", "s1");
        store.set("k2", "s2");
        store.set("k3", "s3");
        let map = store.load_from_disk();
        assert_eq!(map.len(), 3);
        assert_eq!(map.get("k1").map(|s| s.as_str()), Some("s1"));
    }

    #[test]
    fn save_to_disk_round_trip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sessions.json");
        let store1 = OpenCodeSessionStore::new(path.clone());
        let mut input = HashMap::new();
        input.insert("k1".to_string(), "s1".to_string());
        input.insert("k2".to_string(), "s2".to_string());
        store1.save_to_disk(&input);

        let store2 = OpenCodeSessionStore::new(path);
        let loaded = store2.load_from_disk();
        assert_eq!(loaded.get("k1").map(|s| s.as_str()), Some("s1"));
        assert_eq!(loaded.get("k2").map(|s| s.as_str()), Some("s2"));
    }
}
