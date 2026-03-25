# OC Polling Live Status Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Show real-time thinking/tool status in Telegram during OpenCode processing by polling the OC messages API.

**Architecture:** Replace animated dots with polling `GET /session/{id}/message` every 2s. New parts (tool calls, thinking text) are extracted and shown as Telegram status edits. The blocking `send_message` stays in a background tokio task; the foreground polls messages until `step-finish` with `reason: "stop"`.

**Tech Stack:** Rust, tokio, reqwest, Telegram Bot API (edit_message_text)

---

### Task 1: Add `get_messages` to OpenCodeClient

**Files:**
- Modify: `src/opencode/client.rs:200-300`

- [ ] **Step 1: Add `get_messages` method**

```rust
/// Fetch all messages for a session. Used for polling progress.
pub async fn get_messages(&self, session_id: &str) -> ClientResult<Vec<MessageResponse>> {
    let url = format!("{}/session/{}/message", self.base_url, session_id);
    let resp = self.apply_auth(self.http.get(&url)).send().await?;
    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        return Err(OpenCodeError::ServerError { status, body });
    }
    Ok(resp.json().await?)
}
```

Add this after the existing `send_message` method (~line 236).

- [ ] **Step 2: Verify it compiles**

Run: `cargo build 2>&1 | tail -3`
Expected: `Finished`

- [ ] **Step 3: Commit**

```bash
git add src/opencode/client.rs
git commit -m "feat(opencode): add get_messages for polling session progress"
```

---

### Task 2: Add `prompt_with_polling` to OpenCodeManager

**Files:**
- Modify: `src/opencode/mod.rs`

This is the core change. Add a new method that:
1. Sends message (blocking, in background task)
2. Polls `get_messages` every 2s
3. Calls `on_status` callback with new tool/text parts
4. Returns final text when done

- [ ] **Step 1: Add the method**

After the existing `prompt()` method (~line 377), add:

```rust
/// Prompt with polling-based live status updates.
///
/// Instead of relying on SSE (which OC 1.3 doesn't forward to the /event
/// endpoint), this method polls `GET /session/{id}/message` every 2s and
/// reports new parts (tool calls, thinking) via `on_status`.
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

    // Inject history if needed (same as prompt())
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

    // Send message in background task (blocks until OC completes)
    let send_handle = tokio::spawn(async move {
        http.send_message(&sid, &msg, &provider, &model).await
    });

    // Poll messages every 2s until send completes
    let on_status = Arc::new(on_status);
    let mut seen_parts = 0usize;
    let poll_interval = std::time::Duration::from_secs(2);

    loop {
        tokio::time::sleep(poll_interval).await;

        // Check if send completed
        if send_handle.is_finished() {
            break;
        }

        // Poll messages
        if let Ok(messages) = self.http_client.get_messages(&session_id).await {
            let total_parts: usize = messages.iter().map(|m| m.parts.len()).sum();
            if total_parts > seen_parts {
                // Process new parts
                for msg in &messages {
                    if msg.info.role != "assistant" {
                        continue;
                    }
                    for (i, part) in msg.parts.iter().enumerate() {
                        let global_idx = i; // simplified — works for single assistant msg
                        if global_idx < seen_parts {
                            continue;
                        }
                        match part.kind.as_str() {
                            "tool" => {
                                let tool_name = part.extra.get("tool")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("tool");
                                let status = part.extra.pointer("/state/status")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("running");
                                on_status(PollingStatus::Tool {
                                    name: tool_name.to_string(),
                                    status: status.to_string(),
                                });
                            }
                            "text" => {
                                if let Some(t) = &part.text {
                                    let preview = t.chars().take(80).collect::<String>();
                                    if !preview.is_empty() {
                                        on_status(PollingStatus::Thinking(preview));
                                    }
                                }
                            }
                            "step-start" => {
                                on_status(PollingStatus::StepStart);
                            }
                            "step-finish" => {
                                // Will exit on next loop when send_handle finishes
                            }
                            _ => {}
                        }
                    }
                }
                seen_parts = total_parts;
            }
        }
    }

    // Get result from send task
    let result = send_handle.await
        .map_err(|e| anyhow::anyhow!("send task panicked: {e}"))?;

    self.touch_session(history_key).await;

    match result {
        Ok(response) => {
            let text = response.text();
            if text.is_empty() {
                // Fallback: read final text from polled messages
                if let Ok(messages) = self.http_client.get_messages(&session_id).await {
                    let final_text: String = messages.iter()
                        .filter(|m| m.info.role == "assistant")
                        .flat_map(|m| m.parts.iter())
                        .filter(|p| p.kind == "text")
                        .filter_map(|p| p.text.as_deref())
                        .collect::<Vec<_>>()
                        .join("");
                    Ok(final_text)
                } else {
                    Ok(String::new())
                }
            } else {
                Ok(text)
            }
        }
        Err(e) => Err(anyhow::anyhow!("OC prompt error: {e}")),
    }
}
```

Also add the status enum before the impl block:

```rust
/// Status updates from polling OC messages.
#[derive(Debug, Clone)]
pub enum PollingStatus {
    /// Model is thinking — preview of text so far
    Thinking(String),
    /// Tool call started or completed
    Tool { name: String, status: String },
    /// New reasoning step started
    StepStart,
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo build 2>&1 | tail -3`

- [ ] **Step 3: Commit**

```bash
git add src/opencode/mod.rs
git commit -m "feat(opencode): add prompt_with_polling for live status via message API"
```

---

### Task 3: Wire polling into handle_oc_bypass_if_needed

**Files:**
- Modify: `src/channels/mod.rs:1190-1310`

Replace the animated dots + dead SSE callback with `prompt_with_polling`, forwarding `PollingStatus` to Telegram status edits.

- [ ] **Step 1: Replace the prompt call and dots animation**

In `handle_oc_bypass_if_needed`, replace everything from `// Animated progress dots` (line ~1217) through the `mgr.prompt(...)` call (line ~1284) with:

```rust
    // Use polling-based live status updates.
    let notifier_poll = Arc::clone(&notifier);
    let last_edit_ms = Arc::new(AtomicI64::new(0i64));
    let status_msg_id_poll = status_msg_id;

    let result = mgr
        .prompt_with_polling(&history_key, &oc_message, history_ref, move |status| {
            use crate::opencode::PollingStatus;
            let text = match &status {
                PollingStatus::Thinking(preview) => {
                    format!("\u{1f4ad} {}", preview)
                }
                PollingStatus::Tool { name, status } => {
                    if status == "completed" {
                        format!("\u{2705} `{name}` done")
                    } else {
                        format!("\u{2699}\u{fe0f} Running `{name}`\u{2026}")
                    }
                }
                PollingStatus::StepStart => "\u{1f4ad} Thinking\u{2026}".to_string(),
            };
            // Throttle edits to 2s minimum
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
                .saturating_mul(1000) as i64;
            let last = last_edit_ms.load(Ordering::Relaxed);
            if now_ms - last < 2000 {
                return;
            }
            if last_edit_ms
                .compare_exchange(last, now_ms, Ordering::Relaxed, Ordering::Relaxed)
                .is_err()
            {
                return;
            }
            let notifier_inner = Arc::clone(&notifier_poll);
            if let Some(msg_id) = status_msg_id_poll {
                tokio::spawn(async move {
                    notifier_inner.edit_status(msg_id, &text).await;
                });
            }
        })
        .await;
```

Remove the dots_stop/dots_handle/dots animation code entirely.
Remove the dead SSE callback code.

- [ ] **Step 2: Remove dots cleanup after prompt**

Remove `dots_stop.cancel()` and `dots_handle.abort()` lines since they no longer exist.

- [ ] **Step 3: Verify it compiles**

Run: `cargo clippy --all-targets -- -D warnings 2>&1 | tail -5`

- [ ] **Step 4: Build release and test manually**

```bash
cargo build --release && ./dev/restart-daemon.sh
```

Send "пи, скажи ок" to bot via Telegram — should see status updates instead of dots.

- [ ] **Step 5: Run existing tests**

```bash
set -a && source .env && set +a
cargo test --test telegram_search_quality i13 -- --ignored --test-threads=1
```

Expected: i13 passes (bot responds after /new).

- [ ] **Step 6: Commit**

```bash
git add src/channels/mod.rs
git commit -m "feat(channels): polling-based live status for OC bypass

Replace animated dots with real-time status from OC message API.
Shows thinking preview and tool call names in Telegram during processing."
```

---

### Task 4: Cleanup — remove unused SSE callback infrastructure

**Files:**
- Modify: `src/opencode/mod.rs`

The old `prompt()` method with SSE can be kept as fallback but the SSE event types used only for status display in the callback can be simplified.

- [ ] **Step 1: Add `pub use PollingStatus` to mod.rs exports**

Ensure `PollingStatus` is accessible from `src/channels/mod.rs`.

- [ ] **Step 2: Run full test suite**

```bash
cargo test --lib 2>&1 | tail -5
```

Expected: all unit tests pass.

- [ ] **Step 3: Final commit**

```bash
cargo fmt --all
git add -A
git commit -m "style: cargo fmt after OC polling feature"
git push origin main
```
