# OpenCode Integration — Implementation Plan

**Spec:** `docs/superpowers/specs/2026-03-24-opencode-integration-design.md`
**Risk tier:** Medium (new module, no security/gateway boundary)

---

## Implementation Order

Steps must be done in dependency order. Steps within the same number can run in parallel.

---

### Step 1 — Scaffold module + config schema

**Files:** `src/opencode/mod.rs` (stub), `src/lib.rs`, `src/config/schema.rs`, `src/daemon/mod.rs`

1. Create `src/opencode/mod.rs` with stub submodule declarations:
   ```rust
   pub mod config;
   pub mod session;
   pub mod client;
   pub mod events;
   pub mod process;
   pub mod status;
   pub mod telegram;
   ```
2. Add `pub mod opencode;` to `src/lib.rs` after `pub mod pi;`.
3. Add `OpenCodeConfig` struct to `src/config/schema.rs` after `PiConfig`, same derive pattern:
   - Fields: `enabled: bool` (default false), `port: u16` (14096), `hostname: String` ("127.0.0.1"), `provider: String` ("minimax"), `model: String` ("MiniMax-M2.7-highspeed"), `base_url: String` ("https://api.minimax.chat/v1"), `api_key_profile: Option<String>`, `idle_timeout_secs: u64` (1800), `history_inject_limit: usize` (50), `history_inject_max_chars: usize` (50000)
   - Wire into `ZeroClawConfig` with `pub opencode: OpenCodeConfig` + `#[serde(default)]`
   - Update all three `ZeroClawConfig::default()` calls in schema.rs
4. In `src/daemon/mod.rs`: after the `pi_api_key` resolution block, add `opencode_api_key` resolution via `config.opencode.api_key_profile` → `config.reliability.fallback_api_keys`.

**Validate:** `cargo build` compiles. `cargo test --lib` passes.

---

### Step 2 — `config.rs` + `session.rs` (parallel, no dependencies between them)

#### 2a: `src/opencode/config.rs`

Private serialization structs matching confirmed OpenCode JSON format:
- `OpencodeJsonServer { port, hostname }`
- `OpencodeJsonProviderOptions { api_key (→ "apiKey"), base_url (→ "baseURL") }`
- `OpencodeJsonProvider { npm: "@ai-sdk/openai-compatible", name, options, models: HashMap<String, serde_json::Value> }`
- `OpencodeJsonCompaction { auto: bool }`
- `OpencodeJson { server, provider: HashMap<String, OpencodeJsonProvider>, model, compaction }`

Public function:
```rust
pub async fn write_opencode_config(
    config: &OpenCodeConfig,
    api_key: &str,
    workspace_dir: &Path,
) -> anyhow::Result<PathBuf>
```
- Validate: bail if `api_key.is_empty()`
- `create_dir_all(workspace_dir/opencode/)`
- Build `OpencodeJson`, serialize with `serde_json::to_string_pretty`
- Atomic write: write to `.tmp` then `rename` (prevents corrupt read on crash)
- Return path to `opencode.json`

Tests: valid JSON round-trip, empty key → Err, parent dir created, no leftover `.tmp` file.

#### 2b: `src/opencode/session.rs`

Near-copy of `src/pi/session.rs`. Store maps `history_key → SessionEntry`.

```rust
struct SessionEntry {
    opencode_session_id: String,
    created_at: String,  // RFC3339
    last_active: String, // RFC3339 (updated on each prompt)
}

pub struct OpenCodeSessionStore { path: PathBuf }
```

Public API: `get(key) -> Option<String>`, `set(key, session_id)`, `remove(key)`, `load_from_disk() -> HashMap<String, String>`, `save_to_disk(map)`.

Private: `read_map() -> HashMap<String, SessionEntry>`, `write_map(map)`.

Error handling: corrupted JSON → `tracing::error!` + return empty map (do not panic, do not fail).

Tests: set/get roundtrip, missing key → None, remove, corrupted JSON → empty (no panic), load_from_disk/save_to_disk roundtrip.

**Validate:** `cargo test --lib -- opencode::` passes.

---

### Step 3 — `client.rs` (depends on Step 1)

HTTP client wrapping `reqwest::Client`. All calls to `http://127.0.0.1:{port}`.

```rust
pub struct OpenCodeClient {
    http: reqwest::Client,  // timeout=600s, connect_timeout=5s, no_proxy()
    base_url: String,
    password: Option<String>,  // from OPENCODE_SERVER_PASSWORD env
}

impl OpenCodeClient {
    pub fn new(port: u16) -> Self
    pub fn with_base_url(base_url: impl Into<String>) -> Self  // for tests
}
```

Error type:
```rust
pub enum OpenCodeError {
    Http(#[from] reqwest::Error),
    ServerError { status: u16, body: String },
    SessionNotFound { session_id: String },
    SseTimeout { secs: u64 },
}
pub type ClientResult<T> = Result<T, OpenCodeError>;
```

Methods (all `async`, return `ClientResult<T>`):
- `health_check(&self) -> ClientResult<()>` — GET /path → 2xx
- `create_session(&self, directory: &str) -> ClientResult<String>` — POST /session → id
- `get_session(&self, session_id: &str) -> ClientResult<Option<SessionInfo>>` — GET /session/{id}, 404 → Ok(None)
- `send_message(&self, session_id, text, model_id, provider_id) -> ClientResult<MessageResponse>` — POST /session/{id}/message
- `send_message_no_reply(&self, session_id, text, model_id, provider_id) -> ClientResult<()>` — same + noReply:true
- `send_message_async(&self, session_id, text, model_id, provider_id) -> ClientResult<()>` — POST /session/{id}/prompt_async → 204
- `abort(&self, session_id) -> ClientResult<bool>` — POST /session/{id}/abort → bool
- `delete_session(&self, session_id) -> ClientResult<()>` — DELETE /session/{id}, 404 → Ok(())
- `subscribe_events(&self) -> ClientResult<ReceiverStream<ClientResult<OpenCodeEvent>>>` — GET /event SSE

All requests apply basic auth via private `apply_auth(req) -> req` helper.

Confirmed wire formats (tested live):
- POST /session body: `{}` with `x-opencode-directory` header
- POST /session/{id}/message body: `{"parts":[{"type":"text","text":"..."}],"model":{"providerID":"...","modelID":"..."}}`
- SSE delta: `{"type":"message.part.delta","properties":{"sessionID":"...","field":"text","delta":"..."}}`

Tests via `wiremock::MockServer`: health_check_ok, health_check_server_error, create_session_returns_id, create_session_directory_header, get_session_found, get_session_not_found, send_message_success, send_message_server_error, send_message_async_204, abort_true, abort_false, delete_ok, delete_not_found_is_ok, basic_auth_header_present.

**Validate:** `cargo test --lib -- opencode::client` passes.

---

### Step 4 — `events.rs` (depends on Step 3)

SSE consumer: parse raw bytes from `client.subscribe_events()` into typed events.

```rust
pub enum OpenCodeEvent {
    TextDelta(String),
    ThinkingDelta(String),
    ToolStart { name: String },
    ToolEnd { name: String },
    SessionIdle,
    Heartbeat,
    Connected,
}
```

Key functions:
```rust
pub(crate) fn parse_sse_event(raw_data: &str, session_id: &str) -> Option<OpenCodeEvent>
pub(crate) fn subscribe_sse(client, base_url, session_id) -> (Receiver<OpenCodeEvent>, CancellationToken, JoinHandle<()>)
pub(crate) fn drain_sse_into_status(event, status, thinking_buf, active_tool) -> bool  // true = done
```

`parse_sse_event` filters by `sessionID` in properties. Maps:
- `message.part.delta` + `field="text"` → `TextDelta`
- `message.part.delta` + `field="thinking"|"reasoning"` → `ThinkingDelta`
- `message.part.updated` + `type="tool-invocation"` + `state="running"` → `ToolStart`
- `message.part.updated` + `state="result"|"error"` → `ToolEnd`
- `session.status` + `status="idle"` + matching sessionID → `SessionIdle`
- `server.heartbeat` (no filter) → `Heartbeat`
- `server.connected` (no filter) → `Connected`

`subscribe_sse`: `tokio::spawn(SseReader.run())` with reconnect loop (500ms → 30s backoff). Returns `(mpsc::Receiver<OpenCodeEvent>, CancellationToken, JoinHandle<()>)`.

SSE byte parsing: manual line-based parsing of `bytes_stream()`. No new crate needed.
- Each chunk: split on `\n`, accumulate lines, emit on `data: {json}` lines.
- SSE comment/id/event lines: skip.

`drain_sse_into_status`: bridges `OpenCodeEvent` → `StatusBuilder`. Accumulates thinking in a local buffer, flushes on `ToolStart` or `SessionIdle`. Returns `true` on `SessionIdle`.

Inactivity timer: main task holds `tokio::sync::watch::Sender<Instant>`. Status task sends `Instant::now()` on each `Heartbeat`. Main selects on 30s since last heartbeat.

Tests: all `parse_sse_event` variants (unit, pure), SSE reader cancel, reconnect on disconnect, session-ID filter (async, wiremock).

**Validate:** `cargo test --lib -- opencode::events` passes.

---

### Step 5 — `process.rs` (depends on Step 3)

```rust
pub struct OpenCodeProcessManager {
    state: tokio::sync::Mutex<Option<OpenCodeProcess>>,
    port: u16,
    hostname: String,
    config_path: PathBuf,
}

static OPENCODE_PROCESS: OnceLock<Arc<OpenCodeProcessManager>> = OnceLock::new();
pub fn init_opencode_process(port, hostname, config_path) { ... }
pub fn opencode_process() -> Option<Arc<OpenCodeProcessManager>> { ... }
```

Key methods:
- `find_opencode_binary() -> Result<PathBuf>` — which/XDG/NVM/bun paths
- `spawn_opencode() -> Result<OpenCodeProcess>` — `tokio::process::Command`, env_clear + PATH/HOME, stderr piped to `tracing::info!`
- `wait_for_ready() -> Result<()>` — poll GET /path every 500ms up to 30s
- `pub async fn ensure_running() -> Result<()>` — try_wait → respawn if dead
- `pub async fn shutdown()` — POST /instance/dispose, wait 5s, kill
- `pub async fn cleanup_orphans()` — `pkill -f "opencode serve"`

**No** `env_clear()` actually — OpenCode needs bun/node runtime in PATH. Pass through PATH, HOME, LANG. Set `OPENCODE_CONFIG_DIR` to `workspace_dir/opencode/` so OpenCode picks up our `opencode.json`.

Tests: wait_for_ready timeout (wiremock), wait_for_ready success (wiremock), shutdown noop when not running.

**Validate:** `cargo test --lib -- opencode::process` passes.

---

### Step 6 — Copy status.rs + telegram.rs

```bash
cp src/pi/status.rs src/opencode/status.rs
cp src/pi/telegram.rs src/opencode/telegram.rs
```

Update module paths in both files (`use crate::opencode::` where needed, but these files are self-contained).

**Validate:** `cargo build` compiles.

---

### Step 7 — `OpenCodeManager` in `mod.rs` (depends on Steps 2–6)

```rust
pub struct OpenCodeManager {
    port: u16,
    workspace_dir: PathBuf,
    provider: String,
    model: String,
    session_store: Arc<RwLock<OpenCodeSessionStore>>,
    http_client: Arc<OpenCodeClient>,
    active_sse: tokio::sync::Mutex<HashMap<String, tokio::task::AbortHandle>>,
    idle_timeout: Duration,
    history_inject_limit: usize,
    history_inject_max_chars: usize,
}

static OC_MANAGER: OnceLock<Arc<OpenCodeManager>> = OnceLock::new();
pub fn init_oc_manager(config: &OpenCodeConfig, api_key: &str, workspace_dir: &Path) { ... }
pub fn oc_manager() -> Option<Arc<OpenCodeManager>> { OC_MANAGER.get().cloned() }
```

Methods (see spec for full semantics):

**`ensure_session(history_key) -> Result<String>`**
Lock order: read → release → HTTP → write. TOCTOU guard on write. Verifies existing session with `get_session` (404 triggers recreation).

**`prompt(history_key, text, history, on_event) -> Result<String>`**
1. `ensure_session`
2. If `needs_history_injection`: `inject_history` (failure logged, not propagated)
3. `subscribe_sse` → `(rx, cancel_token, sse_handle)`
4. Spawn status-update task: reads rx, calls `drain_sse_into_status`, throttles Telegram edits to every 2s
5. `send_message` HTTP await
6. `cancel_token.cancel()` → status task stops
7. Update `last_active` in store
8. Return text from HTTP response

On connection error: `ensure_server_running()` then retry once.

**`prompt_async(history_key, text) -> Result<()>`**
`ensure_session` → `send_message_async`. Non-204 → surface error.

**`abort(history_key) -> Result<bool>`**
Abort active SSE handle (if any) → `abort_session` HTTP call. Return bool from HTTP or false if no session.

**`inject_history(history_key, messages) -> Result<()>`**
Format using `format_history_for_injection` (copy from `src/pi/mod.rs`). `send_message_no_reply`. Set `history_injected = true`.

**`stop(history_key) -> Result<()>`**
Abort SSE → `delete_session` HTTP → remove from store.

**`kill_idle(max_idle)`**
Collect idle keys (read lock), stop each. Skip keys with active SSE handles.

Tests: mirror `PiManager` tests (kill_idle, format_history variants). Also: ensure_session creates then reuses, prompt error → respawn retry.

**Validate:** `cargo test --lib -- opencode::` all pass.

---

### Step 8 — Wire into daemon (depends on Step 7)

In `src/daemon/mod.rs` (or `src/main.rs`, wherever `init_pi_manager` is called):

```rust
if config.opencode.enabled {
    let config_path = crate::opencode::config::write_opencode_config(
        &config.opencode, &opencode_api_key, &workspace_dir,
    ).await.context("write opencode.json")?;

    crate::opencode::process::init_opencode_process(
        config.opencode.port, &config.opencode.hostname, config_path,
    );
    crate::opencode::process::cleanup_orphans().await;

    if let Some(pm) = crate::opencode::process::opencode_process() {
        pm.ensure_running().await.context("start opencode server")?;
    }

    crate::opencode::init_oc_manager(&config.opencode, &opencode_api_key, &workspace_dir);

    // Idle cleanup loop (same as Pi's idle loop in main.rs)
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(300)).await;
            if let Some(mgr) = crate::opencode::oc_manager() {
                mgr.kill_idle(Duration::from_secs(1800)).await;
            }
        }
    });
}
```

**Validate:** `./dev/restart-daemon.sh` with `[opencode] enabled = false` — daemon starts normally. With `enabled = true` — daemon starts OpenCode server, logs show "opencode server ready".

---

### Step 9 — `channels/mod.rs` changes (depends on Step 7)

#### 9a: New commands

Extend `ChannelRuntimeCommand` enum (near line 468):
```rust
PiSteer(Option<String>),   // /ps [text]
PiFollowup(String),        // /pf <text>
```

Extend `parse_runtime_command` match (near line 1338):
```rust
"/ps" => Some(ChannelRuntimeCommand::PiSteer(rest.trim().to_string().into_some_if_nonempty())),
"/pf" => { let t = rest.trim().to_string(); if t.is_empty() { None } else { Some(ChannelRuntimeCommand::PiFollowup(t)) } }
```

Dispatch in `handle_runtime_command_if_needed`:
```rust
ChannelRuntimeCommand::PiSteer(text) => handle_ps_command(ctx, &sender_key, text),
ChannelRuntimeCommand::PiFollowup(text) => handle_pf_command(ctx, &sender_key, text),
```

Add helpers near `handle_models_command`:
- `handle_ps_command`: spawn abort + optional prompt, return "Aborting…"
- `handle_pf_command`: spawn `prompt_async`, return "Queued"

#### 9b: Message routing gate (near line 3136)

```rust
if ctx.config.opencode.enabled {
    if handle_oc_bypass_if_needed(ctx.as_ref(), &msg, target_channel.as_ref()).await { return; }
} else {
    if handle_pi_bypass_if_needed(ctx.as_ref(), &msg, target_channel.as_ref()).await { return; }
}
```

#### 9c: `handle_oc_bypass_if_needed`

Near-copy of `handle_pi_bypass_if_needed` (lines 1053–1317). Replace Pi-specific calls with OC equivalents. Same `/ps` stop detection, same `pi_mode` flag, same `record_pi_turn`, same TelegramNotifier/StatusBuilder (from `opencode::` module).

Regular message while OC busy → `prompt_async` (queue automatically).

#### 9d: Update `handle_models_command`

When `hint == "pi"` and `opencode.enabled`: spawn `oc_manager.ensure_session()` instead of `pi_manager.ensure_running()`.
When leaving pi_mode and `opencode.enabled`: spawn `oc_manager.stop()`.

Tests:
- `parse_runtime_command` for `/ps`, `/ps hello world`, `/pf text`, `/pf` (empty → None)
- `handle_ps_command` with oc_manager mock
- `handle_pf_command` with oc_manager mock

**Validate:** `cargo test --lib -- channels::` passes. Full: `./dev/ci.sh all`.

---

### Step 10 — E2E validation

```bash
# Start daemon with opencode enabled
[opencode]
enabled = true
port = 14096
api_key_profile = "minimax:pi-fresh-4"
provider = "minimax"
model = "MiniMax-M2.7-highspeed"
base_url = "https://api.minimax.chat/v1"

./dev/restart-daemon.sh

# Telegram E2E sequence:
# 1. Send /models pi → "Pi mode activated"
# 2. Send "напиши функцию на Python для сортировки" → response from OpenCode
# 3. Send /ps → "Aborting..."
# 4. Send long task then /pf "another task" → both complete in order
# 5. Restart daemon mid-conversation → session resumes (SQLite persistence)
```

Adapt coder E2E tests (c1–c7) to run against OpenCode backend:
```bash
cd ~/.zeroclaw/workspace/skills/coder
python3 -m pytest tests/test_e2e.py -v -s
```

---

## Cargo.toml Changes

**No new dependencies required.** All needed crates already present:
- `tokio` (process, sync, time)
- `reqwest` (json, stream, rustls)
- `serde` + `serde_json`
- `anyhow`
- `tracing`
- `tokio-util` (CancellationToken)
- `tokio-stream` (ReceiverStream)
- `futures-util` (StreamExt)
- `wiremock` (dev-dependency)
- `tempfile` (dev-dependency)

---

## Removal (after E2E passes)

1. Remove `src/pi/rpc.rs` and `src/pi/mod.rs` (PiManager, PiInstance, spawn_pi)
2. Keep `src/pi/session.rs`, `src/pi/status.rs`, `src/pi/telegram.rs` until users confirm no regressions
3. Remove `[pi]` config section after one release cycle
4. Remove feature flag check in channels/mod.rs

---

## Validation Checklist

After each step:
- `cargo fmt --all -- --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test --lib`

After Step 9:
- `./dev/ci.sh all`
- Coder E2E c1–c7: `python3 -m pytest tests/test_e2e.py -v -s`
- Manual Telegram: `/ps`, `/pf`, abort, queue, daemon restart
