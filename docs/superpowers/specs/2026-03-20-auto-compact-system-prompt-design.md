# Auto-Compact System Prompt for Small-Context Providers

**Date:** 2026-03-20
**Status:** Approved
**Scope:** `src/channels/mod.rs` only

## Problem

The channel system prompt is built once at startup (~20KB: bootstrap files + 39 tool schemas + skill instructions) and stored in `ctx.system_prompt: Arc<String>`. When a per-chat route override targets a small-context provider (Groq free tier ~8K tokens, Ollama local models), the system prompt alone exceeds the per-request token limit. The first request with empty history returns HTTP 413 / context-window-exceeded.

## Solution

Rebuild a compact system prompt per-message when the active route targets a known small-context provider.

### Insertion Point

`process_channel_message()` at line ~2751, where `base_system_prompt` is selected:

```rust
// Before (current):
let base_system_prompt = if had_prior_history {
    ctx.system_prompt.as_str().to_string()
} else {
    refreshed_new_session_system_prompt(ctx.as_ref())
};

// After (new):
let base_system_prompt = if is_small_context_provider(&route.provider) {
    build_compact_system_prompt(ctx.as_ref())
} else if had_prior_history {
    ctx.system_prompt.as_str().to_string()
} else {
    refreshed_new_session_system_prompt(ctx.as_ref())
};
```

### Components

#### 1. `is_small_context_provider(provider: &str) -> bool`

Case-insensitive check against a hardcoded list:
- `groq`
- `ollama`

~5 lines. Easily extensible by adding entries.

#### 2. Core Tools Whitelist

Tools retained in compact mode (constant array):

```
shell, memory_store, memory_query, model_switch, web_search,
http_request, read_file, write_file
```

Removed: `browser_*`, `git_*`, `gpio_*`, `arduino_*`, all hardware tools, heavy skill-generated tools.

#### 3. `build_compact_system_prompt(ctx: &ChannelRuntimeContext) -> String`

~25 lines. Performs three steps:

1. **Extract tool tuples:** Iterate `ctx.tools_registry`, collect `(name(), description())` pairs, filter against `COMPACT_CORE_TOOLS` whitelist. This mirrors the startup code at line ~5010 that builds `tool_descs: Vec<(&str, &str)>`.

2. **Reload skills:** Call `load_skills_with_config()` + `skills_to_prompt_with_mode()` with `Compact` mode (same as `refreshed_new_session_system_prompt` does for skill refresh).

3. **Build prompt:** Call `build_system_prompt_with_mode_and_autonomy()` with compact parameters:

| Parameter | Normal | Compact |
|-----------|--------|---------|
| `bootstrap_max_chars` | `None` (~20KB) | `Some(2000)` (~500 tokens, leaving ~7500 for tools+skills+conversation) |
| `skills_prompt_mode` | `Full` | `Compact` (names only) |
| `tools` | All 39 tools | Core whitelist only (~8 tools) |
| `native_tools` | Provider-dependent | Provider-dependent (see below) |

**`native_tools` handling:** NOT hardcoded to `true`. Ollama defaults to prompt-guided tool calling (commit `9b14ddab`), so `native_tools` must be resolved per-provider. The function checks `ctx.tools_registry` provider capability or defaults to `false` for safety. When `native_tools == false`, the XML `build_tool_instructions()` block IS appended (but only for the compact tool subset, keeping it small).

Target size: ~3-5KB (down from ~20KB).

**Logging:** Emits `tracing::debug!("Using compact system prompt for small-context provider: {provider}")` when activated.

### Data Flow

```
Message arrives
  → get_route_selection() → route.provider = "groq"
  → is_small_context_provider("groq") == true
  → build_compact_system_prompt() → ~3-4KB
  → build_channel_system_prompt() wraps with channel context
  → ChatMessage::system(compact_prompt)
  → Send to Groq — fits in context window
```

### Edge Cases

**Route changes mid-session:** When a user switches from groq back to minimax (or vice versa), the system prompt style changes. This is fine — the system message is rebuilt for every incoming message (line 2758: `ChatMessage::system(system_prompt)` prepended fresh). The history's prior system message is not carried forward.

### What We Do NOT Change

- Global `compact_context` config flag — untouched, that's for CLI use
- ReliableProvider fallback logic — separate issue
- System prompt caching — compact prompt is rebuilt per-message (~1ms, only for small-context chats)
- No new config options — whitelist is hardcoded

## Testing

### Unit Tests

1. **`compact_system_prompt_for_small_context_provider`** — verify that for `groq` the prompt is < 5KB, contains core tools (shell, model_switch), does not contain hardware tools, and bootstrap section is under 2000 chars
2. **`full_system_prompt_for_normal_provider`** — verify that for `minimax` the prompt uses the full cached version
3. **`compact_prompt_no_xml_for_native_tools_provider`** — verify that for `groq` (native tools) the prompt does NOT contain `<tool_call>` XML instructions
4. **`compact_prompt_has_xml_for_ollama`** — verify that for `ollama` (prompt-guided) the prompt DOES contain XML tool instructions for the compact tool subset

### E2E Validation

1. Switch to groq via model_switch in Telegram
2. Bot responds without 413 error
3. Verify core tools work (e.g., ask bot to remember something → memory_store works)
4. Switch back to default → full prompt restored

## Files Changed

| File | Change |
|------|--------|
| `src/channels/mod.rs` | Add `is_small_context_provider()`, `COMPACT_CORE_TOOLS`, `build_compact_system_prompt()`, modify `base_system_prompt` selection |
