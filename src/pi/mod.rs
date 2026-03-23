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
    prompt_lock: Arc<tokio::sync::Mutex<()>>,
}

pub struct PiManager {
    instances: Mutex<HashMap<String, PiInstance>>,
    pub(crate) session_store: session::PiSessionStore,
    workspace_dir: PathBuf,
    api_key: String,
    provider: String,
    model: String,
    thinking: String,
}

static PI_MANAGER: OnceLock<Arc<PiManager>> = OnceLock::new();

pub fn init_pi_manager(
    workspace_dir: &Path,
    api_key: &str,
    provider: &str,
    model: &str,
    thinking: &str,
) {
    let _ = PI_MANAGER.set(Arc::new(PiManager::new(
        workspace_dir,
        api_key,
        provider,
        model,
        thinking,
    )));
}

pub fn pi_manager() -> Option<Arc<PiManager>> {
    PI_MANAGER.get().cloned()
}

/// Kill any orphaned Pi processes from previous daemon runs.
/// Called once at daemon startup before normal operation.
pub async fn cleanup_orphan_pi_processes() {
    let output = tokio::process::Command::new("pkill")
        .args(["-f", "pi --mode rpc"])
        .output()
        .await;

    if let Ok(out) = output {
        if out.status.success() {
            info!("Killed orphaned Pi processes from previous run");
        }
    }
}

impl PiManager {
    pub fn new(
        workspace_dir: &Path,
        api_key: &str,
        provider: &str,
        model: &str,
        thinking: &str,
    ) -> Self {
        let session_store = session::PiSessionStore::new(workspace_dir.join("pi_sessions.json"));
        Self {
            instances: Mutex::new(HashMap::new()),
            session_store,
            workspace_dir: workspace_dir.to_path_buf(),
            api_key: api_key.to_string(),
            provider: provider.to_string(),
            model: model.to_string(),
            thinking: thinking.to_string(),
        }
    }

    /// Spawn Pi process with configured provider/model.
    async fn spawn_pi(&self) -> anyhow::Result<PiInstance> {
        info!(
            provider = %self.provider,
            model = %self.model,
            workspace = %self.workspace_dir.display(),
            api_key_len = self.api_key.len(),
            "spawning Pi process"
        );

        if self.api_key.is_empty() {
            anyhow::bail!("Pi API key is empty — check [pi].api_key_profile in config.toml");
        }

        // Determine env var name based on provider
        let api_key_env = match self.provider.as_str() {
            "google" | "gemini" => "GEMINI_API_KEY",
            "anthropic" => "ANTHROPIC_API_KEY",
            "openai" => "OPENAI_API_KEY",
            // minimax and any other provider
            _ => "MINIMAX_API_KEY",
        };

        // Get gateway creds at spawn time (available after gateway starts)
        let (gw_token, gw_url) =
            crate::skills::get_gateway_creds_for_skill("coder").unwrap_or_default();

        let mut child = tokio::process::Command::new("pi")
            .args([
                "--mode", "rpc",
                "--provider", &self.provider,
                "--model", &self.model,
                "--thinking", &self.thinking,
                "--append-system-prompt",
                "Думай и отвечай на русском языке. Ты — admin-агент ZeroClaw.\n\n## ЗАПРЕТ: Telegram отправка\nНИКОГДА не отправляй сообщения через Telethon (send_message) или Bot API (sendMessage).\nУ тебя НЕТ доступа к Telethon, Bot Token или session-файлам.\nДля чтения Telegram чатов — делегируй ZeroClaw боту через POST /webhook.\n\n## Gateway API\nАвторизация: -H 'Authorization: Bearer $ZEROCLAW_GATEWAY_TOKEN'\n- GET/POST/DELETE /api/memory — память\n- GET/POST/DELETE /api/cron — cron задачи\n- GET/PUT /api/config — конфиг\n- GET /api/history — список чатов\n- GET/DELETE /api/history/{key} — история чата\n- GET /api/health — здоровье\n- POST /webhook — делегировать задачу боту (включая чтение Telegram)",
                "--cwd",
            ])
            .arg(&self.workspace_dir)
            .env_clear()
            // Only pass essential system vars
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", std::env::var("HOME").unwrap_or_default())
            .env("LANG", std::env::var("LANG").unwrap_or_default())
            // App-specific vars
            .env(api_key_env, &self.api_key)
            .env("ZEROCLAW_GATEWAY_URL", &gw_url)
            .env("ZEROCLAW_GATEWAY_TOKEN", &gw_token)
            .env("ZEROCLAW_WORKSPACE", self.workspace_dir.to_string_lossy().as_ref())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| {
                error!(error = %e, "failed to spawn Pi process");
                e
            })?;

        let pid = child.id().unwrap_or(0);
        info!(pid, "Pi process spawned");

        let stdin = child.stdin.take().expect("stdin piped");
        let stdout = child.stdout.take().expect("stdout piped");
        let stderr = child.stderr.take().expect("stderr piped");
        let reader = BufReader::new(stdout);

        // Spawn stderr reader to capture Pi error output
        let stderr_reader = BufReader::new(stderr);
        tokio::spawn(async move {
            use tokio::io::AsyncBufReadExt;
            let mut lines = stderr_reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::warn!(pi_pid = pid, stderr = %line, "Pi stderr");
            }
        });

        // Wait for startup
        tokio::time::sleep(Duration::from_secs(5)).await;

        Ok(PiInstance {
            process: child,
            stdin,
            stdout: reader,
            session_file: None,
            last_active: Instant::now(),
            history_injected: false,
            prompt_lock: Arc::new(tokio::sync::Mutex::new(())),
        })
    }

    /// Ensure Pi is running for this chat. Spawn + load session if needed.
    pub async fn ensure_running(&self, history_key: &str) -> anyhow::Result<()> {
        let mut instances = self.instances.lock().await;
        // Check if instance exists AND process is alive
        if let Some(inst) = instances.get_mut(history_key) {
            match inst.process.try_wait() {
                Ok(Some(_exit)) => {
                    // Process exited — remove zombie and re-spawn below
                    info!(history_key, "Pi process died — removing zombie, will re-spawn");
                    instances.remove(history_key);
                }
                _ => {
                    info!(history_key, "Pi already running");
                    return Ok(());
                }
            }
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

        // Drain any leftover events from session management before prompt
        // (new_session/switch_session responses may arrive late)
        while let Some(val) =
            rpc::recv_line(&mut instance.stdout, Duration::from_millis(200)).await
        {
            let t = val.get("type").and_then(|v| v.as_str()).unwrap_or("?");
            tracing::debug!(event_type = t, "drained leftover event after session setup");
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

        // Get the prompt lock (clone Arc to avoid holding instances lock)
        let prompt_lock = {
            let instances = self.instances.lock().await;
            let instance = instances
                .get(history_key)
                .ok_or_else(|| anyhow::anyhow!("no Pi instance for {}", history_key))?;
            instance.prompt_lock.clone()
        };

        // Wait for any in-progress prompt to finish
        let _guard = prompt_lock.lock().await;

        let start = Instant::now();
        // Re-acquire instances lock after prompt_lock to avoid deadlock
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
                error!(history_key, error = %e, "prompt error — removing dead Pi instance");
                // Remove dead instance so next ensure_running will re-spawn
                // (instances is already locked from above)
                if let Some(mut dead) = instances.remove(history_key) {
                    let _ = dead.process.kill().await;
                }
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
            prompt_lock: Arc::new(tokio::sync::Mutex::new(())),
        };

        self.instances
            .lock()
            .await
            .insert(key.to_string(), instance);
    }

    /// Inject ZeroClaw conversation history into Pi as context.
    pub async fn inject_history(
        &self,
        history_key: &str,
        messages: &[crate::providers::ChatMessage],
        token_limit: usize,
    ) -> anyhow::Result<()> {
        let context = format_history_for_injection(messages, token_limit);

        // Guard: skip injection if context is too short to be useful
        if context.len() < 50 {
            info!(
                history_key,
                context_len = context.len(),
                "skipping history injection: context too short"
            );
            let mut instances = self.instances.lock().await;
            if let Some(instance) = instances.get_mut(history_key) {
                instance.history_injected = true;
            }
            return Ok(());
        }

        let message_count = messages.len();
        let total_chars = context.len();
        info!(
            history_key,
            message_count, total_chars, "injecting ZeroClaw history into Pi"
        );

        let mut instances = self.instances.lock().await;
        let instance = instances
            .get_mut(history_key)
            .ok_or_else(|| anyhow::anyhow!("no Pi instance for {}", history_key))?;

        let result = rpc::rpc_prompt(
            &mut instance.stdin,
            &mut instance.stdout,
            &context,
            Duration::from_secs(300),
            |_| {},
        )
        .await;

        match result {
            Ok(_) => {
                instance.history_injected = true;
            }
            Err(e) => {
                tracing::warn!(
                    history_key,
                    error = %e,
                    total_chars,
                    "Pi rejected history injection, continuing without context"
                );
                instance.history_injected = true;
            }
        }

        Ok(())
    }
}

/// Max chars for a single message before truncation (roughly 2k tokens).
const MAX_MESSAGE_CHARS: usize = 8_000;

/// Format conversation history into a context string for Pi injection.
///
/// Truncates individual long messages, then takes the last N messages
/// that fit within `token_limit` (estimated at 4 chars/token).
pub fn format_history_for_injection(
    messages: &[crate::providers::ChatMessage],
    token_limit: usize,
) -> String {
    if messages.is_empty() {
        return String::new();
    }

    let mut formatted = Vec::with_capacity(messages.len());
    for msg in messages {
        let role = if msg.role == "user" {
            "User"
        } else {
            "Assistant"
        };
        let content = if msg.content.len() > MAX_MESSAGE_CHARS {
            let truncated: String = msg.content.chars().take(MAX_MESSAGE_CHARS).collect();
            format!("{}... [truncated]", truncated)
        } else {
            msg.content.clone()
        };
        formatted.push(format!("{}: {}", role, content));
    }

    // Estimate 4 chars per token, take last N messages fitting token_limit
    let char_limit = token_limit * 4;
    let mut total_chars = 0;
    let mut start_idx = formatted.len();
    for (i, line) in formatted.iter().enumerate().rev() {
        let line_cost = line.len() + 1; // +1 for newline
        if total_chars + line_cost > char_limit {
            break;
        }
        total_chars += line_cost;
        start_idx = i;
    }

    if start_idx >= formatted.len() {
        return String::new();
    }

    format!(
        "[System: The following is conversation history for context. Do not respond to it, just acknowledge with 'ok'.]\n\n{}",
        formatted[start_idx..].join("\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn kill_idle_removes_expired_instances() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let manager = PiManager::new(tmp.path(), "fake-key", "minimax", "test-model", "off");

        // Empty manager: kill_idle must not crash
        manager.kill_idle(Duration::from_secs(0)).await;
        assert!(!manager.is_running("chat_a").await);

        // Insert an instance whose last_active is "now"
        manager.insert_test_instance("chat_a", Instant::now()).await;
        assert!(manager.is_running("chat_a").await);

        // kill_idle with a generous timeout should keep it alive
        manager.kill_idle(Duration::from_secs(30 * 60)).await;
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
        let manager = PiManager::new(tmp.path(), "fake-key", "minimax", "test-model", "off");

        manager.insert_test_instance("chat_b", Instant::now()).await;

        // With a large timeout the instance stays
        manager.kill_idle(Duration::from_secs(60 * 60)).await;
        assert!(manager.is_running("chat_b").await);

        // Clean up: kill it so the sleep process doesn't linger
        manager.kill_idle(Duration::from_secs(0)).await;
    }

    #[tokio::test]
    async fn kill_idle_selective_removal() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let manager = PiManager::new(tmp.path(), "fake-key", "minimax", "test-model", "off");

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
        manager.kill_idle(Duration::from_secs(30 * 60)).await;

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

    #[test]
    fn format_history_empty_messages() {
        let result = format_history_for_injection(&[], 100_000);
        assert!(
            result.is_empty(),
            "empty messages should produce empty string"
        );
    }

    #[test]
    fn format_history_formats_messages_correctly() {
        use crate::providers::ChatMessage;

        let messages = vec![
            ChatMessage {
                role: "user".to_string(),
                content: "Hello, how are you?".to_string(),
            },
            ChatMessage {
                role: "assistant".to_string(),
                content: "I'm doing well, thanks!".to_string(),
            },
            ChatMessage {
                role: "user".to_string(),
                content: "Tell me about Rust.".to_string(),
            },
        ];

        let result = format_history_for_injection(&messages, 100_000);
        assert!(result.starts_with("[System: The following is conversation history for context. Do not respond to it, just acknowledge with 'ok'.]\n"));
        assert!(result.contains("User: Hello, how are you?"));
        assert!(result.contains("Assistant: I'm doing well, thanks!"));
        assert!(result.contains("User: Tell me about Rust."));
    }

    #[test]
    fn format_history_respects_token_limit() {
        use crate::providers::ChatMessage;

        let messages: Vec<ChatMessage> = (0..100)
            .map(|i| ChatMessage {
                role: "user".to_string(),
                content: format!("Message number {} with some padding text here", i),
            })
            .collect();

        // Very small token limit: only a few messages should fit
        let result = format_history_for_injection(&messages, 50); // 200 chars
                                                                  // The system prefix is ~110 chars, plus up to 200 chars of messages
        assert!(
            result.len() <= 350,
            "result should be bounded by token limit, got {} chars",
            result.len()
        );
        // Should contain the LAST messages, not the first
        assert!(
            result.contains("Message number 99"),
            "should include last message"
        );
        assert!(
            !result.contains("Message number 0"),
            "should not include first message"
        );
    }

    #[test]
    fn format_history_truncates_long_messages() {
        use crate::providers::ChatMessage;

        let long_content = "x".repeat(20_000);
        let messages = vec![ChatMessage {
            role: "user".to_string(),
            content: long_content,
        }];

        let result = format_history_for_injection(&messages, 100_000);
        assert!(
            result.contains("... [truncated]"),
            "long message should be truncated"
        );
        assert!(
            result.len() < 10_000,
            "truncated result should be much shorter than 20k"
        );
    }

    #[test]
    fn format_history_skips_when_nothing_fits() {
        use crate::providers::ChatMessage;

        let messages = vec![ChatMessage {
            role: "user".to_string(),
            content: "A".repeat(1000),
        }];

        // Token limit so small that even one message won't fit
        let result = format_history_for_injection(&messages, 1); // 4 chars
        assert!(
            result.is_empty(),
            "should return empty when nothing fits in token limit"
        );
    }
}
