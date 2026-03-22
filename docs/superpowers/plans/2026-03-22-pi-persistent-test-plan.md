# Pi Persistent Mode — Test Plan

## Unit Tests (Rust, cargo test)

### 1. `pi::rpc` — JSONL protocol
| Test | Input | Expected |
|------|-------|----------|
| `parse_agent_end_extracts_text` | agent_end event with assistant text | `AgentEnd { text: "hello" }` |
| `parse_agent_end_empty_content` | agent_end with empty content | `None` |
| `parse_tool_execution_start` | tool_execution_start bash+pwd | `ToolStart { name: "bash", args }` |
| `parse_tool_execution_end` | tool_execution_end with output | `ToolEnd { name: "read", output }` |
| `parse_thinking_delta` | message_update thinking_delta | `ThinkingDelta("text")` |
| `parse_thinking_end` | message_update thinking_end | `ThinkingEnd("text")` |
| `parse_text_delta` | message_update text_delta | `TextDelta("text")` |
| `parse_unknown_event` | unknown type | `None` |
| `parse_malformed_json` | missing fields | `None` (no panic) |

### 2. `pi::status` — StatusBuilder
| Test | Action | Expected |
|------|--------|----------|
| `empty_renders_fallback` | new() → render() | "⚙ Pi is working…" |
| `renders_thinking` | on_thinking_end("text") | contains "💭 text" |
| `renders_tool_bash` | on_tool_start("bash", {cmd}) | contains "🔧 `cmd`" |
| `renders_tool_read` | on_tool_start("read", {path}) | contains "📖 path" |
| `renders_tool_write` | on_tool_start("edit", {path}) | contains "✏️" |
| `renders_tool_output` | on_tool_end("read", "data") | contains "📄" |
| `renders_response` | on_response_text("answer") | contains "answer" |
| `thinking_truncates_200` | on_thinking_end(300 chars) | output ≤ 200 + "…" |
| `tool_output_truncates_150` | on_tool_end(300 chars) | output ≤ 150 + "…" |
| `total_truncates_3800` | 100× on_thinking_end | render().len() ≤ 3800 |
| `keeps_tail_on_truncate` | 100 sections | last section visible, first trimmed |
| `full_sequence` | thinking → tool → output → response | all 4 present in order |

### 3. `pi::session` — PiSessionStore
| Test | Action | Expected |
|------|--------|----------|
| `save_and_load` | save("k", "path") → load("k") | Some("path") |
| `load_missing_key` | load("unknown") | None |
| `load_missing_file` | load from nonexistent path | None (no panic) |
| `delete_session` | save → delete → load | None |
| `save_updates_existing` | save("k", "old") → save("k", "new") → load | Some("new") |
| `multiple_keys` | save k1, k2 → load both | correct values |
| `delete_preserves_others` | save k1, k2 → delete k1 → load k2 | Some |

### 4. `pi::mod` — PiManager
| Test | Action | Expected |
|------|--------|----------|
| `is_running_false_initially` | is_running("key") | false |
| `kill_idle_empty` | kill_idle(0s) on empty map | no crash |
| `kill_idle_preserves_active` | insert active instance → kill_idle(30min) | still running |
| `needs_history_injection_true` | fresh instance | true |
| `needs_history_injection_false` | after inject_history | false |

## Integration Tests (Rust, spawn real Pi)

### 5. Pi spawn + prompt + kill
| Test | Steps | Expected |
|------|-------|----------|
| `spawn_and_prompt` | spawn Pi → prompt "say ok" → read response | response contains "ok" |
| `session_save_and_restore` | prompt → save session → kill → re-spawn → switch_session → prompt | context preserved |
| `prompt_timeout` | prompt with 1s timeout on slow task | Error with "timed out" |
| `concurrent_instances` | spawn 2 Pi for different keys → prompt both | both respond independently |

## E2E Telegram Tests (Python, live bot)

### 6. Phase 1: Activation via `/models pi`

| # | Send | Expected Response | Verify |
|---|------|------------------|--------|
| T1 | `/models pi` | "✅ Pi mode activated" | routes.json has pi_mode:true |
| T2 | "скажи только: activated-ok" | "activated-ok" | Pi responded, no LLM |

### 7. Phase 2: Messages without prefix

| # | Send | Expected | Verify |
|---|------|----------|--------|
| T3 | "сколько 2+2? одним числом" | "4" | Pi handles without "пи, " prefix |
| T4 | "запиши mango в /tmp/pi_persist_test.txt" | "записано" / "mango" | Pi writes file |
| T5 | "прочитай /tmp/pi_persist_test.txt" | "mango" | Pi reads file (same process, context) |
| T6 | "git branch --show-current" | "main" | Pi runs commands |
| T7 | "скажи: persist-test-ok" | "persist-test-ok" | Pi still active |

### 8. Phase 3: Context within session

| # | Send | Expected | Verify |
|---|------|----------|--------|
| T8 | "какой файл я просил создать?" | "pi_persist_test" or "mango" | Pi remembers from T4 (in-process context) |
| T9 | "сколько было 2+2?" | "4" or "42" reference | Pi remembers from T3 |

### 9. Phase 4: Deactivation

| # | Send | Expected | Verify |
|---|------|----------|--------|
| T10 | `/models minimax` | "Switched to minimax" | Pi stopped, LLM active |
| T11 | "скажи: llm-ok" | response without Pi status | LLM responds, no "⚙ Starting Pi" |
| T12 | "что Pi делал?" | mentions mango, 2+2, persist-test-ok | LLM sees [Pi] in history |

### 10. Phase 5: Session restore

| # | Send | Expected | Verify |
|---|------|----------|--------|
| T13 | `/models pi` | "✅ Pi mode activated" | Pi re-spawned |
| T14 | "какой файл мы создавали раньше?" | "pi_persist_test" or "mango" | Session restored, old context available |
| T15 | "скажи: session-restored-ok" | "session-restored-ok" | Pi works after restore |

### 11. Phase 6: Idle timeout

| # | Action | Expected | Verify |
|---|--------|----------|--------|
| T16 | Wait idle timeout (or simulate) | Pi process killed | `ps aux | grep pi` shows no Pi |
| T17 | Send message after idle kill | Pi re-spawns, session loaded | Response comes with ~4s delay |

### 12. Phase 7: Status display

| # | Send | Expected in Telegram | Verify |
|---|------|---------------------|--------|
| T18 | "прочитай /tmp/pi_persist_test.txt" | See transitions: ⚙→💭→📖→📄→response | Status message edited in-place |
| T19 | "запусти pwd" | See: 💭 thinking → 🔧 running: pwd → response | Tool call visible in status |

### 13. Phase 8: Cross-chat isolation

| # | Action | Expected | Verify |
|---|--------|----------|--------|
| T20 | Activate Pi in chat A, send to chat B | Chat B uses LLM, not Pi | Pi mode per-chat |
| T21 | Different sender in same group | Different sender uses LLM | Pi mode per-sender |

### 14. Phase 9: Процесс размышления виден в Telegram

| # | Send | Expected Status Transitions | Verify |
|---|------|----------------------------|--------|
| T22 | "прочитай /etc/hostname" | 🟡 "⚙ Starting Pi" → 💭 thinking text → 📖 "reading /etc/hostname" → 📄 output preview → ✅ final response | Test captures ALL transitions, verifies thinking content non-empty |
| T23 | "запусти ls /tmp" | 💭 thinking → 🔧 "running: ls /tmp" → 📄 file list → ✅ response | Tool call visible, output visible |
| T24 | "напиши hello в /tmp/pi_think_test.txt" | 💭 thinking → ✏️ "write /tmp/pi_think_test.txt" → ✅ "записано" | Edit tool visible |
| T25 | "2+2?" | 💭 thinking → ✅ "4" | Simple query shows thinking before answer |

### 15. Phase 10: Загрузка/выгрузка контекста

| # | Action | Expected | Verify |
|---|--------|----------|--------|
| T26 | Send 3 messages to LLM → `/models pi` | Pi receives ZeroClaw history (last ~100k tokens) | Pi knows what was discussed with LLM |
| T27 | After T26: "что мы обсуждали до тебя?" | Pi mentions topics from LLM conversation | History injection worked |
| T28 | `/models minimax` → `/models pi` | Pi loads saved session, NOT re-inject history | Session file loaded, context from Pi session |
| T29 | Daemon restart → `/models pi` | Pi loads saved session from disk | pi_sessions.json survives restart |

### 16. Phase 11: Переключение моделей туда-сюда

| # | Action | Expected | Verify |
|---|--------|----------|--------|
| T30 | `/models pi` → 3 messages → `/models minimax` → 3 messages → `/models pi` | Pi session restored after minimax, LLM sees [Pi] history, Pi sees old context | Full round-trip |
| T31 | `/models pi` → `/models gemini` → `/models pi` → `/models codex` → `/models pi` | Each switch: Pi stop+save → LLM → Pi restore. 3 round-trips stable | No crashes, no session loss |
| T32 | `/models pi` → `/models minimax` → check routes.json | routes.json: pi_mode=false, provider=minimax | Persistence correct after switch |
| T33 | `/models pi` → check routes.json | routes.json: pi_mode=true | Persistence correct after activate |

### 17. Phase 12: Включение/выключение Pi разными способами

| # | Action | Expected | Verify |
|---|--------|----------|--------|
| T34 | `/models pi` | "✅ Pi mode activated" | Standard activation |
| T35 | "пи, скажи ок" (prefix) | Pi responds + Pi mode activated | Prefix still works |
| T36 | "пи стоп" | "Pi mode off" | Stop via command |
| T37 | `/models minimax` (while Pi active) | Pi stopped, minimax active | Stop via model switch |
| T38 | "пи стоп" (Pi not active) | No response or ignored | No crash on double-stop |
| T39 | `/models pi` → `/models pi` (double activate) | Pi stays active, no double-spawn | Idempotent |

## Acceptance Criteria

- [ ] All unit tests pass: `cargo test --lib -- pi::`
- [ ] All integration tests pass with real Pi binary
- [ ] E2E T1-T15: 15/15 pass
- [ ] E2E T18-T19: status transitions visible in test output
- [ ] Zero timeouts in E2E (Pi on MiniMax, no rate limit)
- [ ] `cargo clippy --all-targets -- -D warnings` clean
- [ ] routes.json correctly persists pi_mode flag
- [ ] Pi process killed on `/models X` switch
- [ ] Pi process killed after 30 min idle
- [ ] Session file preserved after kill, restored on re-activate

### 18. Phase 13: Логирование Pi (daemon log)

Все Pi события должны логироваться в `/tmp/zeroclaw_daemon.log` для отладки.

| # | Action | Expected in daemon log | Verify |
|---|--------|----------------------|--------|
| T40 | `/models pi` | `Pi spawn: provider=minimax model=MiniMax-M2.7 history_key=...` | tracing::info spawn |
| T41 | Send message in Pi mode | `Pi prompt: history_key=... message_preview="..."` | tracing::info prompt start |
| T42 | Pi thinking | `Pi event: thinking_delta len=N` | tracing::debug thinking |
| T43 | Pi tool call | `Pi event: tool_start name=bash args={"command":"pwd"}` | tracing::info tool |
| T44 | Pi tool result | `Pi event: tool_end name=bash output_len=N` | tracing::debug tool output |
| T45 | Pi response | `Pi prompt completed: history_key=... response_len=N elapsed_ms=M` | tracing::info completion |
| T46 | `/models minimax` | `Pi stop: history_key=... session_file=... saved=true` | tracing::info stop |
| T47 | Idle timeout kill | `Pi idle kill: history_key=... idle_secs=1800` | tracing::info idle |
| T48 | Pi spawn error | `Pi spawn failed: error=...` | tracing::error |
| T49 | Pi prompt timeout | `Pi prompt timeout: history_key=... after=300s` | tracing::warn |

### 19. Phase 14: Telegram status message lifecycle

Всё видимое пользователю в Telegram при работе Pi.

| # | Action | Expected in Telegram | Verify |
|---|--------|---------------------|--------|
| T50 | Pi starts | New message: "⚙ Starting Pi…" | sendMessage called |
| T51 | Pi thinking | Message edited: "💭 Пользователь хочет..." | editMessageText with thinking content |
| T52 | Pi reads file | Message edited: adds "📖 /path/to/file" | editMessageText with tool |
| T53 | Pi runs bash | Message edited: adds "🔧 `cmd`" | editMessageText with command |
| T54 | Pi tool output | Message edited: adds "📄 `output preview`" | editMessageText with output |
| T55 | Pi finishes | Message edited: final clean response text | editMessageText replaces all status |
| T56 | Pi error | Message edited: "⚠️ Pi error: ..." | editMessageText with error |
| T57 | Status too long | Message truncated to ≤4096 chars, tail preserved | No Telegram API error |
| T58 | Rapid edits (debounce) | ≤1 edit per 0.8s | No Telegram rate limit 429 |

### E2E test script validates:
- daemon log contains expected tracing entries after each action
- Telegram messages show expected transitions (captured by Telethon)
- Status message ID tracks correctly (send → edit → edit → final)

## Test Commands

```bash
# Unit tests
cargo test --lib -- pi::rpc
cargo test --lib -- pi::status
cargo test --lib -- pi::session
cargo test --lib -- pi::

# Full suite
cargo test --lib

# Clippy
cargo clippy --all-targets -- -D warnings

# E2E
python3 /tmp/test_pi_persistent_e2e.py

# Manual smoke test
# In Telegram: /models pi → "2+2?" → "pwd" → /models minimax → /models pi → "what did we do?"
```
