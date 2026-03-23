# Pi Admin Access — Design Spec

## Problem

Pi (coding agent) can edit files and run shell commands but cannot interact with ZeroClaw's runtime: memory, cron, config, chat history, message sending. User wants to say "пи, создай cron задачу" or "пи, что обсуждали в чате ERP" — Pi needs gateway API access.

## Solution

2 changes:

### 1. Pass gateway credentials to Pi process

In `src/pi/mod.rs` `spawn_pi()`, add env vars:

```rust
.env("ZEROCLAW_GATEWAY_URL", &gateway_url)
.env("ZEROCLAW_GATEWAY_TOKEN", &gateway_token)
.env("ZEROCLAW_WORKSPACE", &self.workspace_dir)
```

Gateway URL/token obtained from `crate::skills::get_gateway_creds_for_skill("coder")` — same mechanism trusted skills already use.

Store in PiManager:
```rust
pub struct PiManager {
    // ... existing fields ...
    gateway_url: String,
    gateway_token: String,
}
```

Init in `daemon/mod.rs`: get gateway creds after gateway starts (they're set by `set_service_token_context`).

### 2. Add GET /api/history/{sender_key} endpoint

Currently only `DELETE /api/history/{key}` exists. Add read access:

**GET `/api/history/{sender_key}`**
- Returns conversation history for a sender
- Response: `{"sender_key": "...", "messages": [{"role": "user"|"assistant", "content": "..."}]}`
- Limit: last 50 messages (MAX_CHANNEL_HISTORY)

In `src/gateway/api.rs`, add handler next to existing `delete_history`.

## What Pi can do after this

| Via Gateway API | Endpoint |
|----------------|----------|
| Read chat history | `GET /api/history/{key}` |
| Clear chat history | `DELETE /api/history/{key}` |
| Store memory | `POST /api/memory` |
| Recall memory | `GET /api/memory?query=...` |
| Create cron job | `POST /api/cron` |
| List/delete cron | `GET/DELETE /api/cron/{id}` |
| Read config | `GET /api/config` |
| Update config (hot-reload) | `PUT /api/config` |
| Send message to bot | `POST /webhook` |
| Check health | `GET /api/health` |
| List tools | `GET /api/tools` |

| Via filesystem (already works) | Path |
|-------------------------------|------|
| Create/edit skills | `~/.zeroclaw/workspace/skills/` |
| Edit SOUL.md / AGENTS.md | `~/.zeroclaw/workspace/` |
| Edit ZeroClaw source | `~/work/erp/zeroclaws/src/` |
| Build & deploy | `cargo build --release && ./dev/restart-daemon.sh` |
| Run tests | `cargo test --lib` |

## Files to modify

| File | Change |
|------|--------|
| `src/pi/mod.rs` | Add gateway_url/token fields, pass as env vars in spawn_pi |
| `src/daemon/mod.rs` | Pass gateway creds to init_pi_manager |
| `src/gateway/api.rs` | Add GET /api/history/{sender_key} handler |

### 3. Pi system prompt — русский язык

Pi should think and respond in Russian. Add `--append-system-prompt` to spawn_pi:

```rust
.args(["--append-system-prompt", "Думай и отвечай на русском языке. You are an admin agent for ZeroClaw. You have full access to the gateway API via ZEROCLAW_GATEWAY_URL and ZEROCLAW_GATEWAY_TOKEN env vars. Use curl to call API endpoints."])
```

## Testing

- Unit: verify env vars are set in spawn command
- E2E: `/models pi` → "прочитай историю чата" → Pi calls GET /api/history
- E2E: `/models pi` → "создай cron задачу" → Pi calls POST /api/cron
