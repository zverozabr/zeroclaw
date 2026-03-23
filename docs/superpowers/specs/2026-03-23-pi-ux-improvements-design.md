# Pi UX Improvements — Design Spec

## 3 Changes

### 1. Thinking sliding window — последние 3 строки

StatusBuilder (`src/pi/status.rs`):
- Keep max 3 thinking sections (FIFO — oldest removed when 4th arrives)
- Tool calls (📖/🔧) stay in the list alongside thinking
- On render: show only current sections
- Final edit replaces everything with clean response

User sees:
```
💭 Пользователь хочет найти дома…
💭 Использую telegram_search…
🔧 curl POST /webhook
```
→ then replaced with clean response.

### 2. Typing indicator per-chat with thread_id

TelegramNotifier (`src/pi/telegram.rs`):
- Add `start_typing()` — calls `sendChatAction` with `chat_id` + optional `message_thread_id`
- Returns `JoinHandle` — typing repeats every 4s in background
- `stop_typing(handle)` — aborts the task

In `handle_pi_bypass_if_needed` (`src/channels/mod.rs`):
- Start typing before `mgr.prompt()`
- Stop typing after response received

### 3. Multi-chat Pi E2E test

Already works architecturally. Need E2E test confirming 2 chats with Pi mode run independent Pi processes simultaneously.

## Files

| File | Change |
|------|--------|
| `src/pi/status.rs` | Max 3 sections sliding window |
| `src/pi/telegram.rs` | Add start_typing/stop_typing with thread_id |
| `src/channels/mod.rs` | Call typing around prompt |
