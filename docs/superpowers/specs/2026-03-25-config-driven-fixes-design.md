# Config-Driven Fixes — Design Spec

**Date:** 2026-03-25
**Status:** Approved
**Scope:** 7 remaining issues from session audit

## Context

Session discovered 10 problems; 3 fixed immediately (kimi→gemini switch, default_provider, temperature=1.0). This spec covers the remaining 7.

## 1. Model Temperature Overrides

**Problem:** kimi-k2.5 and kimi-k2-thinking require `temperature=1` but we send configurable temperature.

**Design:**
- New `[model_overrides]` table in config.toml:
  ```toml
  [model_overrides."kimi-k2.5"]
  temperature = 1.0

  [model_overrides."kimi-k2-thinking"]
  temperature = 1.0
  ```
- `Config` struct gains `model_overrides: HashMap<String, ModelOverride>` where `ModelOverride { temperature: Option<f64> }`
- `ReliableProvider` methods read override before passing temperature to inner provider
- Lookup: exact model name match → override temperature; miss → use caller's temperature

**Files:** `src/config/mod.rs`, `src/providers/reliable.rs`, `config.toml`

## 2. Key Hunter → Config Integration

**Problem:** 3 active moonshot keys in `keys.json` not in config. Manual process.

**Design:**
- `provider-manager` skill already has `provider_apply` tool
- Add `provider_find` integration: reads `keys.json`, filters active keys for requested provider, returns candidates
- User triggers via bot: "добавь moonshot ключи из key hunter"
- Script writes new `[api_keys]` entries to config.toml, daemon hot-reloads
- No auto-integration — user-initiated only (security boundary)

**Files:** `~/.zeroclaw/workspace/skills/provider-manager/scripts/`, `config.toml`

## 3. OC Fallback Provider in opencode.json

**Problem:** OC has single provider; if OpenAI OAuth dies, OC is down.

**Design:**
- `write_opencode_config()` adds moonshot as secondary provider in `opencode.json`:
  ```json
  {
    "provider": {
      "openai": { ... },
      "moonshot": {
        "npm": "@ai-sdk/openai-compatible",
        "options": { "apiKey": "...", "baseURL": "https://api.moonshot.cn/v1" },
        "models": { "kimi-k2-0905-preview": {} }
      }
    },
    "model": "openai/gpt-5.3-codex"
  }
  ```
- New config field: `[opencode] fallback_provider`, `fallback_model`, `fallback_base_url`, `fallback_api_key_profile`
- Config generator includes fallback provider block if configured
- User switches manually via OC `/models` command when primary fails

**Files:** `src/opencode/config.rs`, `config.toml`

## 4. OAuth Token Refresh Warning

**Problem:** OpenAI OAuth token expires 2026-04-04. No alerting.

**Design:**
- Background task in daemon startup (alongside OC watchdog): check `~/.local/share/opencode/auth.json` every hour
- Parse `expires` field (milliseconds epoch)
- If <3 days remaining: `tracing::warn!` + send Telegram message to allowlisted user
- Message: "⚠️ OpenAI OAuth token expires in N days. Run `opencode auth` to refresh."
- No auto-refresh (requires browser interaction)

**Files:** `src/daemon/mod.rs` (new background task), `src/opencode/mod.rs` (helper fn)

## 5. PR #4134 Description Update

**Problem:** PR body uses old format, not full upstream template.

**Design:**
- One-shot `gh pr edit 4134 --body "..."` with full template
- Copy format from #4645/#4648 (already done correctly)

**Files:** None (CLI command only)

## 6. Test c6 → Modernize to OpenCode

**Problem:** c6 kills `pi --mode rpc` which no longer exists. c8 already tests OC recovery.

**Design:**
- Replace c6 body: kill `opencode serve` (like c8), verify recovery + context
- Or: merge c6 into c8 (c8 already covers the scenario), mark c6 as legacy/skip
- Recommended: keep c8 as the canonical OC recovery test, rewrite c6 to test a different recovery scenario (e.g., OC returns error mid-stream)

**Files:** `~/.zeroclaw/workspace/skills/coder/tests/test_e2e.py`

## 7. Cleanup

**Problem:** `web/dist/.gitkeep` deleted by upstream, `check_minimax_*.py` untracked.

**Design:**
- `rm check_minimax_jwt_keys.py check_minimax_keys.py`
- `git checkout HEAD -- web/dist/.gitkeep` (or accept deletion if upstream intended it)
- Add `check_minimax*.py` to `.gitignore` as precaution

**Files:** repo root, `.gitignore`

## Implementation Order

1. **Cleanup** (#7) — 2 min, unblocks clean git status
2. **PR description** (#5) — 5 min, unblocks PR review
3. **Test c6** (#6) — 10 min
4. **Model temperature overrides** (#1) — 30 min, config + reliable.rs
5. **OC fallback provider** (#3) — 20 min, config.rs
6. **OAuth token warning** (#4) — 20 min, daemon background task
7. **Key hunter integration** (#2) — 15 min, provider-manager script

Total estimated: ~2 hours sequential, parallelizable to ~1 hour with agents.
