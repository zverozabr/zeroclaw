# Coder Skill Design — ZeroClaw + Pi

**Date:** 2026-03-20
**Status:** Draft v2 (post spec-review)
**Scope:** New ZeroClaw skill `coder` backed by [Pi](https://pi.dev/) (badlogic/pi-mono)

---

## Problem

ZeroClaw is a general-purpose agent runtime — not a coding assistant. It cannot:
- Read and edit files autonomously
- Understand a codebase with repo-map / tree-sitter
- Maintain per-task coding context across messages
- Run shell commands, apply diffs, fix lint errors iteratively

The goal is to add a dedicated coding assistant accessible from Telegram that has:
- Full file read/write/edit/shell access to the repo
- Per-Telegram-thread session continuity
- Access to ZeroClaw daemon logs for debugging
- Multi-model support (default: MiniMax)

---

## Solution: `coder` Skill Backed by Pi

[Pi](https://github.com/badlogic/pi-mono) is a MIT-licensed terminal coding agent with:
- Multi-provider support (15+ providers including MiniMax, Gemini, OpenAI)
- Tree-structured sessions stored as JSONL files under `~/.pi/agent/sessions/`
- Auto-compaction of context
- Four operational modes: interactive TUI, print, JSON, **RPC** (stdin/stdout JSONL)
- Extensions/skills framework in TypeScript
- Real-world embedding example: clawdbot

Pi is wrapped as a ZeroClaw skill. ZeroClaw routes coding requests to the skill, which manages a Pi process and maps Telegram threads to Pi sessions.

---

## Architecture

### Directory Layout

```
~/.zeroclaw/workspace/skills/coder/
├── SKILL.toml              # ZeroClaw skill definition
├── scripts/
│   ├── coder.py            # entrypoint called by ZeroClaw per message
│   ├── pi_manager.py       # Pi process lifecycle (start/stop/health)
│   └── rpc_client.py       # Pi RPC protocol client (stdin/stdout JSONL)
└── data/
    ├── sessions.json        # thread_id → pi_session_file mapping
    └── pi.pid              # Pi process PID + cmdline for verification
```

### Data Flow

```
User → Telegram thread #42: "fix bug in channels/mod.rs"
  → ZeroClaw routes to `coder` skill
    → coder.py invoked with {message, thread_id}
      → pi_manager.py: is Pi alive?
          check PID file + /proc/<pid>/cmdline contains "pi"
          no → spawn: pi --mode rpc --no-session
                       --provider minimax --model MiniMax-M2.7-highspeed
                       --cwd $DAEMON_CWD
      → sessions.json: lookup thread_id=#42
          hit  → rpc_client.switch_session(session_file)
          miss → rpc_client.new_session() → save session_file to mapping
      → rpc_client.prompt(message) → stream agent_end event
      → Pi: reads files, edits code, runs shell, auto-cancels ui dialogs
      → coder.py: collect response → return to ZeroClaw → Telegram
```

---

## Components

### 1. `SKILL.toml`

Exposes one tool: `code`

**Parameters:**
- `message` (String) — task description, bug report, or code question
- `thread_id` (String, internal) — Telegram chat/thread ID for session continuity; passed automatically by ZeroClaw routing, not user-facing

**Routing hint in description:** triggers on coding tasks — file editing, bug fixing, reading logs, writing scripts, explaining code, refactoring.

### 2. `pi_manager.py` — Process Lifecycle

Responsibilities:
- Check if Pi is running: read `data/pi.pid`, verify via `os.kill(pid, 0)` AND `/proc/<pid>/cmdline` contains `pi` (guards against PID reuse)
- Spawn Pi if dead or PID stale:
  ```
  pi --mode rpc --no-session \
     --provider minimax --model MiniMax-M2.7-highspeed \
     --cwd {DAEMON_CWD}
  ```
- Write PID to `data/pi.pid` alongside cmdline snippet for verification
- Expose `ensure_running()` → returns open subprocess handle (stdin/stdout pipes)
- Expose `restart()` for manual recovery

**Working directory:** taken from `ZEROCLAW_CWD` env var, defaulting to `$HOME/work/erp/zeroclaws`.

**Environment / credentials:** Pi reads the MiniMax key from `MINIMAX_API_KEY`. This variable must be added to `shell_env_passthrough` in `~/.zeroclaw/config.toml` before deployment (see Installation). Pi is spawned as a child of `coder.py` and inherits the filtered env.

**Note on shared state:** only one ZeroClaw daemon instance is expected in this deployment. If a second instance is started (e.g., for testing), it will share the same Pi process and `data/` directory, causing session cross-contamination. Run test instances with a separate `ZEROCLAW_HOME`.

### 3. `rpc_client.py` — Pi RPC Client

Wraps Pi's stdin/stdout JSONL RPC protocol. Key operations:

- `new_session()` → sends `{"type": "new_session"}`, reads `get_state` response, returns `session_file` (full path to `~/.pi/agent/sessions/.../timestamp_uuid.jsonl`)
- `switch_session(session_file)` → sends `{"type": "switch_session", "sessionPath": "..."}` — **always called before `prompt` when restoring an existing session after Pi restart**
- `prompt(message)` → sends `{"type": "prompt", "text": "..."}`, reads events until `agent_end` or timeout
- `set_model(provider, model_id)` → sends `{"type": "set_model", "provider": "...", "modelId": "..."}` — used by `/model` command handler in `coder.py`
- Auto-cancels `extension_ui_request` events: responds `{"type": "extension_ui_response", "id": "...", "cancelled": true}` immediately — Pi is headless in this context
- Retry on broken pipe: calls `pi_manager.restart()` then re-issues `switch_session` before retrying the failed `prompt`

**Timeout:** 300 seconds. After 60 seconds without an `agent_end`, send a "Pi is still working…" status message to Telegram (requires `coder.py` to support incremental output). Hard abort at 300s returns error without killing Pi.

**Concurrency / file locking:** `sessions.json` is read-modify-written under `fcntl.flock(LOCK_EX)` to prevent races when two Telegram messages arrive simultaneously for different threads.

### 4. `coder.py` — Entrypoint

1. Parse `{message, thread_id}` from ZeroClaw skill args
2. Handle `/reset` command: delete `thread_id` mapping from `sessions.json`, reply "Session reset."
3. Handle `/model <provider> <model>` command: call `rpc_client.set_model(provider, model)`, reply "Switched to `provider/model`."
4. Call `pi_manager.ensure_running()`
5. Lock `sessions.json`, look up `thread_id`
6. If found: `rpc_client.switch_session(session_file)`; if not: `rpc_client.new_session()` → save `session_file`
7. Release lock
8. `rpc_client.prompt(message)` → collect response (with 60s heartbeat)
9. Return response text to ZeroClaw

### 5. Log Access

Pi's system prompt is injected at `new_session()` via a `set_system_prompt` RPC call:

```
You are a coding assistant operating on the ZeroClaw codebase.

You have access to:
  - ZeroClaw daemon log: /tmp/zeroclaw_daemon.log
  - ZeroClaw config and workspace: ~/.zeroclaw/
  - Current repo: {DAEMON_CWD}

Read logs with your file tools to debug ZeroClaw behavior (errors,
channel activity, provider failures). Edit code directly in {DAEMON_CWD}.
```

Pi's file access is unrestricted — it reads `/tmp/zeroclaw_daemon.log` and anything under `~/.zeroclaw/` via its built-in file tools.

---

## Session Management

`data/sessions.json` schema (v2 — stores `session_file`, not session ID):

```json
{
  "thread_42": {
    "session_file": "/home/spex/.pi/agent/sessions/--home-spex-work-erp-zeroclaws--/1742468400_abc12345.jsonl",
    "created_at": "2026-03-20T10:00:00Z",
    "label": null
  }
}
```

Sessions persist across Pi restarts because the JSONL files remain on disk. On restart, `rpc_client.switch_session(session_file)` restores context before any new `prompt`.

Users can reset a session by sending `/reset` to the coder skill, which deletes the mapping and creates a fresh Pi session on next message.

---

## Model Configuration

Default: MiniMax (`MiniMax-M2.7-highspeed`) — consistent with ZeroClaw default.

Pi starts with `--provider minimax --model MiniMax-M2.7-highspeed`. To switch mid-session, the user sends `/model gemini gemini-2.0-flash` and `coder.py` translates this to a `set_model` RPC call. Natural language model switching ("use gemini") is **not** supported — Pi has no such tool in RPC mode.

---

## Error Handling

| Scenario | Behavior |
|---|---|
| Pi process dead | `pi_manager` respawns with `--no-session`, re-issues `switch_session`, retries |
| Pi PID stale (OS reuse) | `/proc/pid/cmdline` check catches this → respawn |
| Pi session JSONL missing | Log warning, create new session, inform user: "Previous context unavailable, starting fresh." |
| `extension_ui_request` event | Auto-cancelled via `extension_ui_response(cancelled=true)` |
| RPC heartbeat (>60s no end) | Send "Pi is still working…" to Telegram |
| RPC hard timeout (>300s) | Return error, leave Pi running for next request |
| Unknown thread_id | Create new session automatically |
| `MINIMAX_API_KEY` missing | Skill returns: "MiniMax key not in environment. Add to shell_env_passthrough." |
| Pi binary missing | Skill returns: `npm install -g @badlogic/pi@<pinned-version>` |

---

## Installation

1. **Install Pi** (version pinned after confirming compatibility with this RPC protocol design):
   ```bash
   npm install -g @badlogic/pi@1.2.x   # pin exact version after testing
   ```

2. **Add `MINIMAX_API_KEY` to ZeroClaw passthrough** in `~/.zeroclaw/config.toml`:
   ```toml
   [security]
   shell_env_passthrough = ["TELEGRAM_*", "ERPNEXT_*", "GOOGLE_API_KEY", "GROQ_API_KEY", "MINIMAX_API_KEY"]
   ```

3. **Install skill** per standard ZeroClaw skill install procedure.

4. **Verify**:
   ```bash
   pi --version
   MINIMAX_API_KEY=... pi --mode rpc --no-session --provider minimax --model MiniMax-M2.7-highspeed --cwd /tmp
   # send: {"type": "get_state"} → should return session info
   ```

---

## Out of Scope

- Multiple working directories / project switching (future: `--cwd` flag per message)
- Git operations beyond what Pi does natively (commit, push — Pi can do these via shell)
- Streaming partial responses to Telegram during Pi execution (future: edit-in-place via message editing)
- Pi extensions/TypeScript plugins (can be added later without redesign)

---

## Success Criteria

- User sends coding task in Telegram thread → Pi responds with correct file edits
- Same thread → Pi remembers previous context across messages and daemon restarts
- Different thread → isolated session, no context bleed
- Pi can read `/tmp/zeroclaw_daemon.log` and report ZeroClaw errors from it
- Tasks taking >60s send a heartbeat "still working" message
- `/reset` clears session for that thread only
- `/model gemini gemini-2.0-flash` switches model mid-session
- Model is MiniMax by default
