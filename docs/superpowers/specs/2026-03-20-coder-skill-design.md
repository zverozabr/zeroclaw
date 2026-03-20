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

`thread_id` is **not** a tool parameter. It is read from the `ZC_THREAD_ID` environment variable injected by ZeroClaw (see §ZeroClaw Rust Changes below).

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
- `set_system_prompt(text)` → sends `{"type": "set_system_prompt", "prompt": "..."}` — called once after `new_session()` to inject log access context; **not** called on `switch_session` (system prompt is already saved in the session JSONL)
- `prompt(message)` → sends `{"type": "prompt", "text": "..."}`, reads events until `agent_end` or timeout; emits structured progress events for each Pi action
- `set_model(provider, model_id)` → sends `{"type": "set_model", "provider": "...", "modelId": "..."}` — used by `/model` command handler in `coder.py`
- Auto-cancels `extension_ui_request` events: responds `{"type": "extension_ui_response", "id": "...", "cancelled": true}` immediately — Pi is headless in this context
- Retry on broken pipe: calls `pi_manager.restart()` then re-issues `switch_session` before retrying the failed `prompt`

**Timeout:** 300 seconds hard abort. No soft heartbeat — progress is shown via live status message editing (see §Progress via Status Message).

**Concurrency / file locking:** `sessions.json` is read-modify-written under `fcntl.flock(LOCK_EX)` to prevent races when two Telegram messages arrive simultaneously for different threads.

### 4. `coder.py` — Entrypoint

1. Read `thread_id = os.environ["ZC_THREAD_ID"]` (see §ZeroClaw Rust Changes)
2. Parse `message` from ZeroClaw skill args
3. Handle `/reset` command: delete `thread_id` mapping from `sessions.json`, reply "Session reset."
4. Handle `/model <provider> <model>` command: call `rpc_client.set_model(provider, model)`, reply "Switched to `provider/model`."
5. Call `pi_manager.ensure_running()`
6. Lock `sessions.json` (`fcntl.flock`), look up `thread_id`
7. If found and `session_file` exists: `rpc_client.switch_session(session_file)`
   If found but `session_file` missing (deleted externally): log warning, fall through to create new
   If not found: `rpc_client.new_session()` → `rpc_client.set_system_prompt(LOG_ACCESS_PROMPT)` → save `session_file`, update mapping
8. Release lock
9. Post initial status message via gateway API → receive `status_msg_id`
10. `rpc_client.prompt(message)` → for each Pi event: edit status message (see §Progress via Status Message)
11. On `agent_end`: edit status message to final response text, return to ZeroClaw

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

## Progress via Status Message

Same pattern as ZeroClaw's tool-call progress display: send one status message, edit it in place as Pi works.

### Mechanism

`coder.py` uses the ZeroClaw gateway API (available as `ZEROCLAW_GATEWAY_URL` + `ZEROCLAW_GATEWAY_TOKEN` in env):

```
POST  {GATEWAY_URL}/v1/messages          → send initial status, returns msg_id
PATCH {GATEWAY_URL}/v1/messages/{msg_id} → edit in place
```

### Status message lifecycle

```
[send]   ⚙ Pi is working...

[edit]   ⚙ reading src/channels/mod.rs

[edit]   ⚙ running: cargo fmt --check

[edit]   ⚙ writing src/channels/mod.rs

[edit]   ⚙ running: cargo clippy

[edit → final answer]
Fixed the formatting issue in channels/mod.rs:
- line 42: removed trailing whitespace
- ...
```

### Pi event → status text mapping

| Pi RPC event | Status text shown |
|---|---|
| `tool_call` with `read_file` | `⚙ reading {path}` |
| `tool_call` with `write_file` | `⚙ writing {path}` |
| `tool_call` with `edit_file` | `⚙ editing {path}` |
| `tool_call` with `run_command` / `bash` | `⚙ running: {command[:60]}` |
| `tool_call` with `search` / `glob` | `⚙ searching {pattern}` |
| `tool_call` (other) | `⚙ {tool_name}` |
| `thinking` / `assistant_message` chunk | `⚙ Pi is thinking…` |
| `agent_end` | replace with final answer text |
| timeout (300s) | `✗ Pi timed out after 300s` |
| error | `✗ Error: {message}` |

### Rate limiting

Telegram allows ~1 edit/second per message. `rpc_client` debounces edits: coalesce rapid tool events, emit at most 1 edit/second. On `agent_end`, always emit final edit regardless of debounce.

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
| Pi session JSONL missing | Log warning, create new session via `new_session()`, replace stale `sessions.json` entry with new `session_file`, inform user: "Previous context unavailable, starting fresh." |
| `extension_ui_request` event | Auto-cancelled via `extension_ui_response(cancelled=true)` |
| RPC heartbeat (>60s no end) | Send "Pi is still working…" to Telegram |
| RPC hard timeout (>300s) | Return error, leave Pi running for next request |
| Unknown thread_id | Create new session automatically |
| `MINIMAX_API_KEY` missing | Skill returns: "MiniMax key not in environment. Add to shell_env_passthrough." |
| Pi binary missing | Skill returns: `npm install -g @badlogic/pi@<pinned-version>` |

---

## ZeroClaw Rust Changes

The skill requires `ZC_THREAD_ID` to be injected into skill subprocess env. This env var does not currently exist. Required changes:

**`src/agent/loop_.rs`** — add a new task-local alongside `TOOL_LOOP_REPLY_TO_MESSAGE_ID`:

```rust
tokio::task_local! {
    pub(crate) static TOOL_LOOP_THREAD_ID: Option<String>;
}

pub(crate) async fn scope_thread_id<F>(thread_id: Option<String>, future: F) -> F::Output
where F: Future
{
    TOOL_LOOP_THREAD_ID.scope(thread_id, future).await
}
```

**`src/channels/mod.rs`** — wrap agent invocation with `scope_thread_id`, passing the stable per-thread identifier: `interruption_scope_id.or(thread_ts).or(Some(msg.id.clone()))`.

**`src/skills/tool_handler.rs`** — inject into subprocess env alongside `ZC_REPLY_TO_MESSAGE_ID`:

```rust
if let Ok(Some(thread_id)) = crate::agent::loop_::TOOL_LOOP_THREAD_ID.try_with(|v| v.clone()) {
    cmd.env("ZC_THREAD_ID", thread_id);
}
```

These changes are **prerequisites** for the skill to function. They must be implemented as part of this feature, not as a follow-up.

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
- Status message appears immediately and updates live as Pi reads/writes/runs commands
- `/reset` clears session for that thread only
- `/model gemini gemini-2.0-flash` switches model mid-session
- Model is MiniMax by default

---

## E2E Test Plan (Live Telegram, Real Files)

**Mandatory before declaring done.** Run via Telethon as `zverozabr` (same pattern as existing `telegram_reader_e2e` tests). All tests use the live bot `@zGsR_bot`.

### Test c1 — Basic file write

1. Create Telegram topic/thread "coder-test-c1"
2. Send: `"create a file /tmp/coder_e2e_c1.txt with the content: hello from Pi"`
3. Assert: file `/tmp/coder_e2e_c1.txt` exists with expected content
4. Assert: bot reply contains confirmation of file creation

### Test c2 — Context continuity within thread

1. Reuse thread "coder-test-c1" (same thread as c1)
2. Send: `"append a second line to that file: line 2"`
3. Assert: `/tmp/coder_e2e_c1.txt` now has 2 lines — Pi remembered the filename from c1

### Test c3 — Context isolation between threads

1. Create a **new** topic/thread "coder-test-c3"
2. Send: `"what file did we create earlier?"`
3. Assert: Pi does **not** mention `coder_e2e_c1.txt` — no context bleed from thread c1
4. Assert: bot reply says no previous files or context in this session

### Test c4 — Shell action in repo

1. Create thread "coder-test-c4"
2. Send: `"in the zeroclaws repo, run: cargo fmt --check and tell me the result"`
3. Assert: bot reply contains actual cargo output (exit code, any formatting issues)
4. Assert: Pi ran the command in `$DAEMON_CWD` (zeroclaws repo)

### Test c5 — Log reading

1. Create thread "coder-test-c5"
2. Send: `"read the last 20 lines of the ZeroClaw daemon log and summarize what happened"`
3. Assert: bot reply contains content from `/tmp/zeroclaw_daemon.log`
4. Assert: reply is a coherent summary, not raw log dump

### Test c6 — Daemon restart session recovery

1. Use thread "coder-test-c1", confirm Pi session exists (c1 + c2 ran)
2. Kill and restart Pi process manually (simulate crash): `pkill -f "pi --mode rpc"`
3. In thread "coder-test-c1", send: `"what file did we create?"`
4. Assert: Pi restored session from JSONL → still knows about `coder_e2e_c1.txt`
5. Assert: no "previous context unavailable" warning (session JSONL intact)

### Test c7 — `/reset` clears only that thread

1. In thread "coder-test-c1", send: `/reset`
2. Assert: bot replies "Session reset."
3. Send: `"what file did we create?"`
4. Assert: Pi has no memory of c1 files (fresh session)
5. Switch to thread "coder-test-c4", send: `"what did we run earlier?"`
6. Assert: Pi still remembers cargo fmt from c4 (other thread unaffected)

### Running the tests

```bash
source ~/.zeroclaw/.env && \
python3 ~/.zeroclaw/workspace/skills/coder/tests/test_e2e.py \
  --test-threads=1
```

Tests must pass with real Telegram messages before any "done" claim. No mocking of Pi or Telegram.
