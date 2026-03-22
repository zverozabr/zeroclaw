# Pi Persistent Mode — Design Spec

## Problem

Pi (coding agent) перезапускается на каждое сообщение: spawn Python → spawn Pi → load session → prompt → kill Pi → kill Python. Это:
- 4-5 секунд overhead на каждое сообщение
- Pi теряет in-memory контекст между сообщениями
- Session reload через файл не гарантирует сохранение всех деталей

## Solution

Pi процесс живёт пока Pi mode активен. Управляется из Rust (ZeroClaw daemon), без Python промежуточного слоя.

```
/models pi → spawn Pi RPC → load/create session
  → msg 1 → prompt Pi (stdin/stdout) → response
  → msg 2 → prompt Pi → response
  → 30 min idle → save session → kill Pi
  → next msg → re-spawn → load session
/models X → save session → kill Pi → LLM resumes
```

## Architecture

### New module: `src/pi/`

```
src/pi/
  mod.rs        — PiManager: spawn, prompt, kill, idle timeout
  rpc.rs        — JSONL RPC client over stdin/stdout
  session.rs    — Session persistence (pi_sessions.json)
```

### PiManager (src/pi/mod.rs)

Manages one Pi process per chat (keyed by `history_key`).

```rust
pub struct PiManager {
    instances: Mutex<HashMap<String, PiInstance>>,
}

struct PiInstance {
    process: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    stdout: BufReader<tokio::process::ChildStdout>,
    session_file: Option<String>,
    last_active: Instant,
    history_key: String,
}
```

**Public API:**
- `ensure_running(history_key, workspace_dir) → Result<()>` — spawn if not running, load session
- `prompt(history_key, message, on_status: Callback) → Result<String>` — send prompt, stream events, return response text
- `stop(history_key) → Result<()>` — save session, kill Pi
- `stop_all()` — shutdown hook
- `inject_history(history_key, messages: &[ChatMessage], token_limit: usize)` — send ZeroClaw history as Pi context (first activation only)

**Background task:** `idle_reaper()` — runs every 60s, kills Pi instances idle > 30 min (saves session first).

### RPC Client (src/pi/rpc.rs)

Port of Python `rpc_client.py` to async Rust.

```rust
pub async fn send(stdin: &mut ChildStdin, msg: &serde_json::Value) -> Result<()>
pub async fn recv_line(stdout: &mut BufReader<ChildStdout>, timeout: Duration) -> Option<serde_json::Value>
pub async fn recv_response(stdout, command: &str, timeout: Duration) -> Option<serde_json::Value>

pub async fn rpc_prompt(
    stdin: &mut ChildStdin,
    stdout: &mut BufReader<ChildStdout>,
    message: &str,
    on_event: impl FnMut(PiEvent),
    timeout: Duration,
) -> Result<String>
```

**Pi Events:**
```rust
pub enum PiEvent {
    ThinkingStart,
    ThinkingDelta(String),
    ThinkingEnd,
    ToolStart { name: String, args: serde_json::Value },
    ToolEnd { name: String, output: String },
    TextDelta(String),
    AgentEnd { text: String },
}
```

### Session Persistence (src/pi/session.rs)

```rust
pub struct PiSessionStore {
    path: PathBuf, // ~/.zeroclaw/workspace/pi_sessions.json
}

// JSON format:
// { "history_key": { "session_file": "...", "created_at": "...", "last_active": "..." } }

pub fn load_session(history_key: &str) -> Option<String>  // returns session_file path
pub fn save_session(history_key: &str, session_file: &str)
pub fn delete_session(history_key: &str)
```

### Integration: channels/mod.rs

**`handle_pi_bypass_if_needed`** simplifies to:

```rust
async fn handle_pi_bypass_if_needed(ctx, msg, channel) -> bool {
    let history_key = conversation_history_key(msg);
    let is_pi_mode = get_route_selection(ctx, &history_key).pi_mode;

    if !is_pi_mode { return false; }

    // Ensure Pi is running for this chat
    pi_manager.ensure_running(&history_key, &ctx.workspace_dir)?;

    // First activation: inject ZeroClaw history
    if pi_manager.needs_history_injection(&history_key) {
        let history = get_sender_history(ctx, &history_key);
        pi_manager.inject_history(&history_key, &history, 100_000);
    }

    // Send prompt, stream status to Telegram
    let response = pi_manager.prompt(&history_key, &msg.content, |event| {
        match event {
            PiEvent::ToolStart { name, .. } => edit_telegram_status(&status_msg_id, &tool_status(name)),
            PiEvent::ThinkingStart => edit_telegram_status(&status_msg_id, "⚙ Pi is thinking…"),
            PiEvent::AgentEnd { text } => edit_telegram_status(&status_msg_id, &text),
            _ => {}
        }
    }).await?;

    record_pi_turn(ctx, &history_key, &msg.content, &response);
    true
}
```

### Activation: `/models pi`

In `handle_models_command` (src/channels/mod.rs):

```rust
if hint == "pi" {
    // Set pi_mode = true in route overrides
    current.pi_mode = true;
    set_route_selection(ctx, sender_key, current);
    // Spawn Pi
    pi_manager.ensure_running(sender_key, &ctx.workspace_dir);
    return "✅ Pi mode activated. All messages go to Pi.".to_string();
}

// Any other model switch while pi_mode is on → deactivate Pi
if current.pi_mode {
    pi_manager.stop(sender_key);
    current.pi_mode = false;
}
```

### Telegram Status Updates

Pi events → HTTP calls to Telegram Bot API from Rust:
- Create status message: POST `sendMessage`
- Update status: POST `editMessageText` (edit-in-place)
- Uses `TELEGRAM_BOT_TOKEN` from config

```rust
async fn edit_telegram_status(bot_token: &str, chat_id: &str, msg_id: i64, text: &str)
```

Status shows **full process** — thinking content + tool actions + response:

```
⚙ Starting Pi…

💭 Пользователь хочет прочитать файл. Использую read tool...

📖 reading /tmp/secret.txt
📄 апельсин

💭 Файл содержит слово "апельсин"...

В файле написано: апельсин
```

One Telegram message, updated via edit-in-place. Shows:
- 💭 Thinking content (first 200 chars per thinking block)
- 📖/🔧/✏️ Tool calls with args
- 📄 Tool output preview (first 150 chars)
- Final response text at the bottom

Uses `StatusBuilder` pattern (accumulate sections, render, truncate to 3800 chars keeping tail).

### History Injection

On first `/models pi` activation, inject ZeroClaw conversation history:

1. Read `conversation_histories[history_key]` (last N messages, ~100k tokens)
2. Format as single prompt: `"[Context from previous conversation]\nUser: ...\nAssistant: ...\n..."`
3. Send to Pi via `rpc_prompt(stdin, stdout, context_prompt)`
4. Mark as injected (don't re-inject on re-spawn after idle timeout)

Token counting: estimate 4 chars/token, take last `100_000 * 4 = 400_000` chars from history.

### Idle Timeout

Background tokio task in ZeroClaw daemon:
```rust
async fn pi_idle_reaper(manager: Arc<PiManager>) {
    loop {
        tokio::time::sleep(Duration::from_secs(60)).await;
        manager.kill_idle(Duration::from_secs(30 * 60));
    }
}
```

`kill_idle` iterates all instances, saves session + kills those with `last_active + 30min < now`.
Re-spawn on next message is transparent (session loaded from file).

### Pi Spawn Command

```bash
pi --mode rpc \
  --provider minimax --model MiniMax-M2.7-highspeed \
  --thinking high \
  --cwd ~/work/erp/zeroclaws
```

Environment:
- `MINIMAX_API_KEY` — JWT token from config fallback_api_keys

## Data Flow

| Data | Direction | Mechanism |
|------|-----------|-----------|
| User message | ZeroClaw → Pi | JSONL stdin: `{"type":"prompt","message":"..."}` |
| Pi events | Pi → ZeroClaw | JSONL stdout: thinking/tool/text events |
| Status updates | ZeroClaw → Telegram | HTTP Bot API editMessageText |
| Final response | ZeroClaw → Telegram | HTTP Bot API editMessageText (replace status) |
| Pi response | → ZeroClaw history | `record_pi_turn()` with `[Pi]` prefix |
| Session file | Pi ↔ disk | `~/.pi/agent/sessions/` JSONL files |
| Session index | ZeroClaw ↔ disk | `~/.zeroclaw/workspace/pi_sessions.json` |

## Files to Create/Modify

| File | Action |
|------|--------|
| `src/pi/mod.rs` | **CREATE** — PiManager, PiInstance, ensure_running, prompt, stop |
| `src/pi/rpc.rs` | **CREATE** — JSONL RPC client, send/recv, rpc_prompt |
| `src/pi/session.rs` | **CREATE** — PiSessionStore, load/save/delete |
| `src/lib.rs` | **MODIFY** — add `pub mod pi;` |
| `src/channels/mod.rs` | **MODIFY** — simplify handle_pi_bypass, add /models pi, remove spawn_coder_subprocess |
| `src/daemon/mod.rs` | **MODIFY** — start idle_reaper background task |

## Testing

### Unit tests
- `rpc::send/recv_line` — JSONL serialization
- `session::load/save/delete` — file I/O
- `PiManager::ensure_running` — spawn + PID tracking
- `PiManager::kill_idle` — timeout logic

### Integration tests
- Spawn real Pi, send prompt, get response
- Session save → kill → re-spawn → load → context preserved

### E2E Telegram tests
1. `/models pi` → Pi mode activated, Pi responds
2. Send 5 messages without prefix → Pi handles all
3. Pi maintains context (file write → read back)
4. `/models minimax` → LLM takes over
5. LLM sees `[Pi]` history
6. `/models pi` again → Pi loads saved session, context preserved
7. 30 min idle → Pi killed → next message → re-spawned with session

## Non-goals (v1)
- Multiple Pi instances per user (one per chat is enough)
- Pi model switching within Pi mode
- Pi tool approval (Pi runs with full autonomy)
- coder.py compatibility (fully replaced by Rust Pi manager)
