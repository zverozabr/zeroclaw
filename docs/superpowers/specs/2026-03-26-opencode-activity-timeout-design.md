# OpenCode Activity-Based Timeout & Agent Loop Visibility

**Date:** 2026-03-26
**Status:** Draft
**Branch:** `fix/opencode-transport-hardening`

## Problem

The current OpenCode transport has three critical issues:

1. **Blind timeout.** `POST /session/{id}/message` blocks until the OC agent loop finishes. The reqwest client has a 600s global timeout. A healthy coding session with many tool calls can exceed 600s, causing the request to be killed — indistinguishable from a genuine stall.

2. **No stall detection.** When the agent loop actually stalls (e.g., LLM provider hangs mid-stream), we wait the full 600s before noticing. There is no mechanism to detect "no progress for N seconds."

3. **Invisible activity.** The user in Telegram cannot see what the OC agent loop is doing. The polling infrastructure exists (`GET /message` every 2s, `on_status` callback) but when the agent stalls, there is no heartbeat — Telegram just shows "⚙️ OpenCode is working…" indefinitely.

### Root Cause (from log analysis)

OC server accepts the POST in ~8ms (logs `status=completed duration=8`), then runs the agent loop in background. The HTTP response body is not sent until the loop finishes. Meanwhile:

- reqwest blocks on `resp.json()` waiting for body
- Polling loop in `prompt_with_polling` runs via `tokio::select!` alongside the blocked send future
- Polling sees new parts (tool calls, text) but has no authority over timeout
- At 600s, reqwest kills the connection → `error sending request for url`
- Transport recovery restarts OC (which was healthy), creates fresh session, repeats the cycle

## Design

### 1. Fire-and-Forget Message Send

**Replace** blocking `send_message` with `send_message_async` (`POST /session/{id}/prompt_async`, returns 204 immediately).

**Before:**
```
POST /session/{id}/message  →  [blocks 0-600s]  →  response body
         ↕ (parallel)
   GET /message polling 2s
```

**After:**
```
POST /session/{id}/prompt_async  →  204 instant
         ↓
   GET /message polling 2s
         ↓
   Completion detection: GET /session/{id} → status == idle
         ↓
   Final text extracted from last GET /message response
```

### 2. Completion Detection

Add `is_session_idle(session_id) -> bool` check to the polling loop.

After each poll cycle, call `GET /session/{id}` and check if the session has returned to idle state. Combined with the presence of a final `text` part from an assistant message, this signals completion.

**Idle detection heuristic:**

The `GET /session/{id}` response shape (observed from live OC 1.3.2):
```json
{
  "id": "ses_xxx",
  "version": "1.3.2",
  "time": { "created": 1774504143062, "updated": 1774506962854 }
}
```

Primary detection: poll `GET /message` and check last assistant message parts:
- No `tool` parts with `state.status == "running"` or `state.status == "pending"`
- Has at least one `kind == "text"` part with non-empty text
- `time.updated` stopped advancing (same value for 2+ consecutive polls)

This avoids false positives between tool calls where the session briefly has no running tools.

### 3. Activity-Based Timeout

Two configurable thresholds in `config.toml`:

```toml
[opencode]
stall_warn_secs = 30     # start showing heartbeat in Telegram
stall_abort_secs = 120   # abort session + retry once
```

**Activity definition:** Any new part in polling response (tool start, tool result, text delta, step-start). If the `seen_parts` counter advances, activity timer resets.

**Startup grace period:** The stall timer does not start until the first part is seen from OC. Before that, a longer `stall_abort_secs * 2` grace period applies (OC may need time for model init, project indexing, etc.).

**Polling loop logic:**

```
last_activity = now()
retry_count = 0
first_part_seen = false

loop every 2s:
    parts = GET /session/{id}/message

    if new parts found:
        last_activity = now()
        first_part_seen = true
        on_status(tool/text/step)

    idle = now() - last_activity
    effective_timeout = stall_abort_secs if first_part_seen else stall_abort_secs * 2

    if idle > stall_warn_secs:
        on_status(Heartbeat { idle_secs })

    if idle > effective_timeout AND retry_count == 0:
        POST /session/{id}/abort
        POST /session/{id}/prompt_async  (re-send same message)
        last_activity = now()
        retry_count = 1
        on_status(Retrying)

    if idle > effective_timeout AND retry_count > 0:
        POST /session/{id}/abort
        return error to user

    if session is idle + final text present:
        return final text
```

### 4. Telegram Visibility

**New `PollingStatus` variant:**

```rust
enum PollingStatus {
    Thinking(String),
    Tool { name, status, detail, input, output },
    StepStart,
    Heartbeat { idle_secs: u64 },  // NEW
    Retrying,                       // NEW
    Stalled,                        // NEW
}
```

**What the user sees:**

```
⚙️ OpenCode is working…
💭 Thinking…
⚙️ `bash`: `cargo check`
✅ `bash` — Compiled successfully
⚙️ `read`: `src/main.rs`
✅ `read` — src/main.rs (142 lines)
⏳ Waiting for response… (35s)          ← heartbeat after stall_warn
⏳ Waiting for response… (60s)          ← updates every 2s
💭 I need to also update the tests      ← activity resumes, heartbeat gone
⚙️ `bash`: `cargo test`
✅ `bash` — 42 tests passed
```

**On stall abort + retry:**
```
⏳ Waiting for response… (120s)
🔄 Agent stalled, retrying…
💭 Thinking…
```

**On final failure:**
```
⏳ Waiting for response… (120s)
❌ Agent stalled after retry, please try again
```

**Heartbeat rendering rules:**
- Heartbeat is always the last line in the scrolling buffer
- Each new heartbeat replaces the previous one (not accumulated)
- When a real part arrives, heartbeat is replaced by the new status line

### 5. Simplified Transport Recovery

With fire-and-forget, `send_with_transport_recovery` simplifies:

```rust
async fn send_async_with_recovery(&self, session_id, text) -> Result<()> {
    match self.http_client.send_message_async(session_id, text, ...).await {
        Ok(()) => Ok(()),
        Err(ServerError { status: 404, .. }) => {
            // Session gone — create fresh, re-inject history
            let new_sid = self.send_with_fresh_session(history_key, text, history).await?;
            Ok(())
        }
        Err(_) => {
            // Transport error — restart OC, retry once
            pm.ensure_running().await?;
            self.http_client.send_message_async(session_id, text, ...).await?;
            Ok(())
        }
    }
}
```

The key difference: this returns instantly. No 600s wait. Errors are immediate (connection refused, 404, 500), not timeout-based.

**Server error handling (500/502/503):** If `prompt_async` returns 5xx, treat as transient — restart OC via `ensure_running()`, retry once. If retry also returns 5xx, return error to user. Same as current transport recovery but instantaneous.

**Duplicate message on retry after stall:** When the polling loop detects stall and retries (Section 3), the original message was already received by OC. Re-sending creates a duplicate user message. Mitigation: on retry, prefix with `[Continue from where you left off]` instead of raw user text. Alternatively, check `get_messages()` — if the user message already exists, skip re-send and just abort + let OC retry internally.

### 6. `/abort` Command — Manual Agent Stop

Telegram command `/abort` — stops the current OC agent loop for this chat. Equivalent to pressing Escape in OC CLI.

**Behavior:**
- Calls `POST /session/{id}/abort` on the active session for this history_key
- Cancels the polling loop in `prompt_with_polling`
- Shows `⛔ Aborted` in Telegram status message
- Does NOT destroy the session — context preserved, user can continue chatting

**Implementation:** Add `Abort` variant to `ChannelRuntimeCommand` enum and `parse_runtime_command`. Add handler in `handle_runtime_command_if_needed` alongside existing `/ps`, `/pf`, `/reset`. Reuses existing `mgr.abort()` method. Pass a `CancellationToken` into `prompt_with_polling` so `/abort` can break the polling loop immediately without waiting for the next 2s cycle.

### 7. `/oc` Command — Replaces `/models pi`

New shorthand `/oc` to toggle OpenCode mode instead of the verbose `/models pi`.

**Behavior:**
- `/oc` — toggle OC mode on/off for this chat
- `/oc on` — explicitly enable
- `/oc off` — explicitly disable (same as `/models pi` toggle-off)
- Shows confirmation: `🟢 OpenCode mode enabled` / `🔴 OpenCode mode disabled`

**Implementation:** Add handler in `handle_runtime_command_if_needed`. Internally sets `pi_mode = true/false` on the route selection and creates/stops OC session, same as current `/models pi` logic but with cleaner UX.

`/models pi` stays as alias for backwards compatibility.

**Important:** `/oc` uses the same `pi_mode` field on `ChannelRouteSelection` — not a parallel mechanism. If user did `/oc on` and then `/models minimax`, OC mode is implicitly disabled (same as current `/models pi` behavior).

## Files Changed

| File | Change |
|------|--------|
| `src/opencode/client.rs` | Remove global 600s timeout. Add `is_session_idle()` method. |
| `src/opencode/mod.rs` | Rewrite `prompt_with_polling`: fire-and-forget → polling → activity timeout → completion detection. Simplify transport recovery for async. Add `Heartbeat`, `Retrying`, `Stalled` to `PollingStatus`. |
| `src/config/schema.rs` | Add `stall_warn_secs` (default 30) and `stall_abort_secs` (default 120) to `OpenCodeConfig`. |
| `src/channels/mod.rs` | Handle new `PollingStatus` variants in on_status callback. Add `/abort` and `/oc` command handlers. |

**Not changed:** `events.rs`, `status.rs`, `telegram.rs`, `config.rs`, `process.rs`, `session.rs`. Watchdog and idle reaper unchanged.

## Testing

### E2E Validation

Send `/github_grep` command to bot via Telethon. Verify:
1. Telegram shows scrolling tool call log in real-time
2. If OC stalls, heartbeat appears after 30s
3. If stall continues to 120s, abort + retry happens
4. Final result delivered to user

### Unit Tests

- `prompt_with_polling` stall detection: mock OC server that stops producing parts → verify abort called after `stall_abort_secs`
- `prompt_with_polling` completion detection: mock OC server returning idle session → verify final text extracted
- `send_async_with_recovery` 404 path: verify fresh session created
- Heartbeat rendering: verify heartbeat replaces previous, disappears on real activity

## Risks

- **Completion detection false positive:** Session may briefly appear idle between tool calls. Mitigation: require both idle status AND final text part present.
- **`prompt_async` semantics:** If OC queues but doesn't start processing (e.g., busy with another session), polling will see no parts and may stall-timeout prematurely. Mitigation: first activity timer starts from prompt_async response, giving OC time to begin.
- **Retry in same session:** If the stall is caused by corrupted session state (not transient LLM hang), retry won't help. Mitigation: after one failed retry, return error — don't create fresh session (user can /reset manually).
