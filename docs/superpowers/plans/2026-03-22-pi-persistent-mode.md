# Pi Persistent Mode Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace per-message Pi spawn/kill with a persistent Pi process managed from Rust. Pi stays alive while Pi mode is active, activated via `/models pi`, with full session persistence and thinking/tool status in Telegram.

**Architecture:** New `src/pi/` module with 3 files: `rpc.rs` (JSONL protocol), `session.rs` (session persistence), `mod.rs` (PiManager lifecycle). Integration via `channels/mod.rs` replaces `spawn_coder_subprocess` with `pi_manager.prompt()`. Background idle reaper kills Pi after 30 min silence.

**Tech Stack:** Rust, tokio (async process, timers), serde_json, reqwest (Telegram Bot API)

**Spec:** `docs/superpowers/specs/2026-03-22-pi-persistent-mode-design.md`

---

## File Structure

| File | Action | Responsibility |
|------|--------|---------------|
| `src/pi/rpc.rs` | CREATE | JSONL send/recv over stdin/stdout, PiEvent enum, rpc_prompt streaming |
| `src/pi/session.rs` | CREATE | Pi session index persistence (pi_sessions.json) |
| `src/pi/status.rs` | CREATE | StatusBuilder — render thinking/tools/response for Telegram |
| `src/pi/telegram.rs` | CREATE | Telegram Bot API send/edit for Pi status messages |
| `src/pi/mod.rs` | CREATE | PiManager: spawn, prompt, stop, idle reaper, history injection |
| `src/lib.rs` | MODIFY | Add `pub(crate) mod pi;` |
| `src/channels/mod.rs` | MODIFY | Replace spawn_coder_subprocess with PiManager, add `/models pi` |
| `src/daemon/mod.rs` | MODIFY | Start Pi idle reaper background task |

---

### Task 1: Pi RPC Client (`src/pi/rpc.rs`)

Port of Python rpc_client.py to async Rust. Lowest layer — no dependencies on other Pi modules.

**Files:**
- Create: `src/pi/rpc.rs`
- Create: `src/pi/mod.rs` (minimal — just `pub mod rpc;`)
- Modify: `src/lib.rs` — add `pub(crate) mod pi;`

- [ ] **Step 1: Write PiEvent enum + send/recv tests**

```rust
// src/pi/rpc.rs
#[derive(Debug, Clone, PartialEq)]
pub enum PiEvent {
    ThinkingDelta(String),
    ThinkingEnd(String),  // full accumulated text
    ToolStart { name: String, args: serde_json::Value },
    ToolEnd { name: String, output: String },
    TextDelta(String),
    AgentEnd { text: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_agent_end_extracts_text() {
        let event = serde_json::json!({
            "type": "agent_end",
            "messages": [{
                "role": "assistant",
                "content": [{"type": "text", "text": "hello world"}]
            }]
        });
        let result = parse_pi_event(&event);
        assert_eq!(result, Some(PiEvent::AgentEnd { text: "hello world".into() }));
    }

    #[test]
    fn parse_tool_execution_start() {
        let event = serde_json::json!({
            "type": "tool_execution_start",
            "toolName": "bash",
            "args": {"command": "pwd"}
        });
        let result = parse_pi_event(&event);
        assert!(matches!(result, Some(PiEvent::ToolStart { name, .. }) if name == "bash"));
    }

    #[test]
    fn parse_thinking_delta() {
        let event = serde_json::json!({
            "type": "message_update",
            "assistantMessageEvent": {
                "type": "thinking_delta",
                "delta": "Let me think..."
            }
        });
        let result = parse_pi_event(&event);
        assert_eq!(result, Some(PiEvent::ThinkingDelta("Let me think...".into())));
    }
}
```

- [ ] **Step 2: Run tests → FAIL (parse_pi_event not defined)**

Run: `cargo test --lib -- pi::rpc::tests`

- [ ] **Step 3: Implement parse_pi_event**

```rust
pub fn parse_pi_event(event: &serde_json::Value) -> Option<PiEvent> {
    match event.get("type")?.as_str()? {
        "message_update" => {
            let ae = event.get("assistantMessageEvent")?;
            match ae.get("type")?.as_str()? {
                "thinking_delta" => Some(PiEvent::ThinkingDelta(
                    ae.get("delta")?.as_str()?.to_string()
                )),
                "thinking_end" => {
                    let content = ae.get("content")?.as_str().unwrap_or("").to_string();
                    Some(PiEvent::ThinkingEnd(content))
                }
                "text_delta" => Some(PiEvent::TextDelta(
                    ae.get("delta")?.as_str()?.to_string()
                )),
                _ => None,
            }
        }
        "tool_execution_start" => Some(PiEvent::ToolStart {
            name: event.get("toolName")?.as_str()?.to_string(),
            args: event.get("args").cloned().unwrap_or(serde_json::json!({})),
        }),
        "tool_execution_end" => Some(PiEvent::ToolEnd {
            name: event.get("toolName")?.as_str()?.to_string(),
            output: event.get("output").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        }),
        "agent_end" => {
            for msg in event.get("messages")?.as_array()?.iter().rev() {
                if msg.get("role")?.as_str()? == "assistant" {
                    let parts: Vec<&str> = msg.get("content")?.as_array()?
                        .iter()
                        .filter(|c| c.get("type").and_then(|t| t.as_str()) == Some("text"))
                        .filter_map(|c| c.get("text").and_then(|t| t.as_str()))
                        .collect();
                    let text = parts.join("").trim().to_string();
                    if !text.is_empty() {
                        return Some(PiEvent::AgentEnd { text });
                    }
                }
            }
            None
        }
        _ => None,
    }
}
```

- [ ] **Step 4: Run tests → PASS**

- [ ] **Step 5: Add async send/recv functions**

```rust
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout};
use std::time::Duration;

pub async fn send(stdin: &mut ChildStdin, msg: &serde_json::Value) -> anyhow::Result<()> {
    let line = serde_json::to_string(msg)? + "\n";
    stdin.write_all(line.as_bytes()).await?;
    stdin.flush().await?;
    Ok(())
}

pub async fn recv_line(
    reader: &mut BufReader<ChildStdout>,
    timeout: Duration,
) -> Option<serde_json::Value> {
    let mut line = String::new();
    match tokio::time::timeout(timeout, reader.read_line(&mut line)).await {
        Ok(Ok(0)) | Ok(Err(_)) | Err(_) => None,
        Ok(Ok(_)) => serde_json::from_str(line.trim()).ok(),
    }
}

pub async fn recv_response(
    reader: &mut BufReader<ChildStdout>,
    command: &str,
    timeout: Duration,
) -> Option<serde_json::Value> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() { return None; }
        let event = recv_line(reader, remaining).await?;
        if event.get("type").and_then(|t| t.as_str()) == Some("response")
            && event.get("command").and_then(|c| c.as_str()) == Some(command)
        {
            return Some(event);
        }
    }
}
```

- [ ] **Step 6: Add rpc_prompt — main streaming prompt function**

```rust
pub async fn rpc_prompt(
    stdin: &mut ChildStdin,
    reader: &mut BufReader<ChildStdout>,
    message: &str,
    mut on_event: impl FnMut(PiEvent),
    timeout: Duration,
) -> anyhow::Result<String> {
    send(stdin, &serde_json::json!({"type": "prompt", "message": message})).await?;

    let ack = recv_response(reader, "prompt", Duration::from_secs(10)).await;
    if !ack.as_ref().is_some_and(|a| a.get("success").and_then(|s| s.as_bool()) == Some(true)) {
        anyhow::bail!("Pi prompt ACK failed");
    }

    let deadline = tokio::time::Instant::now() + timeout;
    let mut thinking = String::new();

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() { anyhow::bail!("Pi prompt timed out"); }

        let Some(event) = recv_line(reader, Duration::from_secs(5).min(remaining)).await else {
            continue;
        };

        if let Some(pi_event) = parse_pi_event(&event) {
            match &pi_event {
                PiEvent::ThinkingDelta(delta) => thinking.push_str(delta),
                PiEvent::ThinkingEnd(_) => {
                    on_event(PiEvent::ThinkingEnd(thinking.clone()));
                    thinking.clear();
                }
                PiEvent::AgentEnd { text } => {
                    on_event(pi_event);
                    return Ok(text.clone());
                }
                _ => {}
            }
            on_event(pi_event);
        }

        // Handle extension UI auto-cancel
        if event.get("type").and_then(|t| t.as_str()) == Some("extension_ui_request") {
            let id = event.get("id").and_then(|i| i.as_str()).unwrap_or("");
            let _ = send(stdin, &serde_json::json!({
                "type": "extension_ui_response", "id": id, "cancelled": true
            })).await;
        }
    }
}
```

- [ ] **Step 7: Add RPC session management functions**

```rust
pub async fn rpc_new_session(stdin: &mut ChildStdin, reader: &mut BufReader<ChildStdout>) -> bool {
    send(stdin, &serde_json::json!({"type": "new_session"})).await.ok();
    recv_response(reader, "new_session", Duration::from_secs(10))
        .await
        .is_some_and(|r| r.get("success").and_then(|s| s.as_bool()) == Some(true))
}

pub async fn rpc_switch_session(
    stdin: &mut ChildStdin,
    reader: &mut BufReader<ChildStdout>,
    session_file: &str,
) -> bool {
    send(stdin, &serde_json::json!({"type": "switch_session", "sessionPath": session_file})).await.ok();
    recv_response(reader, "switch_session", Duration::from_secs(10))
        .await
        .is_some_and(|r| r.get("success").and_then(|s| s.as_bool()) == Some(true))
}

pub async fn rpc_get_session_file(
    stdin: &mut ChildStdin,
    reader: &mut BufReader<ChildStdout>,
) -> Option<String> {
    send(stdin, &serde_json::json!({"type": "get_state"})).await.ok()?;
    let resp = recv_response(reader, "get_state", Duration::from_secs(10)).await?;
    resp.get("data")?.get("sessionFile")?.as_str().map(|s| s.to_string())
}
```

- [ ] **Step 8: Run all rpc tests → PASS**

Run: `cargo test --lib -- pi::rpc`

- [ ] **Step 9: Commit**

```bash
git add src/pi/rpc.rs src/pi/mod.rs src/lib.rs
git commit -m "feat(pi): add RPC client for Pi JSONL protocol"
```

---

### Task 2: StatusBuilder (`src/pi/status.rs`)

Renders Pi events into Telegram-friendly status text. Pure, testable, no I/O.

**Files:**
- Create: `src/pi/status.rs`
- Modify: `src/pi/mod.rs` — add `pub mod status;`

- [ ] **Step 1: Write StatusBuilder tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_thinking() {
        let mut sb = StatusBuilder::new();
        sb.on_thinking_end("Let me read the file...");
        assert!(sb.render().contains("💭"));
        assert!(sb.render().contains("Let me read"));
    }

    #[test]
    fn renders_tool_start() {
        let mut sb = StatusBuilder::new();
        sb.on_tool_start("bash", &serde_json::json!({"command": "pwd"}));
        assert!(sb.render().contains("🔧"));
        assert!(sb.render().contains("pwd"));
    }

    #[test]
    fn renders_tool_output() {
        let mut sb = StatusBuilder::new();
        sb.on_tool_end("read", "/home/user");
        assert!(sb.render().contains("📄"));
    }

    #[test]
    fn truncates_to_limit() {
        let mut sb = StatusBuilder::new();
        for i in 0..100 {
            sb.on_thinking_end(&format!("Thinking block {} with some long text to fill space", i));
        }
        assert!(sb.render().len() <= 3800);
    }

    #[test]
    fn empty_renders_fallback() {
        let sb = StatusBuilder::new();
        assert_eq!(sb.render(), "⚙ Pi is working…");
    }

    #[test]
    fn renders_response_text() {
        let mut sb = StatusBuilder::new();
        sb.on_thinking_end("thinking...");
        sb.on_response_text("The answer is 42");
        let r = sb.render();
        assert!(r.contains("42"));
        assert!(r.contains("💭"));
    }
}
```

- [ ] **Step 2: Run tests → FAIL**

- [ ] **Step 3: Implement StatusBuilder**

```rust
const TG_MAX_CHARS: usize = 3800;
const THINKING_PREVIEW: usize = 200;
const TOOL_OUTPUT_PREVIEW: usize = 150;

pub struct StatusBuilder {
    sections: Vec<(String, String)>, // (icon, text)
    response: String,
}

impl StatusBuilder {
    pub fn new() -> Self { Self { sections: Vec::new(), response: String::new() } }

    pub fn on_thinking_end(&mut self, text: &str) {
        let preview = if text.len() > THINKING_PREVIEW {
            format!("{}…", &text[..THINKING_PREVIEW])
        } else { text.to_string() };
        self.sections.push(("💭".into(), preview));
    }

    pub fn on_tool_start(&mut self, name: &str, args: &serde_json::Value) {
        let detail = match name {
            "bash" => format!("`{}`", args.get("command").and_then(|c| c.as_str()).unwrap_or("").chars().take(80).collect::<String>()),
            "read" => args.get("path").and_then(|p| p.as_str()).unwrap_or("?").to_string(),
            "write" | "edit" => format!("write {}", args.get("path").and_then(|p| p.as_str()).unwrap_or("?")),
            _ => name.to_string(),
        };
        let icon = match name { "read" => "📖", "write" | "edit" => "✏️", _ => "🔧" };
        self.sections.push((icon.into(), detail));
    }

    pub fn on_tool_end(&mut self, _name: &str, output: &str) {
        if !output.trim().is_empty() {
            let preview = if output.len() > TOOL_OUTPUT_PREVIEW {
                format!("{}…", &output[..TOOL_OUTPUT_PREVIEW])
            } else { output.to_string() };
            self.sections.push(("📄".into(), format!("`{}`", preview)));
        }
    }

    pub fn on_response_text(&mut self, text: &str) {
        self.response = text.to_string();
    }

    pub fn render(&self) -> String {
        if self.sections.is_empty() && self.response.is_empty() {
            return "⚙ Pi is working…".to_string();
        }
        let mut lines: Vec<String> = self.sections.iter()
            .map(|(icon, text)| format!("{icon} {text}"))
            .collect();
        if !self.response.is_empty() {
            if !lines.is_empty() { lines.push(String::new()); }
            lines.push(self.response.clone());
        }
        let mut result = lines.join("\n");
        if result.len() > TG_MAX_CHARS {
            result = format!("…\n{}", &result[result.len() - TG_MAX_CHARS + 5..]);
        }
        result
    }
}
```

- [ ] **Step 4: Run tests → PASS**

- [ ] **Step 5: Commit**

```bash
git add src/pi/status.rs src/pi/mod.rs
git commit -m "feat(pi): add StatusBuilder for Telegram live status"
```

---

### Task 3: Telegram Bot API helper (`src/pi/telegram.rs`)

Send and edit messages via Telegram Bot API from Rust (bypassing channel abstraction).

**Files:**
- Create: `src/pi/telegram.rs`
- Modify: `src/pi/mod.rs` — add `pub mod telegram;`

- [ ] **Step 1: Implement send_status + edit_status**

```rust
use reqwest::Client;

pub struct TelegramNotifier {
    client: Client,
    bot_token: String,
    chat_id: String,
}

impl TelegramNotifier {
    pub fn new(bot_token: &str, chat_id: &str) -> Self {
        Self {
            client: Client::new(),
            bot_token: bot_token.to_string(),
            chat_id: chat_id.to_string(),
        }
    }

    fn api_url(&self, method: &str) -> String {
        format!("https://api.telegram.org/bot{}/{}", self.bot_token, method)
    }

    /// Send initial status message, return message_id for future edits.
    pub async fn send_status(&self, text: &str) -> Option<i64> {
        let body = serde_json::json!({
            "chat_id": self.chat_id,
            "text": text,
            "parse_mode": "Markdown",
        });
        let resp = self.client.post(self.api_url("sendMessage"))
            .json(&body).send().await.ok()?;
        let data: serde_json::Value = resp.json().await.ok()?;
        data.get("result")?.get("message_id")?.as_i64()
    }

    /// Edit existing status message. Silently ignores errors.
    pub async fn edit_status(&self, message_id: i64, text: &str) {
        let body = serde_json::json!({
            "chat_id": self.chat_id,
            "message_id": message_id,
            "text": text,
            "parse_mode": "Markdown",
        });
        let _ = self.client.post(self.api_url("editMessageText"))
            .json(&body).send().await;
    }
}
```

- [ ] **Step 2: Commit**

```bash
git add src/pi/telegram.rs src/pi/mod.rs
git commit -m "feat(pi): add Telegram Bot API notifier for Pi status"
```

---

### Task 4: Session persistence (`src/pi/session.rs`)

Manages pi_sessions.json — maps history_key to Pi session file paths.

**Files:**
- Create: `src/pi/session.rs`
- Modify: `src/pi/mod.rs` — add `pub mod session;`

- [ ] **Step 1: Write session store tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

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
        store.save("chat_1", "/path/to/session.jsonl");
        store.delete("chat_1");
        assert_eq!(store.load("chat_1"), None);
    }
}
```

- [ ] **Step 2: Implement PiSessionStore**

```rust
use std::path::PathBuf;
use std::collections::HashMap;

pub struct PiSessionStore {
    path: PathBuf,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SessionEntry {
    session_file: String,
    created_at: String,
    last_active: String,
}

impl PiSessionStore {
    pub fn new(path: PathBuf) -> Self { Self { path } }

    pub fn load(&self, history_key: &str) -> Option<String> {
        let data = std::fs::read_to_string(&self.path).ok()?;
        let map: HashMap<String, SessionEntry> = serde_json::from_str(&data).ok()?;
        map.get(history_key).map(|e| e.session_file.clone())
    }

    pub fn save(&self, history_key: &str, session_file: &str) {
        let mut map: HashMap<String, SessionEntry> = self.path
            .exists()
            .then(|| std::fs::read_to_string(&self.path).ok())
            .flatten()
            .and_then(|d| serde_json::from_str(&d).ok())
            .unwrap_or_default();

        let now = chrono::Utc::now().to_rfc3339();
        map.entry(history_key.to_string())
            .and_modify(|e| { e.session_file = session_file.to_string(); e.last_active = now.clone(); })
            .or_insert(SessionEntry {
                session_file: session_file.to_string(),
                created_at: now.clone(),
                last_active: now,
            });

        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&self.path, serde_json::to_string_pretty(&map).unwrap_or_default());
    }

    pub fn delete(&self, history_key: &str) {
        let Ok(data) = std::fs::read_to_string(&self.path) else { return };
        let Ok(mut map): Result<HashMap<String, SessionEntry>, _> = serde_json::from_str(&data) else { return };
        map.remove(history_key);
        let _ = std::fs::write(&self.path, serde_json::to_string_pretty(&map).unwrap_or_default());
    }
}
```

- [ ] **Step 3: Run tests → PASS, commit**

```bash
git add src/pi/session.rs src/pi/mod.rs
git commit -m "feat(pi): add PiSessionStore for session persistence"
```

---

### Task 5: PiManager — core lifecycle (`src/pi/mod.rs`)

Manages Pi process lifecycle: spawn, prompt, stop, idle reaper. This is the main orchestrator.

**Files:**
- Modify: `src/pi/mod.rs` — PiManager struct + methods

- [ ] **Step 1: Define PiManager and PiInstance structs**

```rust
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::BufReader;
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex;

pub mod rpc;
pub mod session;
pub mod status;
pub mod telegram;

struct PiInstance {
    _process: Child,
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
```

- [ ] **Step 2: Implement spawn_pi (internal)**

```rust
impl PiManager {
    pub fn new(workspace_dir: &Path, minimax_key: &str) -> Self {
        Self {
            instances: Mutex::new(HashMap::new()),
            session_store: session::PiSessionStore::new(workspace_dir.join("pi_sessions.json")),
            workspace_dir: workspace_dir.to_path_buf(),
            minimax_key: minimax_key.to_string(),
        }
    }

    async fn spawn_pi(&self) -> anyhow::Result<PiInstance> {
        let mut cmd = tokio::process::Command::new("pi");
        cmd.args(["--mode", "rpc",
                   "--provider", "minimax", "--model", "MiniMax-M2.7-highspeed",
                   "--thinking", "high",
                   "--cwd", &self.workspace_dir.to_string_lossy()]);
        cmd.env("MINIMAX_API_KEY", &self.minimax_key);
        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::null());

        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take().ok_or_else(|| anyhow::anyhow!("no stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| anyhow::anyhow!("no stdout"))?;

        // Wait for Pi to initialize
        tokio::time::sleep(Duration::from_secs(4)).await;

        Ok(PiInstance {
            _process: child,
            stdin,
            stdout: BufReader::new(stdout),
            session_file: None,
            last_active: Instant::now(),
            history_injected: false,
        })
    }
}
```

- [ ] **Step 3: Implement ensure_running**

```rust
    pub async fn ensure_running(&self, history_key: &str) -> anyhow::Result<()> {
        let mut instances = self.instances.lock().await;
        if instances.contains_key(history_key) {
            return Ok(());
        }

        let mut instance = self.spawn_pi().await?;

        // Load existing session or create new
        if let Some(session_file) = self.session_store.load(history_key) {
            if std::path::Path::new(&session_file).exists() {
                rpc::rpc_switch_session(&mut instance.stdin, &mut instance.stdout, &session_file).await;
                instance.session_file = Some(session_file);
            } else {
                rpc::rpc_new_session(&mut instance.stdin, &mut instance.stdout).await;
            }
        } else {
            rpc::rpc_new_session(&mut instance.stdin, &mut instance.stdout).await;
        }

        instances.insert(history_key.to_string(), instance);
        Ok(())
    }
```

- [ ] **Step 4: Implement prompt**

```rust
    pub async fn prompt(
        &self,
        history_key: &str,
        message: &str,
        mut on_event: impl FnMut(rpc::PiEvent),
    ) -> anyhow::Result<String> {
        let mut instances = self.instances.lock().await;
        let instance = instances.get_mut(history_key)
            .ok_or_else(|| anyhow::anyhow!("Pi not running for {}", history_key))?;

        instance.last_active = Instant::now();

        let result = rpc::rpc_prompt(
            &mut instance.stdin,
            &mut instance.stdout,
            message,
            &mut on_event,
            Duration::from_secs(300),
        ).await?;

        // Save session file after prompt
        if let Some(sf) = rpc::rpc_get_session_file(&mut instance.stdin, &mut instance.stdout).await {
            instance.session_file = Some(sf.clone());
            self.session_store.save(history_key, &sf);
        }

        Ok(result)
    }
```

- [ ] **Step 5: Implement stop + kill_idle**

```rust
    pub async fn stop(&self, history_key: &str) -> anyhow::Result<()> {
        let mut instances = self.instances.lock().await;
        if let Some(mut instance) = instances.remove(history_key) {
            // Save session before kill
            if let Some(sf) = rpc::rpc_get_session_file(&mut instance.stdin, &mut instance.stdout).await {
                self.session_store.save(history_key, &sf);
            }
            let _ = instance._process.kill().await;
        }
        Ok(())
    }

    pub async fn stop_all(&self) {
        let mut instances = self.instances.lock().await;
        for (key, mut inst) in instances.drain() {
            if let Some(sf) = rpc::rpc_get_session_file(&mut inst.stdin, &mut inst.stdout).await {
                self.session_store.save(&key, &sf);
            }
            let _ = inst._process.kill().await;
        }
    }

    pub async fn kill_idle(&self, max_idle: Duration) {
        let mut instances = self.instances.lock().await;
        let idle_keys: Vec<String> = instances.iter()
            .filter(|(_, inst)| inst.last_active.elapsed() > max_idle)
            .map(|(key, _)| key.clone())
            .collect();
        for key in idle_keys {
            if let Some(mut inst) = instances.remove(&key) {
                tracing::info!(history_key = %key, "Killing idle Pi instance");
                if let Some(sf) = rpc::rpc_get_session_file(&mut inst.stdin, &mut inst.stdout).await {
                    self.session_store.save(&key, &sf);
                }
                let _ = inst._process.kill().await;
            }
        }
    }

    pub fn is_running(&self, history_key: &str) -> bool {
        self.instances.blocking_lock().contains_key(history_key)
    }

    pub fn needs_history_injection(&self, history_key: &str) -> bool {
        self.instances.blocking_lock()
            .get(history_key)
            .is_some_and(|i| !i.history_injected)
    }
```

- [ ] **Step 6: Implement inject_history**

```rust
    pub async fn inject_history(
        &self,
        history_key: &str,
        messages: &[crate::providers::ChatMessage],
        token_limit: usize,
    ) -> anyhow::Result<()> {
        let mut instances = self.instances.lock().await;
        let instance = instances.get_mut(history_key)
            .ok_or_else(|| anyhow::anyhow!("Pi not running"))?;

        if instance.history_injected { return Ok(()); }

        // Format history as context prompt (estimate 4 chars/token)
        let char_limit = token_limit * 4;
        let mut context = String::from("[Previous conversation context]\n\n");
        let mut total_chars = context.len();

        for msg in messages.iter().rev() {
            let role = if msg.role == "user" { "User" } else { "Assistant" };
            let line = format!("{}: {}\n\n", role, msg.content);
            if total_chars + line.len() > char_limit { break; }
            context.insert_str("[Previous conversation context]\n\n".len(), &line);
            total_chars += line.len();
        }

        if total_chars > "[Previous conversation context]\n\n".len() {
            rpc::rpc_prompt(
                &mut instance.stdin,
                &mut instance.stdout,
                &context,
                |_| {}, // ignore events for context injection
                Duration::from_secs(60),
            ).await?;
        }

        instance.history_injected = true;
        Ok(())
    }
```

- [ ] **Step 7: Run tests → PASS, commit**

```bash
git add src/pi/mod.rs
git commit -m "feat(pi): add PiManager with spawn, prompt, stop, idle reaper"
```

---

### Task 6: Integration — channels/mod.rs + daemon

Wire PiManager into ZeroClaw's message processing and daemon lifecycle.

**Files:**
- Modify: `src/channels/mod.rs` — replace spawn_coder_subprocess, add /models pi
- Modify: `src/daemon/mod.rs` — start idle reaper
- Modify: `src/pi/mod.rs` — add global PiManager singleton

- [ ] **Step 1: Add global PiManager singleton**

In `src/pi/mod.rs`:
```rust
static PI_MANAGER: std::sync::OnceLock<Arc<PiManager>> = std::sync::OnceLock::new();

pub fn init_pi_manager(workspace_dir: &Path, minimax_key: &str) {
    let _ = PI_MANAGER.set(Arc::new(PiManager::new(workspace_dir, minimax_key)));
}

pub fn pi_manager() -> Option<Arc<PiManager>> {
    PI_MANAGER.get().cloned()
}
```

- [ ] **Step 2: Initialize in daemon startup**

In `src/daemon/mod.rs`, after config load:
```rust
// Initialize Pi manager
let minimax_key = config.reliability.fallback_api_keys
    .get("minimax-cn:mm-1")
    .or_else(|| config.reliability.fallback_api_keys.get("minimax:mm-1"))
    .cloned()
    .unwrap_or_default();
crate::pi::init_pi_manager(&config.workspace_dir, &minimax_key);

// Start idle reaper
if let Some(manager) = crate::pi::pi_manager() {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            manager.kill_idle(Duration::from_secs(30 * 60)).await;
        }
    });
}
```

- [ ] **Step 3: Add `/models pi` to handle_models_command**

In `src/channels/mod.rs`, inside `handle_models_command`:
```rust
Some(hint) => {
    // Special: Pi mode
    if hint.eq_ignore_ascii_case("pi") {
        current.pi_mode = true;
        set_route_selection(ctx, sender_key, current.clone());
        if let Some(mgr) = crate::pi::pi_manager() {
            let _ = mgr.ensure_running(sender_key).await;
        }
        return "✅ Pi mode activated. All messages go to coding agent.\nTo exit: /models minimax".to_string();
    }

    // Deactivate Pi when switching to another model
    if current.pi_mode {
        current.pi_mode = false;
        if let Some(mgr) = crate::pi::pi_manager() {
            let _ = mgr.stop(sender_key).await;
        }
    }

    // ... existing model route matching ...
}
```

- [ ] **Step 4: Replace handle_pi_bypass_if_needed**

Replace `spawn_coder_subprocess` path with PiManager:
```rust
async fn handle_pi_bypass_if_needed(
    ctx: &ChannelRuntimeContext,
    msg: &traits::ChannelMessage,
    channel: Option<&Arc<dyn Channel>>,
) -> bool {
    let history_key = conversation_history_key(msg);
    let is_pi_mode = global_route_overrides()
        .lock().unwrap_or_else(|e| e.into_inner())
        .get(&history_key)
        .is_some_and(|r| r.pi_mode);

    if is_pi_stop(&msg.content) && is_pi_mode {
        // ... existing stop logic (keep as-is) ...
        return true;
    }

    // Detect explicit "пи, ..." prefix OR active pi_mode
    let message = if let Some(stripped) = detect_pi_prefix(&msg.content) {
        // Activate pi_mode if not already
        // ... existing activation logic ...
        stripped
    } else if is_pi_mode {
        msg.content.clone()
    } else {
        return false;
    };

    let Some(mgr) = crate::pi::pi_manager() else { return false; };

    // Ensure Pi is running
    if let Err(e) = mgr.ensure_running(&history_key).await {
        tracing::error!("Failed to start Pi: {e}");
        return false;
    }

    // Inject history on first activation
    if mgr.needs_history_injection(&history_key) {
        let history = ctx.conversation_histories
            .lock().unwrap_or_else(|e| e.into_inner())
            .get(&history_key).cloned().unwrap_or_default();
        let _ = mgr.inject_history(&history_key, &history, 100_000).await;
    }

    // Create Telegram status notifier
    let bot_token = std::env::var("TELEGRAM_BOT_TOKEN").unwrap_or_default();
    let notifier = crate::pi::telegram::TelegramNotifier::new(&bot_token, &msg.reply_target);
    let status_msg_id = notifier.send_status("⚙ Starting Pi…").await;
    let mut status_builder = crate::pi::status::StatusBuilder::new();

    // Prompt Pi with live status
    let result = mgr.prompt(&history_key, &message, |event| {
        match &event {
            rpc::PiEvent::ThinkingEnd(text) => status_builder.on_thinking_end(text),
            rpc::PiEvent::ToolStart { name, args } => status_builder.on_tool_start(name, args),
            rpc::PiEvent::ToolEnd { name, output } => status_builder.on_tool_end(name, output),
            rpc::PiEvent::AgentEnd { text } => status_builder.on_response_text(text),
            _ => {}
        }
        if let Some(msg_id) = status_msg_id {
            // Fire-and-forget status update (debounced in StatusBuilder)
            let text = status_builder.render();
            let n = notifier.clone();
            tokio::spawn(async move { n.edit_status(msg_id, &text).await; });
        }
    }).await;

    match result {
        Ok(response) => {
            // Edit final response
            if let Some(msg_id) = status_msg_id {
                notifier.edit_status(msg_id, &response).await;
            }
            record_pi_turn(ctx, &history_key, &msg.content, &response);
        }
        Err(err) => {
            let error_text = format!("⚠️ Pi error: {}", err);
            if let Some(msg_id) = status_msg_id {
                notifier.edit_status(msg_id, &error_text).await;
            }
            record_pi_turn(ctx, &history_key, &msg.content, &format!("[Error] {err}"));
        }
    }

    true
}
```

- [ ] **Step 5: Remove old spawn_coder_subprocess + build_coder_env + coder.py references**

Delete functions that are no longer needed:
- `spawn_coder_subprocess` (~lines 1092-1140)
- `build_coder_env` (~lines 1028-1075)
- Keep `detect_pi_prefix`, `is_pi_stop`, `record_pi_turn`

- [ ] **Step 6: Run cargo test --lib → PASS**

- [ ] **Step 7: Commit**

```bash
git add src/pi/ src/channels/mod.rs src/daemon/mod.rs src/lib.rs
git commit -m "feat(pi): integrate PiManager into channels and daemon"
```

---

### Task 7: E2E Tests

**Files:**
- Create: `/tmp/test_pi_persistent_e2e.py`

- [ ] **Step 1: Write E2E test**

Tests:
1. `/models pi` → Pi mode activated
2. Send messages WITHOUT prefix → Pi handles all with context
3. File write + read back → context preserved within session
4. `/models minimax` → LLM takes over, Pi stopped
5. LLM sees `[Pi]` history
6. `/models pi` again → Pi loads saved session, old context available

- [ ] **Step 2: Build release + restart daemon**

```bash
cargo build --release && ./dev/restart-daemon.sh
```

- [ ] **Step 3: Run E2E → ALL PASS**

- [ ] **Step 4: Commit E2E test + final push**

---

## Verification

```bash
# Unit tests
cargo test --lib -- pi::rpc
cargo test --lib -- pi::status
cargo test --lib -- pi::session

# Full suite
cargo test --lib

# Clippy
cargo clippy --all-targets -- -D warnings

# Build + restart
cargo build --release && ./dev/restart-daemon.sh

# E2E:
# 1. /models pi → "✅ Pi mode activated"
# 2. "сколько 2+2?" → Pi: "4" (no prefix needed)
# 3. "запиши mango в /tmp/test.txt" → Pi writes file
# 4. "прочитай /tmp/test.txt" → Pi: "mango" (context in same process)
# 5. /models minimax → LLM takes over
# 6. "что Pi делал?" → LLM mentions mango, 2+2
# 7. /models pi → Pi session restored, knows about mango
```
