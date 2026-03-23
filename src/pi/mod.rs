pub mod rpc;
pub mod session;
pub mod status;
pub mod telegram;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use tokio::io::BufReader;
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex;
use tracing::{error, info};

struct PiInstance {
    process: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    session_file: Option<String>,
    last_active: Instant,
    history_injected: bool,
}

pub struct PiManager {
    instances: Mutex<HashMap<String, PiInstance>>,
    session_store: session::PiSessionStore,
    workspace_dir: PathBuf,
    minimax_key: String,
}

static PI_MANAGER: OnceLock<Arc<PiManager>> = OnceLock::new();

pub fn init_pi_manager(workspace_dir: &Path, minimax_key: &str) {
    let _ = PI_MANAGER.set(Arc::new(PiManager::new(workspace_dir, minimax_key)));
}

pub fn pi_manager() -> Option<Arc<PiManager>> {
    PI_MANAGER.get().cloned()
}

impl PiManager {
    pub fn new(workspace_dir: &Path, minimax_key: &str) -> Self {
        let session_store = session::PiSessionStore::new(workspace_dir.join("pi_sessions.json"));
        Self {
            instances: Mutex::new(HashMap::new()),
            session_store,
            workspace_dir: workspace_dir.to_path_buf(),
            minimax_key: minimax_key.to_string(),
        }
    }

    /// Spawn Pi process with MiniMax provider.
    async fn spawn_pi(&self) -> anyhow::Result<PiInstance> {
        info!(
            provider = "minimax",
            model = "MiniMax-M2.7-highspeed",
            workspace = %self.workspace_dir.display(),
            "spawning Pi process"
        );

        let mut child = tokio::process::Command::new("pi")
            .args([
                "--mode",
                "rpc",
                "--provider",
                "minimax",
                "--model",
                "MiniMax-M2.7-highspeed",
                "--thinking",
                "high",
                "--cwd",
            ])
            .arg(&self.workspace_dir)
            .env("MINIMAX_API_KEY", &self.minimax_key)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| {
                error!(error = %e, "failed to spawn Pi process");
                e
            })?;

        let stdin = child.stdin.take().expect("stdin piped");
        let stdout = child.stdout.take().expect("stdout piped");
        let reader = BufReader::new(stdout);

        // Wait for startup
        tokio::time::sleep(Duration::from_secs(4)).await;

        Ok(PiInstance {
            process: child,
            stdin,
            stdout: reader,
            session_file: None,
            last_active: Instant::now(),
            history_injected: false,
        })
    }

    /// Ensure Pi is running for this chat. Spawn + load session if needed.
    pub async fn ensure_running(&self, history_key: &str) -> anyhow::Result<()> {
        let instances = self.instances.lock().await;
        if instances.contains_key(history_key) {
            info!(history_key, "Pi already running");
            return Ok(());
        }
        drop(instances);

        info!(history_key, "spawning new Pi instance");
        let mut instance = self.spawn_pi().await?;

        // Load existing session or create new one
        if let Some(session_file) = self.session_store.load(history_key) {
            info!(history_key, session_file = %session_file, "switching to saved session");
            if rpc::rpc_switch_session(&mut instance.stdin, &mut instance.stdout, &session_file)
                .await
            {
                instance.session_file = Some(session_file);
            } else {
                info!(history_key, "switch_session failed, creating new session");
                rpc::rpc_new_session(&mut instance.stdin, &mut instance.stdout).await;
            }
        } else {
            info!(history_key, "no saved session, creating new session");
            rpc::rpc_new_session(&mut instance.stdin, &mut instance.stdout).await;
        }

        let mut instances = self.instances.lock().await;
        instances.insert(history_key.to_string(), instance);
        Ok(())
    }

    /// Send prompt to running Pi, stream events via callback, return response text.
    pub async fn prompt(
        &self,
        history_key: &str,
        message: &str,
        on_event: impl Fn(rpc::PiEvent),
    ) -> anyhow::Result<String> {
        let preview: String = message.chars().take(80).collect();
        info!(history_key, message_preview = %preview, "sending prompt to Pi");

        let start = Instant::now();
        let mut instances = self.instances.lock().await;
        let instance = instances
            .get_mut(history_key)
            .ok_or_else(|| anyhow::anyhow!("no Pi instance for {}", history_key))?;

        instance.last_active = Instant::now();

        let result = rpc::rpc_prompt(
            &mut instance.stdin,
            &mut instance.stdout,
            message,
            Duration::from_secs(300),
            |ev| on_event(ev.clone()),
        )
        .await;

        match &result {
            Ok(text) => {
                let elapsed = start.elapsed().as_millis();
                info!(
                    history_key,
                    response_len = text.len(),
                    elapsed_ms = elapsed,
                    "prompt completed"
                );

                // Save session file after successful prompt
                if let Some(sf) =
                    rpc::rpc_get_session_file(&mut instance.stdin, &mut instance.stdout).await
                {
                    instance.session_file = Some(sf.clone());
                    self.session_store.save(history_key, &sf);
                }
            }
            Err(e) => {
                error!(history_key, error = %e, "prompt error");
            }
        }

        result
    }

    /// Stop Pi for a chat -- save session, kill process.
    pub async fn stop(&self, history_key: &str) -> anyhow::Result<()> {
        let mut instances = self.instances.lock().await;
        let mut instance = instances
            .remove(history_key)
            .ok_or_else(|| anyhow::anyhow!("no Pi instance for {}", history_key))?;

        // Try to get session file before killing
        if let Some(sf) = rpc::rpc_get_session_file(&mut instance.stdin, &mut instance.stdout).await
        {
            self.session_store.save(history_key, &sf);
            info!(history_key, session_file = %sf, "session saved, stopping Pi");
        } else if let Some(sf) = &instance.session_file {
            self.session_store.save(history_key, sf);
            info!(history_key, session_file = %sf, "session saved (cached), stopping Pi");
        } else {
            info!(history_key, "no session file to save, stopping Pi");
        }

        let _ = instance.process.kill().await;
        Ok(())
    }

    /// Stop all Pi instances (shutdown hook).
    pub async fn stop_all(&self) {
        let mut instances = self.instances.lock().await;
        let keys: Vec<String> = instances.keys().cloned().collect();
        for key in keys {
            if let Some(mut instance) = instances.remove(&key) {
                if let Some(sf) =
                    rpc::rpc_get_session_file(&mut instance.stdin, &mut instance.stdout).await
                {
                    self.session_store.save(&key, &sf);
                }
                let _ = instance.process.kill().await;
                info!(history_key = %key, "stopped Pi instance");
            }
        }
    }

    /// Kill instances idle for longer than max_idle.
    pub async fn kill_idle(&self, max_idle: Duration) {
        let mut instances = self.instances.lock().await;
        let idle_keys: Vec<String> = instances
            .iter()
            .filter(|(_, inst)| inst.last_active.elapsed() > max_idle)
            .map(|(k, _)| k.clone())
            .collect();

        for key in idle_keys {
            if let Some(mut instance) = instances.remove(&key) {
                let idle_secs = instance.last_active.elapsed().as_secs();
                if let Some(sf) =
                    rpc::rpc_get_session_file(&mut instance.stdin, &mut instance.stdout).await
                {
                    self.session_store.save(&key, &sf);
                }
                let _ = instance.process.kill().await;
                info!(history_key = %key, idle_secs, "killed idle Pi instance");
            }
        }
    }

    /// Check if Pi is running for this history_key.
    pub async fn is_running(&self, history_key: &str) -> bool {
        self.instances.lock().await.contains_key(history_key)
    }

    /// Check if history needs injection (first activation).
    pub async fn needs_history_injection(&self, history_key: &str) -> bool {
        self.instances
            .lock()
            .await
            .get(history_key)
            .map_or(false, |inst| !inst.history_injected)
    }

    /// Insert a fake instance for testing (bypasses spawn_pi).
    #[cfg(test)]
    async fn insert_test_instance(&self, key: &str, last_active: Instant) {
        // Use `true` as a dummy process — exits immediately, so when
        // kill_idle tries rpc_get_session_file the broken-pipe send fails
        // and returns None instantly (no 10s timeout).
        let mut child = tokio::process::Command::new("true")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("failed to spawn true for test");

        let stdin = child.stdin.take().expect("stdin piped");
        let stdout = child.stdout.take().expect("stdout piped");

        let instance = PiInstance {
            process: child,
            stdin,
            stdout: BufReader::new(stdout),
            session_file: None,
            last_active,
            history_injected: false,
        };

        self.instances.lock().await.insert(key.to_string(), instance);
    }

    /// Inject ZeroClaw conversation history into Pi as context.
    pub async fn inject_history(
        &self,
        history_key: &str,
        messages: &[crate::providers::ChatMessage],
        token_limit: usize,
    ) -> anyhow::Result<()> {
        // Format messages as context block
        let mut formatted = Vec::new();
        for msg in messages {
            let role = if msg.role == "user" {
                "User"
            } else {
                "Assistant"
            };
            formatted.push(format!("{}: {}", role, msg.content));
        }

        // Estimate 4 chars per token, take last N messages fitting token_limit
        let char_limit = token_limit * 4;
        let mut total_chars = 0;
        let mut start_idx = formatted.len();
        for (i, line) in formatted.iter().enumerate().rev() {
            total_chars += line.len() + 1; // +1 for newline
            if total_chars > char_limit {
                break;
            }
            start_idx = i;
        }

        let context = format!("[Context]\n{}", formatted[start_idx..].join("\n"));

        let mut instances = self.instances.lock().await;
        let instance = instances
            .get_mut(history_key)
            .ok_or_else(|| anyhow::anyhow!("no Pi instance for {}", history_key))?;

        rpc::rpc_prompt(
            &mut instance.stdin,
            &mut instance.stdout,
            &context,
            Duration::from_secs(300),
            |_| {},
        )
        .await?;

        instance.history_injected = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn kill_idle_removes_expired_instances() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let manager = PiManager::new(tmp.path(), "fake-key");

        // Empty manager: kill_idle must not crash
        manager.kill_idle(Duration::from_secs(0)).await;
        assert!(!manager.is_running("chat_a").await);

        // Insert an instance whose last_active is "now"
        manager
            .insert_test_instance("chat_a", Instant::now())
            .await;
        assert!(manager.is_running("chat_a").await);

        // kill_idle with a generous timeout should keep it alive
        manager
            .kill_idle(Duration::from_secs(30 * 60))
            .await;
        assert!(
            manager.is_running("chat_a").await,
            "instance should survive when idle time < max_idle"
        );

        // kill_idle with ZERO timeout should remove it (elapsed > 0)
        manager.kill_idle(Duration::from_secs(0)).await;
        assert!(
            !manager.is_running("chat_a").await,
            "instance should be killed when max_idle is 0"
        );
    }

    #[tokio::test]
    async fn kill_idle_keeps_active_instance() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let manager = PiManager::new(tmp.path(), "fake-key");

        manager
            .insert_test_instance("chat_b", Instant::now())
            .await;

        // With a large timeout the instance stays
        manager
            .kill_idle(Duration::from_secs(60 * 60))
            .await;
        assert!(manager.is_running("chat_b").await);

        // Clean up: kill it so the sleep process doesn't linger
        manager.kill_idle(Duration::from_secs(0)).await;
    }

    #[tokio::test]
    async fn kill_idle_selective_removal() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let manager = PiManager::new(tmp.path(), "fake-key");

        // Insert two instances: one "old" (backdated by subtracting from now),
        // one fresh
        let old_time = Instant::now()
            .checked_sub(Duration::from_secs(31 * 60))
            .expect("system uptime > 31 min");
        manager.insert_test_instance("old_chat", old_time).await;
        manager
            .insert_test_instance("new_chat", Instant::now())
            .await;

        // Kill anything idle > 30 min
        manager
            .kill_idle(Duration::from_secs(30 * 60))
            .await;

        assert!(
            !manager.is_running("old_chat").await,
            "old instance (31 min idle) should be killed"
        );
        assert!(
            manager.is_running("new_chat").await,
            "fresh instance should survive"
        );

        // Clean up
        manager.kill_idle(Duration::from_secs(0)).await;
    }
}
