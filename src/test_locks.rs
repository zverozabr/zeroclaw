//! Shared test locks to avoid concurrency issues in integration tests.

use std::sync::Mutex;

/// Global lock for tests that interact with the plugin runtime.
pub static PLUGIN_RUNTIME_LOCK: Mutex<()> = Mutex::new(());
