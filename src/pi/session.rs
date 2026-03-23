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

    /// Remove session entries older than `max_age` and delete their session files from disk.
    pub fn cleanup(&self, max_age: std::time::Duration) {
        let mut map = self.read_map();
        if map.is_empty() {
            return;
        }

        let now = chrono::Utc::now();
        let mut to_remove = Vec::new();

        for (key, entry) in &map {
            if let Ok(last_active) = chrono::DateTime::parse_from_rfc3339(&entry.last_active) {
                let age = now.signed_duration_since(last_active);
                if age > chrono::Duration::from_std(max_age).unwrap_or(chrono::Duration::days(30)) {
                    to_remove.push(key.clone());
                }
            }
        }

        let mut removed = 0;
        let mut file_errors = 0;

        for key in &to_remove {
            if let Some(entry) = map.get(key) {
                match std::fs::remove_file(&entry.session_file) {
                    Ok(()) => removed += 1,
                    Err(e) => {
                        tracing::debug!(
                            path = %entry.session_file,
                            error = %e,
                            "Failed to delete Pi session file"
                        );
                        file_errors += 1;
                    }
                }
            }
            map.remove(key);
        }

        if !to_remove.is_empty() {
            self.write_map(&map);
            tracing::info!(
                entries_removed = to_remove.len(),
                files_deleted = removed,
                file_errors,
                "Pi session cleanup completed"
            );
        }
    }

    /// Path to the session store JSON file.
    pub fn path(&self) -> &std::path::Path {
        &self.path
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

    #[test]
    fn cleanup_removes_old_entries() {
        let dir = tempfile::tempdir().unwrap();
        let store = PiSessionStore::new(dir.path().join("sessions.json"));

        // Create a dummy session file so cleanup can delete it
        let session_file = dir.path().join("old_session.jsonl");
        std::fs::write(&session_file, "").unwrap();
        store.save("old_key", session_file.to_str().unwrap());

        // Also save a fresh entry that should survive cleanup
        store.save("fresh_key", "/tmp/fresh_session.jsonl");

        // Manually backdate old_key's last_active to 8 days ago
        let data = std::fs::read_to_string(store.path()).unwrap();
        let mut map: HashMap<String, serde_json::Value> = serde_json::from_str(&data).unwrap();
        map.get_mut("old_key").unwrap()["last_active"] =
            serde_json::Value::String("2020-01-01T00:00:00+00:00".to_string());
        std::fs::write(store.path(), serde_json::to_string_pretty(&map).unwrap()).unwrap();

        // Run cleanup with 7-day retention
        store.cleanup(std::time::Duration::from_secs(7 * 24 * 3600));

        // Old entry should be gone, fresh entry should remain
        assert_eq!(store.load("old_key"), None);
        assert!(store.load("fresh_key").is_some());

        // Session file should have been deleted from disk
        assert!(!session_file.exists());
    }

    #[test]
    fn cleanup_noop_on_empty_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = PiSessionStore::new(dir.path().join("sessions.json"));
        // Should not panic on missing file
        store.cleanup(std::time::Duration::from_secs(3600));
    }

    #[test]
    fn cleanup_handles_missing_session_files() {
        let dir = tempfile::tempdir().unwrap();
        let store = PiSessionStore::new(dir.path().join("sessions.json"));
        // Save with non-existent session file
        store.save("old_key", "/tmp/nonexistent_pi_session_12345.jsonl");

        // Manually backdate
        let data = std::fs::read_to_string(store.path()).unwrap();
        let mut map: HashMap<String, serde_json::Value> = serde_json::from_str(&data).unwrap();
        map.get_mut("old_key").unwrap()["last_active"] =
            serde_json::Value::String("2020-01-01T00:00:00+00:00".to_string());
        std::fs::write(store.path(), serde_json::to_string_pretty(&map).unwrap()).unwrap();

        // Cleanup should not panic even though file doesn't exist
        store.cleanup(std::time::Duration::from_secs(86400));
        assert_eq!(store.load("old_key"), None); // Entry removed from JSON
    }
}
