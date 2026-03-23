# Pi Admin Access Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give Pi full admin access to ZeroClaw — gateway API credentials, chat history reading, Russian system prompt. Enable Pi to manage memory, cron, config, skills, and chats.

**Architecture:** 3 independent tasks: (1) gateway env vars in PiManager spawn, (2) GET history API endpoint, (3) Russian system prompt. All can run in parallel.

**Tech Stack:** Rust, axum (gateway), tokio

**Spec:** `docs/superpowers/specs/2026-03-23-pi-admin-access-design.md`

---

## File Structure

| File | Action | Responsibility |
|------|--------|---------------|
| `src/pi/mod.rs` | MODIFY | Add gateway_url/token fields, pass env vars in spawn_pi, add --append-system-prompt |
| `src/daemon/mod.rs` | MODIFY | Pass gateway creds to init_pi_manager after gateway starts |
| `src/gateway/api.rs` | MODIFY | Add GET /api/history/{sender_key} handler |
| `src/gateway/mod.rs` | MODIFY | Register GET history route |

---

### Task 1: Gateway env vars + Russian prompt in PiManager

**Files:**
- Modify: `src/pi/mod.rs` — add gateway_url/token fields + env vars in spawn_pi + system prompt
- Modify: `src/daemon/mod.rs` — pass gateway creds to init_pi_manager

- [ ] **Step 1: Add gateway fields to PiManager**

In `src/pi/mod.rs`, add to PiManager struct:
```rust
gateway_url: String,
gateway_token: String,
```

Update `new()` signature to accept them. Update `init_pi_manager()` to accept them.

- [ ] **Step 2: Pass gateway env vars in spawn_pi**

In `spawn_pi()`, add after the existing `.env(api_key_env, &self.api_key)`:
```rust
.env("ZEROCLAW_GATEWAY_URL", &self.gateway_url)
.env("ZEROCLAW_GATEWAY_TOKEN", &self.gateway_token)
.env("ZEROCLAW_WORKSPACE", self.workspace_dir.to_string_lossy().as_ref())
```

- [ ] **Step 3: Add Russian system prompt**

In `spawn_pi()`, add `--append-system-prompt` to args:
```rust
.args([
    "--mode", "rpc",
    "--provider", &self.provider,
    "--model", &self.model,
    "--thinking", &self.thinking,
    "--append-system-prompt",
    "Думай и отвечай на русском языке. Ты — admin-агент ZeroClaw. У тебя полный доступ к gateway API через env vars ZEROCLAW_GATEWAY_URL и ZEROCLAW_GATEWAY_TOKEN. Используй curl для вызова API: memory (GET/POST/DELETE /api/memory), cron (GET/POST/DELETE /api/cron), config (GET/PUT /api/config), history (GET/DELETE /api/history/{key}), health (GET /api/health). Также можешь редактировать skills в ~/.zeroclaw/workspace/skills/, исходный код в ~/work/erp/zeroclaws/, собирать cargo build --release и перезапускать ./dev/restart-daemon.sh.",
    "--cwd",
])
```

- [ ] **Step 4: Update daemon init**

In `src/daemon/mod.rs`, after gateway starts, get gateway creds:
```rust
// Gateway creds for Pi (available after set_service_token_context)
let (pi_gw_token, pi_gw_url) = crate::skills::get_gateway_creds_for_skill("coder")
    .unwrap_or_default();
crate::pi::init_pi_manager(
    &config.workspace_dir,
    &pi_api_key,
    &config.pi.provider,
    &config.pi.model,
    &config.pi.thinking,
    &pi_gw_url,
    &pi_gw_token,
);
```

IMPORTANT: `set_service_token_context` is called in gateway startup. Pi init must happen AFTER gateway starts. Check current order — if Pi init is before gateway, move it after.

- [ ] **Step 5: Fix test constructors**

Update all `PiManager::new(...)` calls in tests to pass empty strings for gateway_url/token:
```rust
PiManager::new(tmp.path(), "fake-key", "minimax", "test", "off", "", "")
```

- [ ] **Step 6: Run tests + commit**

```bash
cargo test --lib -- pi::
cargo fmt --all
git commit -m "feat(pi): add gateway credentials + Russian system prompt to Pi spawn"
```

---

### Task 2: GET /api/history endpoint

**Files:**
- Modify: `src/gateway/api.rs` — add handler
- Modify: `src/gateway/mod.rs` — register route

- [ ] **Step 1: Add GET handler in api.rs**

Add next to `handle_api_history_delete` (around line 1287):

```rust
/// GET /api/history/{sender_key} — read conversation history for a sender.
pub async fn handle_api_history_get(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(sender_key): Path<String>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let messages = if let Some(ref histories) = state.conversation_histories {
        let map = histories.lock().unwrap_or_else(|e| e.into_inner());
        map.get(&sender_key)
            .map(|turns| {
                turns.iter().map(|m| {
                    serde_json::json!({
                        "role": m.role,
                        "content": m.content,
                    })
                }).collect::<Vec<_>>()
            })
            .unwrap_or_default()
    } else {
        vec![]
    };

    Json(serde_json::json!({
        "sender_key": sender_key,
        "messages": messages,
        "count": messages.len(),
    })).into_response()
}
```

Note: `ChatMessage` has `role` and `content` fields (check `src/providers/traits.rs`).

- [ ] **Step 2: Register route in mod.rs**

In `src/gateway/mod.rs`, find the delete history route (line 865-866):
```rust
"/api/history/{sender_key}",
delete(api::handle_api_history_delete),
```

Change to handle both GET and DELETE:
```rust
"/api/history/{sender_key}",
get(api::handle_api_history_get).delete(api::handle_api_history_delete),
```

- [ ] **Step 3: Run tests + commit**

```bash
cargo test --lib
cargo fmt --all
git commit -m "feat(gateway): add GET /api/history/{sender_key} endpoint"
```

---

### Task 3: E2E tests — Pi admin capabilities

**Files:**
- Create: `/tmp/test_pi_admin_e2e.py`

- [ ] **Step 1: Write E2E test script**

Tests:
1. `/models pi` → "какие env vars у тебя есть с ZEROCLAW?" → Pi lists ZEROCLAW_GATEWAY_URL, ZEROCLAW_GATEWAY_TOKEN
2. `/models pi` → "прочитай memory через API" → Pi calls `curl GET /api/memory`
3. `/models pi` → "создай тестовую cron задачу" → Pi calls `curl POST /api/cron`
4. `/models pi` → "удали эту cron задачу" → Pi calls `curl DELETE /api/cron/{id}`
5. `/models pi` → "прочитай историю чата" → Pi calls `curl GET /api/history/{key}`
6. `/models pi` → "проверь здоровье системы" → Pi calls `curl GET /api/health`
7. Verify Pi responds in Russian
8. `/models minimax` → cleanup

Use standard Telethon E2E pattern with `wait_reply` that handles Pi edit-in-place.

- [ ] **Step 2: Build release + restart daemon**

```bash
cargo build --release && ./dev/restart-daemon.sh
```

- [ ] **Step 3: Run E2E**

```bash
echo '{}' > ~/.zeroclaw/workspace/routes.json
~/.zeroclaw/workspace/.venv/bin/python3 /tmp/test_pi_admin_e2e.py
```

- [ ] **Step 4: Commit test + push**

```bash
git push origin main
```

---

### Task 4: Fix Pi prompt queue + stream recovery

**Files:**
- Modify: `src/pi/mod.rs` — add prompt lock + stream recovery

- [ ] **Step 1: Add per-instance prompt lock**

Pi returns `"Agent is already processing"` if a second prompt arrives while first is running. Add a `Mutex<()>` guard per instance so messages queue instead of failing:

In PiInstance struct, add:
```rust
prompt_lock: Arc<tokio::sync::Mutex<()>>,
```

In `prompt()`, acquire lock before sending:
```rust
pub async fn prompt(&self, history_key: &str, message: &str, on_event: impl Fn(&PiEvent)) -> anyhow::Result<String> {
    let mut instances = self.instances.lock().await;
    let instance = instances.get_mut(history_key)
        .ok_or_else(|| anyhow::anyhow!("Pi not running"))?;

    let lock = instance.prompt_lock.clone();
    drop(instances); // release instances lock

    let _guard = lock.lock().await; // wait for previous prompt to finish

    // Now re-acquire instances and send prompt
    let mut instances = self.instances.lock().await;
    let instance = instances.get_mut(history_key)
        .ok_or_else(|| anyhow::anyhow!("Pi died while waiting"))?;
    // ... existing prompt logic ...
}
```

- [ ] **Step 2: Handle "stream ended without agent_end"**

In `src/pi/rpc.rs` `rpc_prompt()`, when `recv_line` returns `None` (EOF/broken pipe), don't bail immediately. Check if Pi process is still alive:

Find the event loop in `rpc_prompt` and update the `None` case:
```rust
let val = match recv_line(reader, Duration::from_secs(5).min(remaining)).await {
    Some(v) => v,
    None => {
        // Check if we've been reading for a while without agent_end
        // Pi might have crashed or connection broken
        if deadline.saturating_duration_since(tokio::time::Instant::now()).is_zero() {
            anyhow::bail!("Pi prompt timed out after {:?}", dur);
        }
        continue; // retry — might be a temporary read gap
    }
};
```

The current code does `anyhow::bail!("stream ended without agent_end")` on first `None`. But `recv_line` can return `None` on timeout (5s) even if Pi is still working. Only bail on overall deadline exceeded.

- [ ] **Step 3: Add last active chat tracking**

Store which chat Pi was last active in, so if Pi is processing for chat A and chat B sends a message, chat B gets a clear error instead of confusing "Agent is already processing":

In `handle_pi_bypass_if_needed`, before calling `mgr.prompt()`:
```rust
// Show "Pi is busy with another request, please wait..." if prompt lock is held
```

Actually simpler: the `prompt_lock` will just queue — second message waits for first to complete. User sees "⚙ Pi is working…" status for both, second gets response after first finishes. No error needed.

- [ ] **Step 4: Run tests + commit**

```bash
cargo test --lib -- pi::
cargo fmt --all
git commit -m "fix(pi): add prompt queue lock + handle stream recovery"
```

---

### Task 5: E2E — prompt queue + error recovery

**Files:**
- Add to E2E test script

- [ ] **Step 1: Test rapid messages**

Send 2 messages quickly to Pi (within 1s):
```python
s1 = await client.send_message(bot, "скажи: first")
s2 = await client.send_message(bot, "скажи: second")
r1 = await wait_reply(client, bot, s1.id)
r2 = await wait_reply(client, bot, s2.id)
# Both should get responses (second queued, not error)
check("rapid msg 1", "first" in r1.lower())
check("rapid msg 2", "second" in r2.lower())
```

- [ ] **Step 2: Test recovery after error**

After any Pi error, next message should still work (Pi re-spawns if needed).

---

## Verification

```bash
# Unit tests
cargo test --lib -- pi::
cargo test --lib

# Clippy
cargo clippy --all-targets -- -D warnings

# Build + restart
cargo build --release && ./dev/restart-daemon.sh

# E2E — Pi admin capabilities:
# 1. Pi has ZEROCLAW_GATEWAY_URL/TOKEN env vars
# 2. Pi can curl gateway API (memory, cron, history, health)
# 3. Pi responds in Russian
# 4. Pi can create/delete cron jobs
# 5. Pi can read chat history via API
```
