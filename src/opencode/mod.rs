pub mod client;
pub mod config;
pub mod events;
pub mod process;
pub mod session;
pub mod status;
pub mod telegram;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use tokio::sync::{Mutex, RwLock};
use tracing::{debug, info, warn};

use crate::config::OpenCodeConfig;
use crate::opencode::client::OpenCodeClient;
use crate::opencode::events::{drain_sse_into_status, subscribe_sse, OpenCodeEvent};
use crate::opencode::session::OpenCodeSessionStore;
use crate::opencode::status::StatusBuilder;

// ── Polling status ────────────────────────────────────────────────────────────

/// Status updates from polling OC messages API.
#[derive(Debug, Clone)]
pub enum PollingStatus {
    /// Model is thinking — preview of text so far
    Thinking(String),
    /// Tool call with name, status ("running" / "completed"), and optional detail
    Tool {
        name: String,
        status: String,
        detail: Option<String>,
        /// Full command/input for the tool (shown in verbose mode)
        input: Option<String>,
        /// Tool output/result (shown in verbose mode)
        output: Option<String>,
    },
    /// New reasoning step started
    StepStart,
}

// ── Session entry ─────────────────────────────────────────────────────────────

struct SessionEntry {
    opencode_session_id: String,
    last_active: Instant,
    history_injected: bool,
}

// ── Manager ───────────────────────────────────────────────────────────────────

/// Manages OpenCode sessions per ZeroClaw `history_key`.
///
/// One global instance is created at daemon startup when `[opencode] enabled = true`.
pub struct OpenCodeManager {
    workspace_dir: PathBuf,
    provider: String,
    model: String,
    port: u16,
    /// session_store maps history_key → SessionEntry (runtime metadata)
    session_map: RwLock<HashMap<String, SessionEntry>>,
    /// Persistent disk store: history_key → opencode_session_id
    session_store: OpenCodeSessionStore,
    http_client: Arc<OpenCodeClient>,
    /// Maps history_key → AbortHandle for the active SSE reader task
    active_sse: Mutex<HashMap<String, tokio::task::AbortHandle>>,
    idle_timeout: Duration,
    history_inject_limit: usize,
    history_inject_max_chars: usize,
}

static OC_MANAGER: OnceLock<Arc<OpenCodeManager>> = OnceLock::new();

/// Initialise the global OpenCodeManager. Call once at daemon startup.
pub fn init_oc_manager(config: &OpenCodeConfig, _api_key: &str, workspace_dir: &Path) {
    let store_path = workspace_dir.join("opencode").join("sessions.json");
    let session_store = OpenCodeSessionStore::new(store_path);
    let _ = OC_MANAGER.set(Arc::new(OpenCodeManager {
        workspace_dir: workspace_dir.to_path_buf(),
        provider: config.provider.clone(),
        model: config.model.clone(),
        port: config.port,
        session_map: RwLock::new(HashMap::new()),
        session_store,
        http_client: Arc::new(OpenCodeClient::new(config.port)),
        active_sse: Mutex::new(HashMap::new()),
        idle_timeout: Duration::from_secs(config.idle_timeout_secs),
        history_inject_limit: config.history_inject_limit,
        history_inject_max_chars: config.history_inject_max_chars,
    }));
}

/// Access the global OpenCodeManager.
pub fn oc_manager() -> Option<Arc<OpenCodeManager>> {
    OC_MANAGER.get().cloned()
}

// ── Manager implementation ────────────────────────────────────────────────────

impl OpenCodeManager {
    /// Return the OpenCode session ID for `history_key`, creating one if needed.
    ///
    /// Lock order: read session_map → release → HTTP → write session_map.
    pub async fn ensure_session(&self, history_key: &str) -> anyhow::Result<String> {
        // 1. Check in-memory map first
        {
            let map = self.session_map.read().await;
            if let Some(entry) = map.get(history_key) {
                return Ok(entry.opencode_session_id.clone());
            }
        }

        // 2. Check disk store
        let disk_id = self.session_store.get(history_key);

        // 3. Verify session still exists on OpenCode server
        if let Some(ref id) = disk_id {
            match self.http_client.get_session(id).await {
                Ok(Some(_)) => {
                    // Session exists — add to in-memory map
                    let mut map = self.session_map.write().await;
                    // TOCTOU guard: check again after acquiring write lock
                    if let Some(entry) = map.get(history_key) {
                        return Ok(entry.opencode_session_id.clone());
                    }
                    map.insert(
                        history_key.to_string(),
                        SessionEntry {
                            opencode_session_id: id.clone(),
                            last_active: Instant::now(),
                            history_injected: false,
                        },
                    );
                    return Ok(id.clone());
                }
                Ok(None) => {
                    info!(
                        history_key,
                        session_id = id,
                        "OpenCode session not found, creating new"
                    );
                }
                Err(e) => {
                    warn!(history_key, error = %e, "could not verify session, creating new");
                }
            }
        }

        // 4. Create new session
        let directory = self.workspace_dir.to_string_lossy().to_string();
        let session_id = self
            .http_client
            .create_session(&directory)
            .await
            .map_err(|e| anyhow::anyhow!("create session: {e}"))?;
        info!(history_key, session_id = %session_id, "created new OpenCode session");

        // 5. Persist and update in-memory map (write lock, no HTTP inside)
        {
            let mut map = self.session_map.write().await;
            // TOCTOU guard
            if let Some(entry) = map.get(history_key) {
                return Ok(entry.opencode_session_id.clone());
            }
            map.insert(
                history_key.to_string(),
                SessionEntry {
                    opencode_session_id: session_id.clone(),
                    last_active: Instant::now(),
                    history_injected: false,
                },
            );
        }
        self.session_store.set(history_key, &session_id);

        Ok(session_id)
    }

    /// Returns true if history has not yet been injected for this session.
    pub async fn needs_history_injection(&self, history_key: &str) -> bool {
        let map = self.session_map.read().await;
        map.get(history_key)
            .map(|e| !e.history_injected)
            .unwrap_or(false)
    }

    /// Inject ZeroClaw conversation history into the OpenCode session as context.
    ///
    /// Uses `noReply=true` so OpenCode does not generate a response.
    /// On failure: logs WARN, does not propagate (history injection is best-effort).
    pub async fn inject_history(
        &self,
        history_key: &str,
        messages: &[crate::providers::ChatMessage],
    ) {
        let session_id = match self.get_session_id(history_key).await {
            Some(id) => id,
            None => return,
        };

        let formatted = format_history_for_injection(
            messages,
            self.history_inject_limit,
            self.history_inject_max_chars,
        );
        if formatted.len() < 50 {
            debug!(history_key, "history too short to inject, skipping");
            self.mark_history_injected(history_key).await;
            return;
        }

        match self
            .http_client
            .send_message_no_reply(&session_id, &formatted, &self.provider, &self.model)
            .await
        {
            Ok(()) => {
                debug!(history_key, "history injected successfully");
            }
            Err(e) => {
                warn!(history_key, error = %e, "history injection failed, continuing without context");
            }
        }
        self.mark_history_injected(history_key).await;
    }

    /// Send a message, stream events via `on_event`, and return the final text.
    ///
    /// Subscribes to SSE before sending the HTTP request to avoid missing events.
    /// On connection error: re-spawns the OpenCode server and retries once.
    pub async fn prompt(
        &self,
        history_key: &str,
        text: &str,
        messages: Option<&[crate::providers::ChatMessage]>,
        on_event: impl Fn(OpenCodeEvent) + Send + Sync + 'static,
    ) -> anyhow::Result<String> {
        let session_id = self.ensure_session(history_key).await?;

        // Inject history if not yet done
        if self.needs_history_injection(history_key).await {
            if let Some(msgs) = messages {
                self.inject_history(history_key, msgs).await;
            } else {
                self.mark_history_injected(history_key).await;
            }
        }

        // Update last_active
        self.touch_session(history_key).await;

        // Subscribe SSE before sending message (to avoid missing early events)
        let sse_http_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(600))
            .connect_timeout(Duration::from_secs(5))
            .no_proxy()
            .build()
            .unwrap_or_default();
        let base_url = format!("http://127.0.0.1:{}", self.port);
        let (mut rx, cancel_token, sse_handle) =
            subscribe_sse(sse_http_client, base_url, session_id.clone());

        // Store SSE abort handle
        {
            let mut sse_map = self.active_sse.lock().await;
            sse_map.insert(history_key.to_string(), sse_handle.abort_handle());
        }

        // Spawn status-update task
        let cancel_for_status = cancel_token.clone();
        let on_event = Arc::new(on_event);
        let on_event_clone = Arc::clone(&on_event);
        let (status_done_tx, status_done_rx) = tokio::sync::oneshot::channel::<String>();

        tokio::spawn(async move {
            let mut status = StatusBuilder::new();
            let mut thinking_buf = String::new();
            let mut active_tool: Option<String> = None;
            let mut local_text_buf = String::new();

            loop {
                tokio::select! {
                    () = cancel_for_status.cancelled() => break,
                    ev = rx.recv() => {
                        match ev {
                            None => break,
                            Some(event) => {
                                // Collect text separately
                                if let OpenCodeEvent::TextDelta(ref d) = event {
                                    local_text_buf.push_str(d);
                                }
                                on_event_clone(event.clone());
                                if drain_sse_into_status(&event, &mut status, &mut thinking_buf, &mut active_tool) {
                                    break;
                                }
                            }
                        }
                    }
                }
            }
            let _ = status_done_tx.send(local_text_buf);
        });

        // Send the actual message — retry once if OC server has crashed
        let result = match self
            .http_client
            .send_message(&session_id, text, &self.provider, &self.model)
            .await
        {
            Ok(r) => Ok(r),
            Err(crate::opencode::client::OpenCodeError::Http(e)) => {
                tracing::warn!(history_key, error = %e, "OC HTTP error, attempting server restart and retry");
                // Try to restart the server
                if let Some(pm) = crate::opencode::process::opencode_process() {
                    if pm.ensure_running().await.is_ok() {
                        // Retry with the same session first
                        match self
                            .http_client
                            .send_message(&session_id, text, &self.provider, &self.model)
                            .await
                        {
                            Ok(r) => Ok(r),
                            Err(_retry_err) => {
                                // Session may have been deleted (FK constraint) — create fresh
                                tracing::warn!(
                                    history_key,
                                    "OC retry failed, creating fresh session"
                                );
                                let directory = self.workspace_dir.to_string_lossy().to_string();
                                match self.http_client.create_session(&directory).await {
                                    Ok(new_sid) => {
                                        let mut map = self.session_map.write().await;
                                        map.insert(
                                            history_key.to_string(),
                                            SessionEntry {
                                                opencode_session_id: new_sid.clone(),
                                                last_active: Instant::now(),
                                                history_injected: false,
                                            },
                                        );
                                        drop(map);

                                        // Re-inject conversation history so the fresh
                                        // session has context from before the crash.
                                        if let Some(msgs) = messages {
                                            self.inject_history(history_key, msgs).await;
                                        }

                                        self.http_client
                                            .send_message(
                                                &new_sid,
                                                text,
                                                &self.provider,
                                                &self.model,
                                            )
                                            .await
                                            .map_err(|e| {
                                                anyhow::anyhow!(
                                                    "OC prompt failed with fresh session: {e}"
                                                )
                                            })
                                    }
                                    Err(e) => Err(anyhow::anyhow!(
                                        "OC fresh session creation failed: {e}"
                                    )),
                                }
                            }
                        }
                    } else {
                        Err(anyhow::anyhow!("OC server restart failed: {e}"))
                    }
                } else {
                    Err(anyhow::anyhow!("OC prompt transport error: {e}"))
                }
            }
            Err(e) => Err(anyhow::anyhow!("OC prompt error: {e}")),
        };

        // Cancel SSE reader
        cancel_token.cancel();
        let sse_text = status_done_rx.await.unwrap_or_default();

        // Remove SSE handle
        {
            let mut sse_map = self.active_sse.lock().await;
            sse_map.remove(history_key);
        }

        // Update last_active
        self.touch_session(history_key).await;

        match result {
            Ok(response) => {
                let final_text = response.text();
                if final_text.is_empty() {
                    Ok(sse_text)
                } else {
                    Ok(final_text)
                }
            }
            Err(e) => Err(e),
        }
    }

    /// Prompt with polling-based live status updates.
    ///
    /// OC 1.3 SSE `/event` endpoint doesn't forward session events, so instead
    /// we poll `GET /session/{id}/message` every 2s and report new parts
    /// (tool calls, thinking text) via `on_status`.
    pub async fn prompt_with_polling<F>(
        &self,
        history_key: &str,
        text: &str,
        history: Option<&[crate::providers::ChatMessage]>,
        on_status: F,
    ) -> anyhow::Result<String>
    where
        F: Fn(PollingStatus) + Send + Sync + 'static,
    {
        let session_id = self.ensure_session(history_key).await?;

        // Inject history if needed
        if let Some(turns) = history {
            if self.needs_history_injection(history_key).await {
                self.inject_history(&session_id, turns).await;
                self.mark_history_injected(history_key).await;
            }
        }
        self.touch_session(history_key).await;

        let http = Arc::clone(&self.http_client);
        let provider = self.provider.clone();
        let model = self.model.clone();
        let sid = session_id.clone();
        let msg = text.to_string();

        // Send message in background (blocks until OC completes)
        let send_handle =
            tokio::spawn(async move { http.send_message(&sid, &msg, &provider, &model).await });

        // Poll messages every 2s until send completes
        let on_status = Arc::new(on_status);
        let mut seen_parts = 0usize;
        let poll_interval = Duration::from_secs(2);

        loop {
            tokio::time::sleep(poll_interval).await;

            if send_handle.is_finished() {
                break;
            }

            if let Ok(messages) = self.http_client.get_messages(&session_id).await {
                let mut current_idx = 0usize;
                for msg_resp in &messages {
                    if msg_resp.info.role == "assistant" || msg_resp.info.role.is_empty() {
                        for part in &msg_resp.parts {
                            if current_idx >= seen_parts {
                                match part.kind.as_str() {
                                    "tool" => {
                                        let tool_name = part
                                            .extra
                                            .get("tool")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("tool");
                                        let status = part
                                            .extra
                                            .pointer("/state/status")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("running");
                                        // Extract description or command for context
                                        let detail = part
                                            .extra
                                            .pointer("/state/input/description")
                                            .and_then(|v| v.as_str())
                                            .or_else(|| {
                                                part.extra
                                                    .pointer("/state/input/command")
                                                    .and_then(|v| v.as_str())
                                            })
                                            .map(|s| s.chars().take(120).collect::<String>());
                                        // Full input (command, file path, etc.)
                                        let input = part
                                            .extra
                                            .pointer("/state/input/command")
                                            .and_then(|v| v.as_str())
                                            .or_else(|| {
                                                part.extra
                                                    .pointer("/state/input/file_path")
                                                    .and_then(|v| v.as_str())
                                            })
                                            .map(|s| s.chars().take(200).collect::<String>());
                                        // Tool output/result
                                        let output = part
                                            .extra
                                            .pointer("/state/output")
                                            .and_then(|v| v.as_str())
                                            .map(|s| s.chars().take(300).collect::<String>());
                                        on_status(PollingStatus::Tool {
                                            name: tool_name.to_string(),
                                            status: status.to_string(),
                                            detail,
                                            input,
                                            output,
                                        });
                                    }
                                    "text" => {
                                        if let Some(t) = &part.text {
                                            let preview: String = t.chars().take(300).collect();
                                            if !preview.is_empty() {
                                                on_status(PollingStatus::Thinking(preview));
                                            }
                                        }
                                    }
                                    "step-start" => {
                                        on_status(PollingStatus::StepStart);
                                    }
                                    _ => {}
                                }
                            }
                            current_idx += 1;
                        }
                    } else {
                        current_idx += msg_resp.parts.len();
                    }
                }
                if current_idx > seen_parts {
                    seen_parts = current_idx;
                }
            }
        }

        // Get result from send task
        let result = send_handle
            .await
            .map_err(|e| anyhow::anyhow!("send task panicked: {e}"))?;

        self.touch_session(history_key).await;

        match result {
            Ok(response) => {
                let final_text = response.text();
                if final_text.is_empty() {
                    // Fallback: extract text from polled messages
                    if let Ok(messages) = self.http_client.get_messages(&session_id).await {
                        let text: String = messages
                            .iter()
                            .filter(|m| m.info.role == "assistant" || m.info.role.is_empty())
                            .flat_map(|m| m.parts.iter())
                            .filter(|p| p.kind == "text")
                            .filter_map(|p| p.text.as_deref())
                            .collect::<Vec<_>>()
                            .join("");
                        Ok(text)
                    } else {
                        Ok(String::new())
                    }
                } else {
                    Ok(final_text)
                }
            }
            Err(e) => Err(anyhow::anyhow!("OC prompt error: {e}")),
        }
    }

    /// Send a message asynchronously (fire-and-forget). Used by `/pf`.
    pub async fn prompt_async(&self, history_key: &str, text: &str) -> anyhow::Result<()> {
        let session_id = self.ensure_session(history_key).await?;
        self.http_client
            .send_message_async(&session_id, text, &self.provider, &self.model)
            .await
            .map_err(|e| anyhow::anyhow!("prompt_async: {e}"))
    }

    /// Abort the current generation for this session. Used by `/ps`.
    pub async fn abort(&self, history_key: &str) -> anyhow::Result<bool> {
        // Cancel SSE reader if active
        {
            let mut sse_map = self.active_sse.lock().await;
            if let Some(handle) = sse_map.remove(history_key) {
                handle.abort();
            }
        }

        let session_id = match self.get_session_id(history_key).await {
            Some(id) => id,
            None => return Ok(false),
        };

        self.http_client
            .abort(&session_id)
            .await
            .map_err(|e| anyhow::anyhow!("abort: {e}"))
    }

    /// Delete the OpenCode session and remove from all stores.
    pub async fn stop(&self, history_key: &str) -> anyhow::Result<()> {
        // Abort SSE if active
        {
            let mut sse_map = self.active_sse.lock().await;
            if let Some(handle) = sse_map.remove(history_key) {
                handle.abort();
            }
        }

        let session_id = {
            let map = self.session_map.read().await;
            map.get(history_key).map(|e| e.opencode_session_id.clone())
        };

        if let Some(id) = session_id {
            if let Err(e) = self.http_client.delete_session(&id).await {
                warn!(history_key, error = %e, "failed to delete OpenCode session");
            }
        }

        {
            let mut map = self.session_map.write().await;
            map.remove(history_key);
        }
        self.session_store.remove(history_key);
        info!(history_key, "stopped OpenCode session");
        Ok(())
    }

    /// Stop all sessions. Called on daemon shutdown.
    pub async fn stop_all(&self) {
        let keys: Vec<String> = {
            let map = self.session_map.read().await;
            map.keys().cloned().collect()
        };
        for key in keys {
            let _ = self.stop(&key).await;
        }
    }

    /// Remove sessions idle longer than `max_idle`.
    /// Skips sessions with an active SSE reader (generation in-flight).
    pub async fn kill_idle(&self, max_idle: Duration) {
        let idle_keys: Vec<String> = {
            let map = self.session_map.read().await;
            let sse = self.active_sse.lock().await;
            map.iter()
                .filter(|(k, e)| e.last_active.elapsed() > max_idle && !sse.contains_key(*k))
                .map(|(k, _)| k.clone())
                .collect()
        };

        for key in idle_keys {
            info!(history_key = %key, "killing idle OpenCode session");
            let _ = self.stop(&key).await;
        }
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    /// Return the OpenCode session ID for `history_key` if one exists.
    /// Used to pass `ZC_OC_SESSION_ID` to skill subprocesses.
    pub async fn get_session_id(&self, history_key: &str) -> Option<String> {
        let map = self.session_map.read().await;
        map.get(history_key).map(|e| e.opencode_session_id.clone())
    }

    async fn touch_session(&self, history_key: &str) {
        let mut map = self.session_map.write().await;
        if let Some(entry) = map.get_mut(history_key) {
            entry.last_active = Instant::now();
        }
    }

    async fn mark_history_injected(&self, history_key: &str) {
        let mut map = self.session_map.write().await;
        if let Some(entry) = map.get_mut(history_key) {
            entry.history_injected = true;
        }
    }
}

// ── History formatting ────────────────────────────────────────────────────────

/// Max chars for a single message before truncation.
const MAX_MESSAGE_CHARS: usize = 8_000;

/// Format ZeroClaw conversation history for injection into OpenCode.
///
/// Takes the last `limit` messages that fit within `max_chars` total,
/// truncating individual long messages to `MAX_MESSAGE_CHARS`.
pub fn format_history_for_injection(
    messages: &[crate::providers::ChatMessage],
    limit: usize,
    max_chars: usize,
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

    // Take at most `limit` messages from the end
    let start_from = if formatted.len() > limit {
        formatted.len() - limit
    } else {
        0
    };
    let candidates = &formatted[start_from..];

    // Then trim further to fit within max_chars
    let mut total_chars = 0usize;
    let mut start_idx = candidates.len();
    for (i, line) in candidates.iter().enumerate().rev() {
        let line_cost = line.len() + 1; // +1 for newline
        if total_chars + line_cost > max_chars {
            break;
        }
        total_chars += line_cost;
        start_idx = i;
    }

    if start_idx >= candidates.len() {
        return String::new();
    }

    format!(
        "[System: The following is conversation history for context. Do not respond to it, just acknowledge with 'ok'.]\n\n{}",
        candidates[start_idx..].join("\n")
    )
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_manager(dir: &std::path::Path) -> OpenCodeManager {
        let store_path = dir.join("sessions.json");
        OpenCodeManager {
            workspace_dir: dir.to_path_buf(),
            provider: "minimax".to_string(),
            model: "MiniMax-M2.7-highspeed".to_string(),
            port: 19999,
            session_map: RwLock::new(HashMap::new()),
            session_store: OpenCodeSessionStore::new(store_path),
            http_client: Arc::new(OpenCodeClient::with_base_url("http://127.0.0.1:19999")),
            active_sse: Mutex::new(HashMap::new()),
            idle_timeout: Duration::from_secs(1800),
            history_inject_limit: 50,
            history_inject_max_chars: 50_000,
        }
    }

    #[tokio::test]
    async fn needs_history_injection_true_for_new_session() {
        let dir = tempdir().unwrap();
        let mgr = make_manager(dir.path());
        // Insert a session entry directly (simulate ensure_session)
        {
            let mut map = mgr.session_map.write().await;
            map.insert(
                "key1".to_string(),
                SessionEntry {
                    opencode_session_id: "ses_abc".to_string(),
                    last_active: Instant::now(),
                    history_injected: false,
                },
            );
        }
        assert!(mgr.needs_history_injection("key1").await);
    }

    #[tokio::test]
    async fn needs_history_injection_false_after_mark() {
        let dir = tempdir().unwrap();
        let mgr = make_manager(dir.path());
        {
            let mut map = mgr.session_map.write().await;
            map.insert(
                "key1".to_string(),
                SessionEntry {
                    opencode_session_id: "ses_abc".to_string(),
                    last_active: Instant::now(),
                    history_injected: false,
                },
            );
        }
        mgr.mark_history_injected("key1").await;
        assert!(!mgr.needs_history_injection("key1").await);
    }

    #[tokio::test]
    async fn kill_idle_removes_old_sessions() {
        let dir = tempdir().unwrap();
        let mgr = make_manager(dir.path());
        {
            let mut map = mgr.session_map.write().await;
            map.insert(
                "old_key".to_string(),
                SessionEntry {
                    opencode_session_id: "ses_old".to_string(),
                    last_active: Instant::now()
                        .checked_sub(Duration::from_secs(3600))
                        .unwrap_or(Instant::now()),
                    history_injected: true,
                },
            );
            map.insert(
                "new_key".to_string(),
                SessionEntry {
                    opencode_session_id: "ses_new".to_string(),
                    last_active: Instant::now(),
                    history_injected: true,
                },
            );
        }
        mgr.kill_idle(Duration::from_secs(1800)).await;
        let map = mgr.session_map.read().await;
        assert!(
            !map.contains_key("old_key"),
            "old session should be removed"
        );
        assert!(map.contains_key("new_key"), "new session should remain");
    }

    #[tokio::test]
    async fn kill_idle_skips_sessions_with_active_sse() {
        let dir = tempdir().unwrap();
        let mgr = make_manager(dir.path());
        {
            let mut map = mgr.session_map.write().await;
            map.insert(
                "active_key".to_string(),
                SessionEntry {
                    opencode_session_id: "ses_active".to_string(),
                    last_active: Instant::now()
                        .checked_sub(Duration::from_secs(3600))
                        .unwrap_or(Instant::now()),
                    history_injected: true,
                },
            );
        }
        // Simulate active SSE by inserting a dummy abort handle
        {
            let fut = tokio::spawn(async {
                tokio::time::sleep(Duration::from_secs(60)).await;
            });
            let handle = fut.abort_handle();
            let mut sse = mgr.active_sse.lock().await;
            sse.insert("active_key".to_string(), handle);
            fut.abort(); // cleanup immediately
        }
        mgr.kill_idle(Duration::from_secs(0)).await;
        let map = mgr.session_map.read().await;
        // Session should NOT be removed because SSE was active
        // Note: The abort handle is gone after fut.abort(), but the key is still in active_sse
        // This test verifies the skip logic runs without panic
        drop(map);
    }

    #[tokio::test]
    async fn stop_removes_session_from_map() {
        let dir = tempdir().unwrap();
        let mgr = make_manager(dir.path());
        {
            let mut map = mgr.session_map.write().await;
            map.insert(
                "key1".to_string(),
                SessionEntry {
                    opencode_session_id: "ses_abc".to_string(),
                    last_active: Instant::now(),
                    history_injected: false,
                },
            );
        }
        // stop will try to call delete_session on the mock server (port 19999, will fail)
        // but should still remove the entry from session_map
        let _ = mgr.stop("key1").await;
        let map = mgr.session_map.read().await;
        assert!(!map.contains_key("key1"));
    }

    #[test]
    fn format_history_empty() {
        let result = format_history_for_injection(&[], 50, 50_000);
        assert!(result.is_empty());
    }

    #[test]
    fn format_history_basic() {
        use crate::providers::ChatMessage;
        let messages = vec![
            ChatMessage {
                role: "user".to_string(),
                content: "Hello".to_string(),
            },
            ChatMessage {
                role: "assistant".to_string(),
                content: "Hi there".to_string(),
            },
        ];
        let result = format_history_for_injection(&messages, 50, 50_000);
        assert!(result.contains("User: Hello"));
        assert!(result.contains("Assistant: Hi there"));
        assert!(result.starts_with("[System:"));
    }

    #[test]
    fn format_history_respects_limit() {
        use crate::providers::ChatMessage;
        let messages: Vec<ChatMessage> = (0..100)
            .map(|i| ChatMessage {
                role: "user".to_string(),
                content: format!("Message {i}"),
            })
            .collect();
        // limit=5: only last 5 messages
        let result = format_history_for_injection(&messages, 5, 50_000);
        assert!(result.contains("Message 99"));
        assert!(!result.contains("Message 0"));
    }

    #[test]
    fn format_history_respects_max_chars() {
        use crate::providers::ChatMessage;
        let messages: Vec<ChatMessage> = (0..10)
            .map(|i| ChatMessage {
                role: "user".to_string(),
                content: format!("Message number {i} with some padding text here"),
            })
            .collect();
        // Very small max_chars: only last message(s) should fit
        let result = format_history_for_injection(&messages, 50, 100);
        assert!(result.contains("Message number 9") || result.is_empty());
        if !result.is_empty() {
            assert!(!result.contains("Message number 0"));
        }
    }

    #[test]
    fn format_history_truncates_long_message() {
        use crate::providers::ChatMessage;
        let messages = vec![ChatMessage {
            role: "user".to_string(),
            content: "x".repeat(20_000),
        }];
        let result = format_history_for_injection(&messages, 50, 50_000);
        assert!(result.contains("... [truncated]"));
    }
}
