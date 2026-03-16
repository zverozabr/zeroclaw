# Interactive Model Switcher

## Problem

Current `/models` and `/model` commands have poor UX:
- `/models` dumps all ~25 providers including unconfigured ones
- `/models <provider>` switches provider but keeps stale model (may be incompatible)
- No way to see available models before picking one
- OAuth-only providers (gemini, openai-codex) can't fetch model lists via API
- No concept of "default model per provider"

## Solution

Replace `/models` + `/model` with a single interactive `/models` command using numbered menus and stateless selection.

## Command: `/models`

Aliases: `/model`

### Flow

```
User: /models
Bot:
  🔌 Provider: gemini | Model: gemini-3-flash-preview

  1. gemini ✓
  2. openai-codex

  Reply with number to switch provider:

User: 1
Bot:
  📦 gemini models:

  1. gemini-3-flash-preview ★
  2. gemini-3.1-pro-preview
  3. gemini-3.1-flash-lite-preview
  4. gemini-3.1-pro-preview-customtools
  5. gemini-2.5-flash
  6. gemini-2.5-pro
  ── fetched ──
  7. gemini-2.0-flash
  8. gemini-1.5-pro

  Reply with number, or "default N" to set default:

User: 2
Bot:
  ✅ Switched to gemini-3.1-pro-preview (gemini)

User: default 4
Bot:
  ✅ Default model for gemini set to gemini-3.1-pro-preview-customtools
```

### Quick hint shortcut (preserved)

`/models flash` — if argument is not a number and not `default`, try as model_routes hint. Switches provider+model in one step.

## Provider List

Only providers that appear in:
- `default_provider`
- `model_routes[].provider`
- `reliability.fallback_providers` (provider part before `:`)

Deduplicated, sorted. Not all 25 registered providers.

## Model List: Merged Sources

Two sources merged with deduplication:

1. **Hardcoded (from model_routes)** — models explicitly configured in `[[model_routes]]` for this provider. Always present, shown first.
2. **Fetched (from cache)** — additional models from `models_cache.json` that are NOT in routes. Shown below a `── fetched ──` separator. If empty, no separator.

This ensures OAuth-only providers always show their configured models even when ListModels API is unavailable.

## Default Model Per Provider

Persisted in `~/.zeroclaw/workspace/state/provider_defaults.json`:

```json
{
  "gemini": "gemini-3-flash-preview",
  "openai-codex": "gpt-5.1-codex"
}
```

**Resolution order when switching provider:**
1. Explicit default from `provider_defaults.json`
2. First model from `model_routes` for this provider
3. First model from cache

When switching provider, model auto-sets to the resolved default.

Marked with ★ in model list.

**Concurrent writes:** `provider_defaults.json` is written infrequently (only on explicit `default N`). Last-write-wins is acceptable — no locking needed.

## Pending Selection State

Per-sender, in-memory (not persisted). Keyed by `(channel_name, sender_id)` — same key as `conversation_history_key()`. Each sender has independent selection state; group chats don't interfere across users.

```rust
struct PendingSelectionEntry {
    selection: PendingSelectionKind,
    created_at: Instant,
}

enum PendingSelectionKind {
    AwaitingProvider(Vec<String>),           // shown provider names
    AwaitingModel(String, Vec<String>),      // provider, shown model names
}
```

**Number interception:** bare numbers (`1`, `2`, etc.) and `default N` are intercepted ONLY when pending_selection is active and not expired. Otherwise passed to LLM as normal messages.

**Bare words during pending selection:** any non-number, non-`default` input resets the pending state. Hint shortcuts require the `/models` prefix (e.g. `/models flash`), bare `flash` during pending selection resets state and goes to LLM.

**Reset conditions:**
- Any non-number, non-`default` message
- Lazy expiry: 60 seconds since `created_at` (checked on next message, no background timer)
- `/new` command

## Auto-refresh

Fire-and-forget `refresh_models_quiet()` triggered when model list is shown for a provider (same as current ShowModel behavior). User sees current cache immediately; next `/models` shows fresh data if refresh succeeded.

## Files to Modify

| File | Change |
|---|---|
| `src/channels/mod.rs` | Replace `ShowProviders/SetProvider/ShowModel/SetModel` with `Models`. Add `PendingSelection` state per sender. New handler logic. New `build_provider_list()`, `build_model_list()`. Remove `build_models_help_response()`, `build_providers_help_response()`, `load_cached_model_preview()`. |
| `src/channels/mod.rs` (parser) | `/models`, `/model` → `Models`. Intercept bare numbers and `default N` when pending. |
| `src/onboard/wizard.rs` | Add `load_provider_defaults()`, `save_provider_default()` for `provider_defaults.json`. |
| `src/onboard/mod.rs` | Re-export new functions. |

**Not touched:** daemon refresh worker, `refresh_models_quiet`, config schema, model_routes config.

## Verification

1. `cargo clippy --all-targets -- -D warnings`
2. `cargo test`
3. E2E in Telegram:
   - `/models` → numbered provider list
   - Reply `1` → provider switches, model list shown
   - Reply `2` → model switches, confirmation
   - `default 3` → default saved, persists across restart
   - `/models flash` → hint shortcut still works
   - Non-number message after `/models` → goes to LLM, not intercepted
   - Restart daemon → default model preserved
