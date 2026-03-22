use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct SessionEntry {
    session_file: String,
    created_at: String,
    last_active: String,
}

pub struct PiSessionStore {
    path: PathBuf,
}

impl PiSessionStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    fn read_map(&self) -> HashMap<String, SessionEntry> {
        std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|data| serde_json::from_str(&data).ok())
            .unwrap_or_default()
    }

    fn write_map(&self, map: &HashMap<String, SessionEntry>) {
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(map) {
            let _ = std::fs::write(&self.path, json);
        }
    }

    /// Load session file path for a history_key. Returns None if not found.
    pub fn load(&self, history_key: &str) -> Option<String> {
        self.read_map()
            .get(history_key)
            .map(|e| e.session_file.clone())
    }

    /// Save/update session file path for a history_key.
    pub fn save(&self, history_key: &str, session_file: &str) {
        let mut map = self.read_map();
        let now = chrono::Utc::now().to_rfc3339();
        match map.get_mut(history_key) {
            Some(entry) => {
                entry.session_file = session_file.to_string();
                entry.last_active = now;
            }
            None => {
                map.insert(
                    history_key.to_string(),
                    SessionEntry {
                        session_file: session_file.to_string(),
                        created_at: now.clone(),
                        last_active: now,
                    },
                );
            }
        }
        self.write_map(&map);
    }

    /// Delete session entry for a history_key.
    pub fn delete(&self, history_key: &str) {
        let mut map = self.read_map();
        if map.remove(history_key).is_some() {
            self.write_map(&map);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn save_and_load_session() {
        let dir = tempfile::tempdir().unwrap();
        let store = PiSessionStore::new(dir.path().join("sessions.json"));
        store.save("chat_1", "/path/to/session.jsonl");
        assert_eq!(store.load("chat_1"), Some("/path/to/session.jsonl".into()));
        assert_eq!(store.load("chat_2"), None);
    }

    #[test]
    fn delete_session() {
        let dir = tempfile::tempdir().unwrap();
        let store = PiSessionStore::new(dir.path().join("sessions.json"));
        store.save("chat_1", "/path/session.jsonl");
        store.delete("chat_1");
        assert_eq!(store.load("chat_1"), None);
    }

    #[test]
    fn save_updates_existing() {
        let dir = tempfile::tempdir().unwrap();
        let store = PiSessionStore::new(dir.path().join("sessions.json"));
        store.save("chat_1", "/old/path.jsonl");
        store.save("chat_1", "/new/path.jsonl");
        assert_eq!(store.load("chat_1"), Some("/new/path.jsonl".into()));
    }

    #[test]
    fn load_nonexistent_file_returns_none() {
        let store = PiSessionStore::new(PathBuf::from("/tmp/nonexistent_pi_sessions_test.json"));
        assert_eq!(store.load("anything"), None);
    }
}
