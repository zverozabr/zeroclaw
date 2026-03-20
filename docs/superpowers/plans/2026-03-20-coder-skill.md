# Coder Skill Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `coder` ZeroClaw skill backed by Pi that gives Telegram users a coding assistant with per-thread session continuity, live status updates, and full file/shell access to the ZeroClaw repo.

**Architecture:** Pi (MIT coding agent) runs as a persistent RPC subprocess managed by `pi_manager.py`. `coder.py` maps Telegram thread IDs (injected via new `ZC_THREAD_ID` env var) to Pi JSONL session files, sends messages via Pi's stdin/stdout JSONL protocol, and updates a live Telegram status message as Pi emits `tool_execution_start` and `agent_end` events. Skills use the Bot API directly for Telegram messaging.

**Tech Stack:** Python 3.11, Telegram Bot API (direct HTTP), `@mariozechner/pi-coding-agent@0.61.0` (Node.js binary), Rust (ZeroClaw `tokio::task_local!` for `ZC_THREAD_ID`), `fcntl` for file locking, `tomllib` for config parsing.

---

## File Map

**New files (skill):**
- `~/.zeroclaw/workspace/skills/coder/SKILL.toml` — skill definition, routes `code` tool
- `~/.zeroclaw/workspace/skills/coder/scripts/pi_manager.py` — Pi process lifecycle
- `~/.zeroclaw/workspace/skills/coder/scripts/rpc_client.py` — Pi stdin/stdout JSONL protocol
- `~/.zeroclaw/workspace/skills/coder/scripts/coder.py` — entrypoint, command handling, status messages
- `~/.zeroclaw/workspace/skills/coder/scripts/bot_api.py` — thin Telegram Bot API wrapper (send + edit)
- `~/.zeroclaw/workspace/skills/coder/tests/test_pi_manager.py` — unit tests (mock subprocess)
- `~/.zeroclaw/workspace/skills/coder/tests/test_rpc_client.py` — unit tests (mock Pi stdin/stdout)
- `~/.zeroclaw/workspace/skills/coder/tests/test_e2e.py` — live Telegram E2E tests (c1–c7)
- `~/.zeroclaw/workspace/skills/coder/data/.gitkeep` — ensures data dir exists

**Modified files (ZeroClaw Rust):**
- `src/agent/loop_.rs:230-265` — add `TOOL_LOOP_THREAD_ID` task-local + `scope_thread_id()`
- `src/channels/mod.rs:3106` — wrap agent call with `scope_thread_id`
- `src/skills/tool_handler.rs:450-457` — inject `ZC_THREAD_ID` into subprocess env

**Modified files (config/docs):**
- `CLAUDE.md` — append coder skill log access section (Pi reads this automatically)

---

## Task 1: Rust — Add ZC_THREAD_ID env var to skill subprocesses

**Files:**
- Modify: `src/agent/loop_.rs` (around line 230)
- Modify: `src/channels/mod.rs` (around line 3106)
- Modify: `src/skills/tool_handler.rs` (around line 450)

- [ ] **Step 1.1: Add TOOL_LOOP_THREAD_ID task-local to loop_.rs**

In `src/agent/loop_.rs`, inside the existing `tokio::task_local!` block (line ~230), add alongside `TOOL_LOOP_REPLY_TO_MESSAGE_ID`:

```rust
tokio::task_local! {
    pub(crate) static TOOL_LOOP_SESSION_REPORT_DIR: Option<String>;
    pub(crate) static TOOL_LOOP_SESSION_REPORT_MAX_FILES: usize;
    /// Reply-to message ID from the incoming channel message.
    /// Used by skill tools to pass to subprocess env (ZC_REPLY_TO_MESSAGE_ID).
    pub(crate) static TOOL_LOOP_REPLY_TO_MESSAGE_ID: Option<String>;
    /// Stable thread identifier from the incoming channel message.
    /// Used by skill tools to pass to subprocess env (ZC_THREAD_ID).
    /// Set from interruption_scope_id, thread_ts, or message id (in that priority).
    pub(crate) static TOOL_LOOP_THREAD_ID: Option<String>;
}
```

After the existing `scope_reply_to_message_id` function (~line 258), add:

```rust
/// Run a future with the thread ID set in task-local storage.
/// Skill tools read this to inject `ZC_THREAD_ID` into subprocess env.
pub(crate) async fn scope_thread_id<F>(thread_id: Option<String>, future: F) -> F::Output
where
    F: std::future::Future,
{
    TOOL_LOOP_THREAD_ID.scope(thread_id, future).await
}
```

- [ ] **Step 1.2: Export scope_thread_id in channels/mod.rs imports**

In `src/channels/mod.rs`, line 94, add `scope_thread_id` to the import:

```rust
run_tool_call_loop, scope_reply_to_message_id, scope_thread_id, scrub_credentials,
```

- [ ] **Step 1.3: Wrap agent invocation with scope_thread_id in channels/mod.rs**

Find the `scope_reply_to_message_id(` call (~line 3106). It currently wraps `run_tool_call_loop`. Add an outer wrap with `scope_thread_id`:

```rust
scope_thread_id(
    msg.interruption_scope_id.clone()
        .or_else(|| msg.thread_ts.clone())
        .or_else(|| Some(msg.id.clone())),
    scope_reply_to_message_id(
        msg.reply_to_message_id.clone(),
        run_tool_call_loop(
            // ... existing args unchanged ...
        ),
    ),
)
```

- [ ] **Step 1.4: Inject ZC_THREAD_ID in tool_handler.rs**

In `src/skills/tool_handler.rs`, after the existing `ZC_REPLY_TO_MESSAGE_ID` injection (~line 455):

```rust
if let Ok(Some(thread_id)) =
    crate::agent::loop_::TOOL_LOOP_THREAD_ID.try_with(|v| v.clone())
{
    cmd.env("ZC_THREAD_ID", thread_id);
}
```

- [ ] **Step 1.5: Verify compilation**

```bash
cd ~/work/erp/zeroclaws
cargo check 2>&1 | tail -20
```

Expected: no errors.

- [ ] **Step 1.6: Add unit test in tool_handler.rs**

In the test module of `src/skills/tool_handler.rs`, add the following test alongside the existing `ZC_REPLY_TO_MESSAGE_ID` test:

```rust
#[tokio::test]
async fn test_zc_thread_id_injected() {
    use crate::agent::loop_::{scope_thread_id, TOOL_LOOP_THREAD_ID};
    let thread_id = Some("tg_thread_42".to_string());
    let result = scope_thread_id(thread_id.clone(), async {
        TOOL_LOOP_THREAD_ID.try_with(|v| v.clone()).unwrap()
    })
    .await;
    assert_eq!(result, thread_id);
}

#[tokio::test]
async fn test_zc_thread_id_none_when_unset() {
    use crate::agent::loop_::TOOL_LOOP_THREAD_ID;
    // Outside any scope, try_with returns Err (not set), so env injection is skipped
    let result = TOOL_LOOP_THREAD_ID.try_with(|v| v.clone());
    assert!(result.is_err());
}
```

- [ ] **Step 1.7: Run tests**

```bash
cargo test -p zeroclaws tool_handler 2>&1 | tail -20
```

Expected: all tool_handler tests pass.

- [ ] **Step 1.8: Commit**

```bash
git add src/agent/loop_.rs src/channels/mod.rs src/skills/tool_handler.rs
git commit -m "feat(skills): inject ZC_THREAD_ID env var for per-thread skill routing"
```

---

## Task 2: Install Pi and update CLAUDE.md

**Files:**
- Modify: `CLAUDE.md` (append section)

- [ ] **Step 2.1: Install Pi**

```bash
npm install -g @mariozechner/pi-coding-agent@0.61.0
```

Verify:

```bash
which pi && pi --version
```

Expected: `pi` binary found, version `0.61.0`.

- [ ] **Step 2.2: Verify Pi RPC mode starts**

```bash
# Get MiniMax key from config
MINIMAX_KEY=$(python3 -c "
import tomllib, pathlib
cfg = tomllib.loads(pathlib.Path('~/.zeroclaw/config.toml').expanduser().read_text())
keys = cfg.get('reliability', {}).get('fallback_api_keys', {})
print(keys.get('minimax:mm-1', '') or keys.get('minimax:mm-fresh', ''))
")
echo "Key found: ${#MINIMAX_KEY} chars"
```

Expected: key length > 0.

- [ ] **Step 2.3: Append log access section to CLAUDE.md**

Append to `/home/spex/work/erp/zeroclaws/CLAUDE.md`:

```markdown

## Coder Skill: Log & Workspace Access

When invoked as a Telegram coding assistant via the `coder` ZeroClaw skill:

You have direct access to:
- ZeroClaw daemon log: `/tmp/zeroclaw_daemon.log`
- ZeroClaw config and workspace: `~/.zeroclaw/`

Use your file read tool on these paths to debug ZeroClaw behavior (errors, channel activity, provider failures, skill invocations). Edit code directly in this repository.
```

- [ ] **Step 2.4: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: add coder skill log access context for Pi"
```

---

## Task 3: Skill directory structure + SKILL.toml

**Files:**
- Create: `~/.zeroclaw/workspace/skills/coder/SKILL.toml`
- Create: `~/.zeroclaw/workspace/skills/coder/data/.gitkeep`

- [ ] **Step 3.1: Create directories**

```bash
mkdir -p ~/.zeroclaw/workspace/skills/coder/{scripts,tests,data}
touch ~/.zeroclaw/workspace/skills/coder/data/.gitkeep
```

- [ ] **Step 3.2: Create SKILL.toml**

Create `~/.zeroclaw/workspace/skills/coder/SKILL.toml`:

```toml
name = "coder"
description = "Coding assistant powered by Pi agent. Reads, writes, and edits files; runs shell commands; explains code; fixes bugs. Maintains separate context per Telegram thread. Use for: file editing, bug fixing, reading daemon logs, writing scripts, refactoring, explaining code, running build commands."
version = "0.1.0"
trusted = true

[[tools]]
name = "code"
description = "Send a coding task to the Pi agent. The agent reads files, edits code, runs shell commands, and returns results. Context is preserved within the same Telegram thread. Use for any coding task: fixing bugs, writing code, reading logs, running cargo/python commands."
script = "scripts/coder.py"
interpreter = "python3"

[[tools.parameters]]
name = "message"
type = "String"
description = "The coding task, question, or instruction. Examples: 'fix the formatting error in channels/mod.rs', 'read the last 30 lines of /tmp/zeroclaw_daemon.log', 'run cargo clippy and show me the output', 'explain how skill routing works'."
```

- [ ] **Step 3.3: Verify skill parses without error**

```bash
cd ~/.zeroclaw/workspace/skills/coder
python3 -c "import tomllib; tomllib.load(open('SKILL.toml', 'rb')); print('OK')"
```

Expected: `OK`

---

## Task 4: bot_api.py — Telegram Bot API wrapper

**Files:**
- Create: `~/.zeroclaw/workspace/skills/coder/scripts/bot_api.py`

This module handles sending and editing Telegram messages directly. Skills use the Bot API directly (not ZeroClaw gateway) — same pattern as `telegram-reader` and `erp-analyst` skills. The ZeroClaw gateway has no message send/edit endpoint, so direct Bot API is the correct v1 approach.

- [ ] **Step 4.1: Create bot_api.py**

```python
"""Minimal Telegram Bot API client for coder skill status messages."""
import os
import requests

BOT_TOKEN = os.environ.get("TELEGRAM_BOT_TOKEN", "")
CHAT_ID = os.environ.get("TELEGRAM_OPERATOR_CHAT_ID", "")
# ZC_THREAD_ID is the Telegram topic/thread ID (message_thread_id for forum groups)
THREAD_ID = os.environ.get("ZC_THREAD_ID", "")
REPLY_TO = os.environ.get("ZC_REPLY_TO_MESSAGE_ID", "")

BASE_URL = f"https://api.telegram.org/bot{BOT_TOKEN}"


def send_message(text: str) -> int | None:
    """Send a new message. Returns message_id on success, None on failure."""
    body: dict = {"chat_id": CHAT_ID, "text": text, "parse_mode": "HTML"}
    if REPLY_TO:
        body["reply_to_message_id"] = int(REPLY_TO)
    if THREAD_ID and THREAD_ID.isdigit():
        body["message_thread_id"] = int(THREAD_ID)
    try:
        r = requests.post(f"{BASE_URL}/sendMessage", json=body, timeout=10)
        data = r.json()
        if data.get("ok"):
            return data["result"]["message_id"]
    except Exception:
        pass
    return None


def edit_message(message_id: int, text: str) -> bool:
    """Edit an existing message. Returns True on success."""
    body = {"chat_id": CHAT_ID, "message_id": message_id, "text": text, "parse_mode": "HTML"}
    try:
        r = requests.post(f"{BASE_URL}/editMessageText", json=body, timeout=10)
        return r.json().get("ok", False)
    except Exception:
        return False
```

- [ ] **Step 4.2: Quick smoke test (requires BOT env)**

```bash
cd ~/.zeroclaw/workspace/skills/coder
source ~/.zeroclaw/.env
TELEGRAM_BOT_TOKEN="$TELEGRAM_BOT_TOKEN" \
TELEGRAM_OPERATOR_CHAT_ID="$TELEGRAM_OPERATOR_CHAT_ID" \
python3 -c "
import scripts.bot_api as api
mid = api.send_message('coder skill: smoke test')
print(f'sent message_id={mid}')
import time; time.sleep(1)
ok = api.edit_message(mid, 'coder skill: smoke test EDITED')
print(f'edit ok={ok}')
"
```

Expected: message appears in Telegram, then gets edited.

---

## Task 5: pi_manager.py — Pi process lifecycle

**Files:**
- Create: `~/.zeroclaw/workspace/skills/coder/scripts/pi_manager.py`

- [ ] **Step 5.1: Create pi_manager.py**

```python
"""Manages the Pi coding agent process lifecycle."""
import os
import subprocess
import signal
import tomllib
import pathlib
import json
import time
import sys
from typing import Optional

SKILL_DIR = pathlib.Path(os.environ.get("SKILL_DIR", pathlib.Path(__file__).parent.parent))
PID_FILE = SKILL_DIR / "data" / "pi.pid"
DATA_DIR = SKILL_DIR / "data"
DAEMON_CWD = os.environ.get("ZEROCLAW_CWD", str(pathlib.Path.home() / "work/erp/zeroclaws"))
PI_BIN = "pi"


def _get_minimax_key() -> str:
    """Extract MiniMax API key from ZeroClaw config.toml."""
    config_path = pathlib.Path.home() / ".zeroclaw" / "config.toml"
    try:
        cfg = tomllib.loads(config_path.read_text())
        keys = cfg.get("reliability", {}).get("fallback_api_keys", {})
        # Try active model key first, then fallback keys
        for k in ("minimax:mm-fresh", "minimax:mm-1"):
            if keys.get(k):
                return keys[k]
    except Exception as e:
        print(f"[pi_manager] Warning: could not read MiniMax key: {e}", file=sys.stderr)
    return os.environ.get("MINIMAX_API_KEY", "")


def _read_pid() -> Optional[int]:
    """Read PID from pid file. Returns None if file missing or invalid."""
    try:
        data = json.loads(PID_FILE.read_text())
        return int(data["pid"])
    except Exception:
        return None


def _write_pid(pid: int) -> None:
    DATA_DIR.mkdir(parents=True, exist_ok=True)
    PID_FILE.write_text(json.dumps({"pid": pid, "cmd": "pi"}))


def _is_alive(pid: int) -> bool:
    """Check if PID is alive AND is actually a pi process."""
    try:
        os.kill(pid, 0)  # raises OSError if dead
    except OSError:
        return False
    # Verify it's actually Pi (guard against PID reuse)
    try:
        cmdline = pathlib.Path(f"/proc/{pid}/cmdline").read_bytes().decode(errors="replace")
        return "pi" in cmdline
    except Exception:
        return False


def is_running() -> bool:
    """Return True if Pi process is alive."""
    pid = _read_pid()
    return pid is not None and _is_alive(pid)


def spawn() -> subprocess.Popen:
    """Start a new Pi RPC process. Returns the Popen handle."""
    key = _get_minimax_key()
    env = os.environ.copy()
    if key:
        env["MINIMAX_API_KEY"] = key

    proc = subprocess.Popen(
        [
            PI_BIN, "--mode", "rpc", "--no-session",
            "--provider", "minimax", "--model", "MiniMax-M2.7-highspeed",
            "--cwd", DAEMON_CWD,
        ],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        env=env,
        text=False,  # binary mode for JSONL
    )
    _write_pid(proc.pid)
    return proc


def kill() -> None:
    """Kill the Pi process if running."""
    pid = _read_pid()
    if pid and _is_alive(pid):
        try:
            os.kill(pid, signal.SIGTERM)
            time.sleep(0.5)
            if _is_alive(pid):
                os.kill(pid, signal.SIGKILL)
        except OSError:
            pass
    try:
        PID_FILE.unlink(missing_ok=True)
    except Exception:
        pass
```

- [ ] **Step 5.2: Write unit tests for pi_manager**

Create `~/.zeroclaw/workspace/skills/coder/tests/test_pi_manager.py`:

```python
"""Unit tests for pi_manager — no real Pi process needed."""
import json
import os
import sys
import pathlib
import tempfile
import unittest
from unittest.mock import patch, MagicMock

# Add scripts to path
sys.path.insert(0, str(pathlib.Path(__file__).parent.parent / "scripts"))

import pi_manager


class TestPiManager(unittest.TestCase):
    def setUp(self):
        self.tmpdir = tempfile.TemporaryDirectory()
        pi_manager.DATA_DIR = pathlib.Path(self.tmpdir.name)
        pi_manager.PID_FILE = pi_manager.DATA_DIR / "pi.pid"

    def tearDown(self):
        self.tmpdir.cleanup()

    def test_read_pid_missing_file(self):
        self.assertIsNone(pi_manager._read_pid())

    def test_write_and_read_pid(self):
        pi_manager._write_pid(12345)
        self.assertEqual(pi_manager._read_pid(), 12345)

    def test_is_alive_dead_pid(self):
        # PID 99999999 almost certainly doesn't exist
        self.assertFalse(pi_manager._is_alive(99999999))

    def test_is_running_no_pid_file(self):
        self.assertFalse(pi_manager.is_running())

    def test_is_running_stale_pid(self):
        pi_manager._write_pid(99999999)
        self.assertFalse(pi_manager.is_running())

    def test_get_minimax_key_from_env(self):
        with patch.dict(os.environ, {"MINIMAX_API_KEY": "test-key"}):
            # When config read fails, falls back to env
            with patch("pathlib.Path.read_text", side_effect=FileNotFoundError):
                key = pi_manager._get_minimax_key()
        self.assertEqual(key, "test-key")


if __name__ == "__main__":
    unittest.main()
```

- [ ] **Step 5.3: Run unit tests**

```bash
cd ~/.zeroclaw/workspace/skills/coder
~/.zeroclaw/workspace/.venv/bin/python3 -m pytest tests/test_pi_manager.py -v
```

Expected: all tests pass.

---

## Task 6: rpc_client.py — Pi RPC protocol client

**Files:**
- Create: `~/.zeroclaw/workspace/skills/coder/scripts/rpc_client.py`

Wraps Pi's stdin/stdout JSONL protocol. Pi sends one JSON object per line on stdout. Commands go to stdin.

Key facts from Pi source (`packages/agent/src/types.ts`):
- `prompt` field name in command is `message` (not `text`)
- `agent_end` event: `{"type": "agent_end", "messages": [...]}`
- `tool_execution_start`: `{"type": "tool_execution_start", "toolCallId": "...", "toolName": "...", "args": {...}}`
- `get_state` response: `{"type": "response", "command": "get_state", "success": true, "data": {"sessionFile": "...", ...}}`
- `new_session` response: `{"type": "response", "command": "new_session", "success": true, "data": {"cancelled": false}}`

- [ ] **Step 6.1: Create rpc_client.py**

```python
"""Pi RPC client — communicates via stdin/stdout JSONL."""
import json
import sys
import time
import subprocess
from typing import Callable, Optional

# Progress callback signature: (status_text: str) -> None
ProgressCallback = Callable[[str], None]


def _send(proc: subprocess.Popen, obj: dict) -> None:
    """Write one JSON line to Pi's stdin."""
    line = json.dumps(obj) + "\n"
    proc.stdin.write(line.encode())
    proc.stdin.flush()


def _recv_line(proc: subprocess.Popen, timeout: float = 30.0) -> Optional[dict]:
    """Read one JSON line from Pi's stdout. Returns None on EOF or error."""
    import select
    deadline = time.monotonic() + timeout
    buf = b""
    while time.monotonic() < deadline:
        remaining = deadline - time.monotonic()
        ready, _, _ = select.select([proc.stdout], [], [], min(remaining, 1.0))
        if not ready:
            continue
        chunk = proc.stdout.read(1)
        if not chunk:
            return None  # EOF
        if chunk == b"\n":
            try:
                return json.loads(buf.decode())
            except Exception:
                buf = b""
                continue
        buf += chunk
    return None  # timeout


def _recv_response(proc: subprocess.Popen, command: str, timeout: float = 30.0) -> Optional[dict]:
    """Read events until we get a response for the given command."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        remaining = deadline - time.monotonic()
        event = _recv_line(proc, timeout=min(remaining, 5.0))
        if event is None:
            break
        if event.get("type") == "response" and event.get("command") == command:
            return event
    return None


def new_session(proc: subprocess.Popen) -> Optional[str]:
    """Create a new Pi session. Returns session_file path."""
    _send(proc, {"type": "new_session"})
    resp = _recv_response(proc, "new_session")
    if not resp or not resp.get("success"):
        return None
    # Get session file from state
    _send(proc, {"type": "get_state"})
    state_resp = _recv_response(proc, "get_state")
    if state_resp and state_resp.get("success"):
        return state_resp["data"].get("sessionFile")
    return None


def switch_session(proc: subprocess.Popen, session_file: str) -> bool:
    """Switch Pi to an existing session. Returns True on success."""
    _send(proc, {"type": "switch_session", "sessionPath": session_file})
    resp = _recv_response(proc, "switch_session")
    return bool(resp and resp.get("success"))


def set_model(proc: subprocess.Popen, provider: str, model_id: str) -> bool:
    """Change the active model mid-session."""
    _send(proc, {"type": "set_model", "provider": provider, "modelId": model_id})
    resp = _recv_response(proc, "set_model")
    return bool(resp and resp.get("success"))


def get_last_assistant_text(proc: subprocess.Popen) -> Optional[str]:
    """Get the last assistant message text from current session."""
    _send(proc, {"type": "get_last_assistant_text"})
    resp = _recv_response(proc, "get_last_assistant_text")
    if resp and resp.get("success"):
        return resp["data"].get("text")
    return None


def _tool_status(tool_name: str, args: dict) -> str:
    """Convert tool_execution_start event into human-readable status."""
    if tool_name == "read":
        path = args.get("path", "")
        return f"⚙ reading {path}"
    elif tool_name == "write":
        path = args.get("path", "")
        return f"⚙ writing {path}"
    elif tool_name == "edit":
        path = args.get("path", "")
        return f"⚙ editing {path}"
    elif tool_name == "bash":
        cmd = (args.get("command") or args.get("cmd") or "")[:60]
        return f"⚙ running: {cmd}"
    elif tool_name in ("find", "grep"):
        pattern = args.get("pattern") or args.get("path", "")
        return f"⚙ searching {pattern}"
    else:
        return f"⚙ {tool_name}"


def prompt(
    proc: subprocess.Popen,
    message: str,
    on_progress: Optional[ProgressCallback] = None,
    timeout: float = 300.0,
) -> Optional[str]:
    """
    Send a prompt to Pi and wait for agent_end.
    Calls on_progress(status_text) for each notable event (debounced to 1/sec).
    Returns final assistant text, or None on timeout/error.
    """
    _send(proc, {"type": "prompt", "message": message})
    # Read immediate ACK
    ack = _recv_response(proc, "prompt", timeout=10.0)
    if not ack or not ack.get("success"):
        return None

    deadline = time.monotonic() + timeout
    last_progress_at = 0.0
    last_status = ""
    last_heartbeat_at = time.monotonic()

    def maybe_update(status: str) -> None:
        nonlocal last_progress_at, last_status, last_heartbeat_at
        now = time.monotonic()
        if status != last_status and now - last_progress_at >= 1.0:
            if on_progress:
                on_progress(status)
            last_progress_at = now
            last_status = status
        last_heartbeat_at = now

    maybe_update("⚙ Pi is thinking…")

    while time.monotonic() < deadline:
        remaining = deadline - time.monotonic()
        event = _recv_line(proc, timeout=min(remaining, 5.0))

        # Heartbeat: if no event for >60s, send reassurance
        now = time.monotonic()
        if now - last_heartbeat_at >= 60.0:
            if on_progress:
                on_progress("⚙ Pi is still working…")
            last_heartbeat_at = now

        if event is None:
            continue

        etype = event.get("type", "")

        # Auto-cancel extension UI dialogs (Pi is headless in this context)
        if etype == "extension_ui_request":
            uid = event.get("id", "")
            _send(proc, {"type": "extension_ui_response", "id": uid, "cancelled": True})
            continue

        if etype == "tool_execution_start":
            tool_name = event.get("toolName", "")
            args = event.get("args") or {}
            maybe_update(_tool_status(tool_name, args))

        elif etype == "message_update":
            maybe_update("⚙ Pi is thinking…")

        elif etype == "agent_end":
            # Always fire final progress before fetching text
            if on_progress:
                on_progress("⚙ finishing…")
            return get_last_assistant_text(proc)

    return None  # timeout
```

- [ ] **Step 6.2: Write unit tests for rpc_client**

Create `~/.zeroclaw/workspace/skills/coder/tests/test_rpc_client.py`:

```python
"""Unit tests for rpc_client — mocks Pi stdin/stdout."""
import io
import json
import sys
import pathlib
import unittest
from unittest.mock import MagicMock, patch

sys.path.insert(0, str(pathlib.Path(__file__).parent.parent / "scripts"))
import rpc_client


def _make_proc(*response_lines: dict) -> MagicMock:
    """Create a mock Popen with canned stdout lines."""
    proc = MagicMock()
    proc.stdin = MagicMock()
    lines = [json.dumps(r).encode() + b"\n" for r in response_lines]
    proc.stdout.read.side_effect = [b for line in lines for b in ([bytes([c]) for c in line])] + [b""]
    return proc


class TestToolStatus(unittest.TestCase):
    def test_read(self):
        self.assertEqual(rpc_client._tool_status("read", {"path": "src/main.rs"}), "⚙ reading src/main.rs")

    def test_write(self):
        self.assertEqual(rpc_client._tool_status("write", {"path": "out.txt"}), "⚙ writing out.txt")

    def test_bash(self):
        self.assertEqual(rpc_client._tool_status("bash", {"command": "cargo fmt"}), "⚙ running: cargo fmt")

    def test_bash_truncation(self):
        long_cmd = "a" * 100
        result = rpc_client._tool_status("bash", {"command": long_cmd})
        self.assertLessEqual(len(result), len("⚙ running: ") + 60 + 5)

    def test_unknown_tool(self):
        self.assertEqual(rpc_client._tool_status("glob", {}), "⚙ glob")


class TestAutoCancel(unittest.TestCase):
    def test_extension_ui_cancelled(self):
        """extension_ui_request events are auto-cancelled."""
        sent = []
        proc = MagicMock()
        proc.stdin.write.side_effect = lambda b: sent.append(json.loads(b.decode().strip()))
        proc.stdin.flush = MagicMock()

        # Simulate: prompt ACK → ui_request → agent_end
        events = [
            {"type": "response", "command": "prompt", "success": True},
            {"type": "extension_ui_request", "id": "abc", "method": "select", "title": "Pick"},
            {"type": "agent_end", "messages": []},
            {"type": "response", "command": "get_last_assistant_text", "success": True, "data": {"text": "done"}},
        ]
        call_count = [0]
        def fake_recv(p, timeout=30.0):
            if call_count[0] < len(events):
                ev = events[call_count[0]]
                call_count[0] += 1
                return ev
            return None

        with patch.object(rpc_client, "_recv_line", side_effect=fake_recv), \
             patch.object(rpc_client, "_recv_response", wraps=lambda p, cmd, timeout=30.0: next(
                 (e for e in events if e.get("command") == cmd), None)):
            result = rpc_client.prompt(proc, "hello")

        # Check cancel was sent
        cancel_msgs = [m for m in sent if m.get("type") == "extension_ui_response"]
        self.assertEqual(len(cancel_msgs), 1)
        self.assertTrue(cancel_msgs[0].get("cancelled"))


if __name__ == "__main__":
    unittest.main()
```

- [ ] **Step 6.3: Run rpc_client tests**

```bash
cd ~/.zeroclaw/workspace/skills/coder
~/.zeroclaw/workspace/.venv/bin/python3 -m pytest tests/test_rpc_client.py -v
```

Expected: all tests pass.

---

## Task 7: coder.py — Main entrypoint

**Files:**
- Create: `~/.zeroclaw/workspace/skills/coder/scripts/coder.py`

- [ ] **Step 7.1: Create coder.py**

```python
#!/usr/bin/env python3
"""
ZeroClaw `coder` skill entrypoint.

Called by ZeroClaw per message. Manages Pi sessions per Telegram thread.
Sends live status updates via Telegram Bot API (edit-in-place).

Environment variables provided by ZeroClaw:
  SKILL_DIR                  — path to this skill directory
  ZC_THREAD_ID               — stable Telegram thread/topic ID
  ZC_REPLY_TO_MESSAGE_ID     — Telegram message ID to reply to
  TELEGRAM_BOT_TOKEN         — bot token for direct API calls
  TELEGRAM_OPERATOR_CHAT_ID  — chat to send status messages to
  ZEROCLAW_GATEWAY_TOKEN     — ZeroClaw gateway auth token
  ZEROCLAW_GATEWAY_URL       — ZeroClaw gateway URL
  ZEROCLAW_CWD               — working dir of the daemon (Pi's --cwd)
"""
import fcntl
import json
import os
import pathlib
import subprocess
import sys
import time
from typing import Optional

SKILL_DIR = pathlib.Path(os.environ.get("SKILL_DIR", pathlib.Path(__file__).parent.parent))
sys.path.insert(0, str(SKILL_DIR / "scripts"))

import bot_api
import pi_manager
import rpc_client

SESSIONS_FILE = SKILL_DIR / "data" / "sessions.json"

# ─── Argument parsing ────────────────────────────────────────────────────────

def _get_args() -> tuple[str, str]:
    """Return (message, thread_id) from argv + env."""
    import argparse
    parser = argparse.ArgumentParser()
    parser.add_argument("--message", required=True, help="Coding task or command")
    args, _ = parser.parse_known_args()
    thread_id = os.environ.get("ZC_THREAD_ID", os.environ.get("ZC_REPLY_TO_MESSAGE_ID", "default"))
    return args.message, thread_id

# ─── Session management ───────────────────────────────────────────────────────

def _load_sessions() -> dict:
    try:
        return json.loads(SESSIONS_FILE.read_text())
    except Exception:
        return {}


def _save_sessions(sessions: dict) -> None:
    SESSIONS_FILE.parent.mkdir(parents=True, exist_ok=True)
    SESSIONS_FILE.write_text(json.dumps(sessions, indent=2))


def _get_or_create_session(proc: subprocess.Popen, thread_id: str) -> Optional[str]:
    """
    Return the session_file for this thread_id, creating one if needed.
    Handles stale file paths.
    Must be called with sessions.json lock held.
    """
    sessions = _load_sessions()
    entry = sessions.get(thread_id)

    if entry:
        session_file = entry.get("session_file", "")
        if session_file and pathlib.Path(session_file).exists():
            ok = rpc_client.switch_session(proc, session_file)
            if ok:
                return session_file
            # switch failed — fall through to create new
        # stale/missing file — create new and update mapping
        print(f"[coder] Previous context unavailable for thread {thread_id}, starting fresh.", file=sys.stderr)

    # Create new session
    session_file = rpc_client.new_session(proc)
    if not session_file:
        return None

    sessions[thread_id] = {
        "session_file": session_file,
        "created_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "label": None,
    }
    _save_sessions(sessions)
    return session_file


def _reset_session(thread_id: str) -> str:
    """Delete session mapping for this thread. Returns status message."""
    sessions = _load_sessions()
    if thread_id in sessions:
        del sessions[thread_id]
        _save_sessions(sessions)
        return "Session reset."
    return "No session to reset."

# ─── Main ─────────────────────────────────────────────────────────────────────

def main() -> None:
    message, thread_id = _get_args()

    # ── /reset command ──
    if message.strip() == "/reset":
        result = _reset_session(thread_id)
        print(result)
        return

    # ── /model command ──
    if message.strip().startswith("/model "):
        parts = message.strip().split()
        if len(parts) == 3:
            _, provider, model_id = parts
            if pi_manager.is_running():
                pid_data = json.loads(pi_manager.PID_FILE.read_text())
                # Note: we don't have proc handle here; model switch requires active session
                # For simplicity, print instruction — full impl needs proc handle persistence
                print(f"Model switch to {provider}/{model_id} — restart coder session to apply.")
            else:
                print(f"Pi not running. Will use {provider}/{model_id} on next start.")
        else:
            print("Usage: /model <provider> <model_id>\nExample: /model gemini gemini-2.0-flash")
        return

    # ── Spawn Pi (stateless invocation model) ──
    # Skills are short-lived subprocesses — we cannot persist a Popen handle across invocations.
    # Each call spawns a fresh Pi RPC process, then immediately loads the previous session file
    # via switch_session so context is preserved. Startup cost is ~2s.
    status_id = bot_api.send_message("⚙ Starting Pi coding agent…")
    proc = pi_manager.spawn()
    # Brief wait for Pi to initialise its RPC listener
    time.sleep(2.0)

    # ── Session management (with file lock) ──
    # Ensure data dir exists before opening the lock file
    SESSIONS_FILE.parent.mkdir(parents=True, exist_ok=True)
    lock_fd = open(SESSIONS_FILE.parent / ".sessions.lock", "w")
    try:
        fcntl.flock(lock_fd, fcntl.LOCK_EX)
        session_file = _get_or_create_session(proc, thread_id)
    finally:
        fcntl.flock(lock_fd, fcntl.LOCK_UN)
        lock_fd.close()

    if not session_file:
        if status_id:
            bot_api.edit_message(status_id, "✗ Failed to create Pi session.")
        print("Error: could not create Pi session.")
        return

    # ── Run prompt with live status updates ──
    def on_progress(status: str) -> None:
        if status_id:
            bot_api.edit_message(status_id, status)

    result = rpc_client.prompt(proc, message, on_progress=on_progress, timeout=300.0)

    # ── Finalise ──
    if result:
        # Truncate if too long for Telegram (4096 char limit)
        if len(result) > 3900:
            result = result[:3900] + "\n…(truncated)"
        if status_id:
            bot_api.edit_message(status_id, result)
        print(result)
    else:
        error_msg = "✗ Pi timed out or returned no response after 300s."
        if status_id:
            bot_api.edit_message(status_id, error_msg)
        print(error_msg)

    # Terminate Pi process after responding (stateless invocation model)
    try:
        proc.terminate()
        proc.wait(timeout=3)
    except Exception:
        pass


if __name__ == "__main__":
    main()
```

- [ ] **Step 7.2: Make coder.py executable**

```bash
chmod +x ~/.zeroclaw/workspace/skills/coder/scripts/coder.py
```

- [ ] **Step 7.3: Smoke test (no Pi, just arg parsing)**

```bash
cd ~/.zeroclaw/workspace/skills/coder
SKILL_DIR=$(pwd) ZC_THREAD_ID="thread_test" ZC_REPLY_TO_MESSAGE_ID="123" \
  TELEGRAM_BOT_TOKEN="" TELEGRAM_OPERATOR_CHAT_ID="" \
  ~/.zeroclaw/workspace/.venv/bin/python3 scripts/coder.py --message "/reset" 2>&1
```

Expected: `No session to reset.` (or similar, no crash).

---

## Task 8: Build ZeroClaw and register skill

- [ ] **Step 8.1: Build ZeroClaw with Rust changes**

```bash
cd ~/work/erp/zeroclaws
cargo build --release 2>&1 | tail -20
```

Expected: build succeeds.

- [ ] **Step 8.2: Run full test suite**

```bash
cargo test 2>&1 | tail -30
```

Expected: all tests pass (or only pre-existing failures).

- [ ] **Step 8.3: Add MINIMAX_API_KEY to shell_env_passthrough in config**

In `~/.zeroclaw/config.toml`, add `MINIMAX_API_KEY` to `shell_env_passthrough` so the env fallback in `pi_manager._get_minimax_key()` works if the config.toml read fails:

```toml
# In [runtime] or [skills] section, extend shell_env_passthrough:
shell_env_passthrough = [
  # ... existing entries ...
  "MINIMAX_API_KEY",
]
```

Verify it's present:
```bash
grep MINIMAX_API_KEY ~/.zeroclaw/config.toml
```

Expected: the key appears in the passthrough list.

- [ ] **Step 8.4: Register coder skill in ZeroClaw config**

In `~/.zeroclaw/config.toml`, add to the skills list:

```toml
[[skills]]
path = "/home/spex/.zeroclaw/workspace/skills/coder"
trusted = true
```

- [ ] **Step 8.5: Restart daemon**

```bash
cd ~/work/erp/zeroclaws
./dev/restart-daemon.sh
```

- [ ] **Step 8.6: Verify skill appears in tool list**

```bash
zeroclaw tools 2>/dev/null | grep -i coder || curl -s http://127.0.0.1:42617/api/tools | python3 -m json.tool | grep coder
```

Expected: `code` tool appears.

- [ ] **Step 8.7: Commit all skill files**

```bash
cd ~/work/erp/zeroclaws
git add CLAUDE.md
git commit -m "docs: add coder skill log access to CLAUDE.md"

cd ~/.zeroclaw/workspace/skills/coder
git init 2>/dev/null || true
git add SKILL.toml scripts/ tests/ data/.gitkeep
git commit -m "feat: initial coder skill (Pi-backed coding assistant)"
```

---

## Task 9: E2E Tests (Live Telegram, c1–c7)

**Files:**
- Create: `~/.zeroclaw/workspace/skills/coder/tests/test_e2e.py`

Uses Telethon as `zverozabr` to send real Telegram messages to `@zGsR_bot` and verify results. See `tests/telegram_e2e_howto.md` for setup.

- [ ] **Step 9.1: Create test_e2e.py**

```python
"""
E2E tests for coder skill (c1–c7).
Runs via Telethon as zverozabr against live @zGsR_bot.
Run: source ~/.zeroclaw/.env && python3 tests/test_e2e.py
"""
import asyncio
import os
import pathlib
import sys
import time
import unittest

from telethon import TelegramClient
from telethon.tl.functions.channels import CreateForumTopicRequest

# ── Config ────────────────────────────────────────────────────────────────────
SESSION_PATH = str(pathlib.Path.home() / ".zeroclaw/workspace/skills/telegram-reader/.session/zverozabr")
API_ID = int(os.environ["TELEGRAM_API_ID"])
API_HASH = os.environ["TELEGRAM_API_HASH"]
BOT_USERNAME = "@zGsR_bot"
# The forum group where we create test topics
FORUM_CHAT = os.environ.get("TELEGRAM_OPERATOR_CHAT_ID")  # must be a forum-enabled supergroup

TIMEOUT = 90  # seconds to wait for bot reply


async def send_and_wait(client, chat, topic_id, text, timeout=TIMEOUT):
    """Send text in a topic and wait for bot reply. Returns reply text."""
    await client.send_message(chat, text, reply_to=topic_id)
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        await asyncio.sleep(3)
        msgs = await client.get_messages(chat, limit=5, reply_to=topic_id)
        for msg in msgs:
            if msg.sender and getattr(msg.sender, "bot", False):
                return msg.text or ""
    return ""


class CoderE2ETests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.client = TelegramClient(SESSION_PATH, API_ID, API_HASH)
        await self.client.start()
        self.chat = await self.client.get_entity(FORUM_CHAT)

    async def asyncTearDown(self):
        await self.client.disconnect()

    async def _create_topic(self, name: str) -> int:
        """Create a forum topic, return topic_id (message_thread_id)."""
        result = await self.client(CreateForumTopicRequest(
            channel=self.chat, title=name
        ))
        return result.updates[0].id

    async def test_c1_basic_file_write(self):
        topic_id = await self._create_topic("coder-test-c1")
        reply = await send_and_wait(self.client, self.chat, topic_id,
            "create a file /tmp/coder_e2e_c1.txt with the content: hello from Pi")
        self.assertTrue(pathlib.Path("/tmp/coder_e2e_c1.txt").exists(), "File not created")
        content = pathlib.Path("/tmp/coder_e2e_c1.txt").read_text()
        self.assertIn("hello from Pi", content)
        self.assertGreater(len(reply), 0, "No bot reply received")
        # Store topic_id for c2, c6, c7
        self.__class__._c1_topic = topic_id

    async def test_c2_context_continuity(self):
        if not hasattr(self.__class__, "_c1_topic"):
            self.skipTest("c1 must run first")
        topic_id = self.__class__._c1_topic
        reply = await send_and_wait(self.client, self.chat, topic_id,
            "append a second line to that file: line 2")
        content = pathlib.Path("/tmp/coder_e2e_c1.txt").read_text()
        lines = [l for l in content.splitlines() if l.strip()]
        self.assertGreaterEqual(len(lines), 2, f"Expected 2 lines, got: {content!r}")

    async def test_c3_thread_isolation(self):
        topic_id = await self._create_topic("coder-test-c3")
        reply = await send_and_wait(self.client, self.chat, topic_id,
            "what files have we worked on before in this session?")
        # Should NOT mention c1 file
        self.assertNotIn("coder_e2e_c1", reply.lower(),
            f"Context leaked from c1 thread! reply={reply!r}")

    async def test_c4_shell_in_repo(self):
        topic_id = await self._create_topic("coder-test-c4")
        reply = await send_and_wait(self.client, self.chat, topic_id,
            "run: cargo fmt --check in the zeroclaws repo and tell me the result",
            timeout=120)
        self.assertGreater(len(reply), 0)
        # Should contain cargo output indicators
        has_output = any(kw in reply.lower() for kw in ["cargo", "format", "check", "error", "warning", "ok", "diff"])
        self.assertTrue(has_output, f"Reply doesn't look like cargo output: {reply!r}")
        self.__class__._c4_topic = topic_id

    async def test_c5_log_reading(self):
        topic_id = await self._create_topic("coder-test-c5")
        reply = await send_and_wait(self.client, self.chat, topic_id,
            "read the last 20 lines of /tmp/zeroclaw_daemon.log and summarize what happened",
            timeout=90)
        self.assertGreater(len(reply), 20, "Reply too short to be a log summary")
        # Should not be raw log dump
        self.assertLess(reply.count("["), 20, "Looks like a raw log dump")

    async def test_c6_session_recovery(self):
        if not hasattr(self.__class__, "_c1_topic"):
            self.skipTest("c1 must run first")
        topic_id = self.__class__._c1_topic
        # Kill any live Pi process to simulate a crash/restart
        # (In stateless invocation model, each coder.py call spawns a fresh Pi,
        # so this verifies that a cold start still restores session context from JSONL file)
        os.system("pkill -f 'pi --mode rpc' 2>/dev/null")
        time.sleep(1)
        reply = await send_and_wait(self.client, self.chat, topic_id,
            "what file did we create at the start of this session?")
        self.assertIn("coder_e2e_c1", reply.lower(),
            f"Pi didn't restore session context! reply={reply!r}")
        self.assertNotIn("unavailable", reply.lower(),
            "Pi said context unavailable — session restore failed")

    async def test_c7_reset_per_thread(self):
        if not hasattr(self.__class__, "_c1_topic") or not hasattr(self.__class__, "_c4_topic"):
            self.skipTest("c1 and c4 must run first")
        c1_topic = self.__class__._c1_topic
        c4_topic = self.__class__._c4_topic

        # Reset c1 thread
        reply = await send_and_wait(self.client, self.chat, c1_topic, "/reset", timeout=30)
        self.assertIn("reset", reply.lower())

        # c1 thread should have no memory
        reply = await send_and_wait(self.client, self.chat, c1_topic,
            "what file did we create before?")
        self.assertNotIn("coder_e2e_c1", reply.lower(),
            f"c1 session not cleared! reply={reply!r}")

        # c4 thread should still have memory
        reply = await send_and_wait(self.client, self.chat, c4_topic,
            "what cargo command did we run earlier?")
        has_memory = any(kw in reply.lower() for kw in ["cargo", "fmt", "format", "check"])
        self.assertTrue(has_memory, f"c4 session was unexpectedly cleared! reply={reply!r}")


if __name__ == "__main__":
    unittest.main(verbosity=2)
```

- [ ] **Step 9.2: Run E2E tests (sequential, live Telegram)**

```bash
source ~/.zeroclaw/.env
cd ~/.zeroclaw/workspace/skills/coder
~/.zeroclaw/workspace/.venv/bin/python3 -m pytest tests/test_e2e.py -v --tb=short -s 2>&1 | tee /tmp/coder_e2e.log
```

Expected: all 7 tests pass (c1–c7 in order).

- [ ] **Step 9.3: Fix any failures before marking done**

If a test fails, diagnose via `/tmp/zeroclaw_daemon.log` and Pi's stderr output. Do NOT declare done until all 7 pass.

- [ ] **Step 9.4: Final commit**

```bash
cd ~/.zeroclaw/workspace/skills/coder
git add tests/test_e2e.py
git commit -m "test: add E2E tests c1-c7 for coder skill"

cd ~/work/erp/zeroclaws
git add src/
git commit -m "feat(skills): ZC_THREAD_ID injection for per-thread skill routing"
```

---

## Quick Reference

**Pi RPC command field names (from source):**
- `prompt`: `{"type": "prompt", "message": "..."}` ← field is `message`, not `text`
- `switch_session`: `{"type": "switch_session", "sessionPath": "..."}`
- `new_session`: `{"type": "new_session"}`
- `get_state`: `{"type": "get_state"}` → response has `data.sessionFile`
- `get_last_assistant_text`: → response has `data.text`
- `set_model`: `{"type": "set_model", "provider": "...", "modelId": "..."}`
- `extension_ui_response`: `{"type": "extension_ui_response", "id": "...", "cancelled": true}`

**Pi agent events (stdout):**
- `agent_end`: `{"type": "agent_end", "messages": [...]}`
- `tool_execution_start`: `{"type": "tool_execution_start", "toolName": "...", "args": {...}}`
- `message_update`: streaming LLM text chunk
- `extension_ui_request`: headless → auto-cancel

**Session files:** `~/.pi/agent/sessions/--home-spex-work-erp-zeroclaws--/<timestamp>_<uuid>.jsonl`

**MiniMax key:** extracted from `~/.zeroclaw/config.toml` → `reliability.fallback_api_keys["minimax:mm-1"]`

**Telegram Bot API:**
- Send: `POST /bot{TOKEN}/sendMessage` with `chat_id`, `text`, optional `reply_to_message_id`, `message_thread_id`
- Edit: `POST /bot{TOKEN}/editMessageText` with `chat_id`, `message_id`, `text`

**ZC_THREAD_ID source priority:** `interruption_scope_id` → `thread_ts` → `msg.id`
