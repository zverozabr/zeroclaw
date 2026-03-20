# Coder Skill Design — ZeroClaw + Pi

**Date:** 2026-03-20
**Status:** Draft
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
- Tree-structured sessions with SQLite persistence
- Auto-compaction of context
- Four operational modes: interactive TUI, print, JSON, **RPC**
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
│   └── rpc_client.py       # Pi RPC protocol client
└── data/
    ├── sessions.json        # thread_id → pi_session_id mapping
    └── pi.pid              # Pi process PID
```

### Data Flow

```
User → Telegram thread #42: "fix bug in channels/mod.rs"
  → ZeroClaw routes to `coder` skill
    → coder.py invoked with {message, thread_id}
      → pi_manager.py: is Pi alive? (check PID file + process)
          no → spawn: pi --mode rpc --model minimax --cwd $DAEMON_CWD
      → sessions.json: lookup thread_id=#42
          miss → create new Pi session → save mapping
      → rpc_client.py: send message to Pi session
      → Pi: reads files, edits code, runs shell, streams response
      → coder.py: buffer stream → return to ZeroClaw → Telegram
```

---

## Components

### 1. `SKILL.toml`

Exposes one tool: `code`

**Parameters:**
- `message` (String) — task description, bug report, or code question
- `thread_id` (String) — Telegram chat/thread ID for session continuity

**Routing hint in description:** triggers on coding tasks — file editing, bug fixing, reading logs, writing scripts, explaining code, refactoring.

### 2. `pi_manager.py` — Process Lifecycle

Responsibilities:
- Check if Pi is running via `data/pi.pid` + `os.kill(pid, 0)`
- Spawn Pi if dead: `pi --mode rpc --model minimax --cwd {cwd}`
- Write PID to `data/pi.pid`
- Expose `ensure_running()` → returns RPC connection handle
- Expose `restart()` for manual recovery

Pi process inherits environment including all API keys from ZeroClaw's env.

Working directory (`--cwd`) is taken from the `ZEROCLAW_CWD` env var (set to wherever the daemon was started from), defaulting to `$HOME/work/erp/zeroclaws`.

### 3. `rpc_client.py` — Pi RPC Client

Wraps Pi's stdin/stdout RPC protocol:
- `create_session(session_id?)` → session handle
- `send_message(session_id, message)` → async generator of response chunks
- `list_sessions()` → existing session IDs
- `get_session(session_id)` → session metadata

Implements retry on broken pipe (triggers `pi_manager.restart()`).

### 4. `coder.py` — Entrypoint

1. Parse args from ZeroClaw skill invocation
2. Call `pi_manager.ensure_running()`
3. Look up `thread_id` in `data/sessions.json`
4. If missing: `rpc_client.create_session()` → persist mapping
5. `rpc_client.send_message(session_id, message)` → collect stream
6. Return response text to ZeroClaw

### 5. Log Access

Pi's system prompt (injected at session creation) includes:

```
You have access to ZeroClaw daemon logs at:
  - /tmp/zeroclaw_daemon.log   (live daemon output)
  - ~/.zeroclaw/               (config, workspace, skills)

To read logs, use your file read tool on these paths.
Use logs for debugging ZeroClaw behavior, tracing errors,
understanding channel/provider activity.
```

Pi's file access is unrestricted within the session — it can read `/tmp/zeroclaw_daemon.log` and any path under `~/.zeroclaw/` directly via its built-in file tools.

---

## Session Management

`data/sessions.json` schema:

```json
{
  "thread_42": {
    "pi_session_id": "abc123",
    "created_at": "2026-03-20T10:00:00Z",
    "label": null
  }
}
```

Sessions persist across Pi restarts (Pi loads them from its own SQLite). The JSON file is the bridge between Telegram thread IDs and Pi session IDs.

Users can reset a session by sending `/reset` to the coder skill, which deletes the mapping and creates a fresh Pi session on next message.

---

## Model Configuration

Default: MiniMax (`minimax/MiniMax-M2.7-highspeed`) — consistent with ZeroClaw default.

Pi is started with `--model minimax`. Users can switch mid-session by including `use gemini` or `use claude` in a message — Pi's model switch tool handles this within the session.

Pi reads API keys from environment (inherited from ZeroClaw daemon env where all keys are already configured).

---

## Error Handling

| Scenario | Behavior |
|---|---|
| Pi process dead | `pi_manager` respawns, retries message |
| Pi session lost | Recreate session, warn user context was reset |
| RPC timeout (>30s) | Return error message, leave Pi running |
| Unknown thread_id | Create new session automatically |
| Pi install missing | Skill returns actionable error: `npm install -g @badlogic/pi` |

---

## Installation

Pi is a Node.js package. Added to skill setup:

```bash
# In skill install script
npm install -g @badlogic/pi
```

Pi binary available as `pi` in PATH. Version pinned in skill metadata.

---

## Out of Scope

- Multiple working directories / project switching (future: `--cwd` flag per message)
- Git operations beyond what Pi does natively
- Streaming partial responses to Telegram during long Pi runs (future: Telegram edit-in-place)
- Pi extensions/TypeScript plugins (can be added later without redesign)

---

## Success Criteria

- User sends coding task in Telegram → Pi responds with correct file edits
- Same thread → Pi remembers previous context
- Different thread → isolated session, no context bleed
- Pi can read `/tmp/zeroclaw_daemon.log` and report errors from it
- Pi survives daemon restart (sessions restored from SQLite)
- Model is MiniMax by default, switchable in-session
