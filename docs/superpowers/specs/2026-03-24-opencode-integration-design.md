# OpenCode Integration Design

**Date:** 2026-03-24
**Status:** Draft
**Replaces:** `src/pi/` module (stdin/stdout RPC with Claude Code)

---

## Problem

The current Pi integration (`src/pi/`) communicates with Claude Code via stdin/stdout JSONL. This causes:

- `timeout waiting for agent_end` — 300s total deadline kills long active sessions
- `timeout waiting for prompt ACK` — Pi process hangs, stdin unresponsive
- `stream ended without agent_end` — Pi crashes mid-generation
- `Agent is already processing` — second message causes hard error, instance killed
- `Pi bypass failed: no Pi instance` — race between inject_history and prompt
- UTF-8 panics in status rendering (fixed, but symptom of fragile transport)
- No reconnect — daemon restart kills all Pi sessions

---

## Solution

Replace `src/pi/` with an OpenCode-backed implementation. OpenCode runs as a persistent HTTP server; ZeroClaw connects via `reqwest` HTTP calls and SSE streaming.

**OpenCode advantages over stdin/stdout:**
- HTTP/SSE — proper transport, no byte-boundary issues
- Native message queue — second message while busy waits automatically
- Abort endpoint — clean interrupt without killing the session
- Context compaction — automatic summary when context overflows
- Session persistence — SQLite-backed, survives daemon restart
- Reconnect — re-subscribe to SSE after reconnect, pull missed history via REST

---

## Architecture

```
Telegram message
  → channels/mod.rs
      → OpenCodeManager (replaces PiManager)
          → OpenCode server (HTTP :14096)
              → MiniMax M2.7-highspeed (or configured model)
          ← SSE event stream (text deltas, tool calls, finish)
      → TelegramNotifier (unchanged — typing + status edit)
  → Telegram reply
```

### New module: `src/opencode/`

```
src/opencode/
  mod.rs          — OpenCodeManager, init, global singleton
  client.rs       — reqwest HTTP client: send_message, abort, get_session
  session.rs      — session store: history_key → opencode_session_id
  events.rs       — SSE subscriber: parse MessageEvent deltas
  process.rs      — spawn/monitor opencode server subprocess
  config.rs       — write opencode.json from ZeroClaw config
  status.rs       — reuse src/pi/status.rs (unchanged)
  telegram.rs     — reuse src/pi/telegram.rs (unchanged)
```

### Removed: `src/pi/rpc.rs`, `src/pi/mod.rs` (PiManager, PiInstance, spawn_pi, rpc_prompt, rpc_new_session, rpc_switch_session)

`src/pi/status.rs` and `src/pi/telegram.rs` move to `src/opencode/` unchanged.

---

## OpenCode Server Lifecycle

ZeroClaw manages the OpenCode server process:

1. **Startup**: ZeroClaw writes `~/.zeroclaw/opencode/opencode.json` with provider config, then spawns `opencode serve --port 14096 --hostname 127.0.0.1`.
2. **Health check**: `GET /path` — if fails, re-spawn.
3. **Shutdown**: `POST /instance/dispose` then SIGTERM.
4. **Daemon restart**: OpenCode sessions survive in SQLite. ZeroClaw reconnects to existing sessions.

The OpenCode server port is fixed at `14096` (configurable via `[opencode].port` in `config.toml`).

---

## Session Mapping

Each ZeroClaw `history_key` maps to one OpenCode session ID.

```rust
// src/opencode/session.rs
struct SessionStore {
    map: HashMap<String, String>,  // history_key → opencode_session_id
    path: PathBuf,                 // ~/.zeroclaw/opencode/sessions.json
}
```

On first message: `POST /session` → store returned `id`.
On reconnect: load from `sessions.json`, verify with `GET /session/{id}`.
On session not found (404): create new session, update store.

---

## Message Flow

### Normal message (Pi idle)

```
1. ensure_session(history_key) → opencode_session_id
2. inject_history if needed (POST /session/{id}/message with noReply=true)
   - on failure: delete session, retry once; if still fails, proceed without context and warn
3. Subscribe SSE FIRST: GET /event (filter events by sessionID client-side)
4. POST /session/{id}/message → blocks until HTTP response (final message)
5. SSE reader task (tokio::spawn) collects message.part.delta events → update Telegram status
6. HTTP response returns completed message → cancel SSE reader, send final Telegram reply
```

**SSE subscription must start before POST /session/{id}/message** to avoid missing events.
The SSE stream is global (all sessions); filter by `properties.sessionID` in client.
Confirmed delta format: `{ type: "message.part.delta", properties: { sessionID, messageID, partID, field: "text", delta: "..." } }`

### Second message while Pi busy (`/pf` or regular)

```
POST /session/{id}/prompt_async  → 204 immediately (queued by OpenCode natively)
```

OpenCode's internal callback queue handles sequencing. No ZeroClaw-side queue needed.
If `prompt_async` returns non-204: surface error to user ("Pi busy, try again"). No retry — best-effort delivery.

### Abort and steer (`/ps`)

```
1. POST /session/{id}/abort      → cancels current generation
2. POST /session/{id}/message    → send new message immediately
```

### Command routing in Telegram

| User sends | Action |
|-----------|--------|
| `/ps <text>` | abort current + send `<text>` as new message |
| `/pf <text>` | `prompt_async` with `<text>` |
| regular text (Pi busy) | `prompt_async` (queued) |
| regular text (Pi idle) | `POST /session/{id}/message` |

---

## SSE Event Handling

Subscribe to `GET /event` before sending each message. Filter events by `sessionID`.

Relevant event types:

| Event type | Action |
|-----------|--------|
| `message.part.delta` with `field: "text"` | append to text buffer, update status |
| `message.part.updated` with `type: "tool-invocation"` | update status with tool name |
| `message.updated` with completed role | extract final text |
| `server.heartbeat` | reset inactivity timer |
| `server.connected` | confirm subscription active |

On SSE disconnect: re-subscribe, pull message history via `GET /session/{id}/message` to reconstruct state.

---

## History Injection

When a new OpenCode session is created for an existing ZeroClaw conversation, inject the last N messages so OpenCode has context:

```
POST /session/{id}/message
{
  "parts": [{ "type": "text", "text": "<injected history>" }],
  "noReply": true
}
```

This replaces the current `inject_history` → `rpc_prompt` path.

Maximum injection: 50 messages or 50k chars (configurable).

---

## Provider Configuration

OpenCode config written to `~/.zeroclaw/opencode/opencode.json`:

```json
{
  "server": { "port": 14096, "hostname": "127.0.0.1" },
  "provider": {
    "minimax": {
      "npm": "@ai-sdk/openai-compatible",
      "name": "MiniMax",
      "options": {
        "apiKey": "<from [opencode].api_key in config.toml>",
        "baseURL": "https://api.minimax.chat/v1"
      },
      "models": {
        "MiniMax-M2.7-highspeed": {},
        "MiniMax-M1": {}
      }
    }
  },
  "model": "minimax/MiniMax-M2.7-highspeed",
  "compaction": { "auto": true }
}
```

ZeroClaw config additions (`config.toml`):

```toml
[opencode]
port = 14096
api_key_profile = "minimax:pi-fresh-4"
provider = "minimax"
model = "MiniMax-M2.7-highspeed"
base_url = "https://api.minimax.chat/v1"
history_inject_limit = 50
history_inject_max_chars = 50000
```

---

## Commands: `/ps` and `/pf`

Parsed in `channels/mod.rs` alongside existing `/models`, `/reset`:

```
/ps [text]   — abort + optional new message (steer)
/pf <text>   — followup (queued behind current response)
```

If Pi not active, `/ps` and `/pf` behave like a normal message.

---

## Inactivity / Idle Management

OpenCode manages session lifetime natively. ZeroClaw kills idle OpenCode sessions after `opencode.idle_timeout_secs` (default: 1800s = 30min) by calling `DELETE /session/{id}`.

The `kill_idle` loop from `src/pi/mod.rs` is preserved but calls OpenCode's delete endpoint instead of killing a process.

---

## Error Handling

| Error | Recovery |
|-------|----------|
| OpenCode server not responding (health check fails) | re-spawn, poll `/path` every 500ms up to 30s, then retry message |
| OpenCode process crash mid-response | detected via reqwest connection error; re-spawn, session survives in SQLite, user sees "⚠️ Pi restarted" |
| Session 404 | delete from session store, create new session, re-inject history (one retry) |
| History injection fails | proceed without context, log WARN; do not block user message |
| Message error (provider fail) | surface error to user via Telegram |
| SSE disconnect | re-subscribe; pull missed history via `GET /session/{id}/message` (returns JSON array) |
| Abort while idle | no-op (OpenCode returns false, ignored) |
| Idle cleanup race (DELETE while message in-flight) | per-session write lock; idle checker skips sessions with active SSE reader |
| sessions.json corrupted | log ERROR, start with empty map (sessions re-created on next message) |

---

## Migration Plan

1. Add `[opencode]` config section — daemon reads both `[pi]` and `[opencode]` temporarily.
2. Implement `src/opencode/` module in parallel with existing `src/pi/`.
3. Feature flag: `opencode.enabled = true` switches routing in `channels/mod.rs`.
4. Remove `src/pi/rpc.rs`, `src/pi/mod.rs` after E2E tests pass.
5. Keep `src/pi/status.rs` and `src/pi/telegram.rs` (moved/reused).

---

## Rust Implementation Notes

### Async task structure per prompt

```
tokio::spawn(sse_reader)  ← subscribes GET /event, filters sessionID, sends deltas to channel
    ↕ mpsc channel
main_task: POST /session/{id}/message → awaits HTTP response
    → on response: abort sse_reader task, send final Telegram reply
    → on reqwest error: re-spawn OpenCode, retry once
```

### Error type

```rust
enum OpenCodeError {
    Http(reqwest::Error),
    ServerError(u16, String),  // status, body
    NoSession(String),
    SseTimeout,
    SpawnFailed(std::io::Error),
    ProviderError(String),
}
```

### Lock ordering (deadlock prevention)

1. `sessions_lock` (RwLock) — session store reads/writes
2. `active_sse_lock` (per-session Mutex) — marks session as having active SSE reader
3. Never hold `sessions_lock` while awaiting HTTP

### Known limitations

- Config changes (API key rotation) require daemon restart
- No cascade/fallback across providers (planned)
- prompt_async is best-effort (no durable queue)
- SSE events missed during disconnect recovered via REST history only (no replay buffer unlike Goose)

## Testing

- Unit: `session.rs` store load/save, `client.rs` mock HTTP responses
- Integration: start real OpenCode server, test abort/queue/reconnect
- E2E: existing coder E2E tests (c1–c7) adapted for OpenCode backend
- New E2E: `/ps` abort test, `/pf` queue test, daemon-restart reconnect test

---

## Out of Scope

- Cascade/fallback across multiple providers (implement later if rate limits become an issue; current ZeroClaw `ReliableProvider` can serve as a proxy)
- OpenCode authentication (`OPENCODE_SERVER_PASSWORD`) — loopback only, no auth needed
- Migrating existing Pi JSONL sessions to OpenCode format
