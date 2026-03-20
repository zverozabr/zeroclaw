# Auto-Compact System Prompt Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Automatically rebuild a compact system prompt when a per-chat route override targets a small-context provider (Groq, Ollama).

**Architecture:** Add three small functions to `src/channels/mod.rs`: a provider classifier, a core-tools whitelist constant, and a compact prompt builder. Insert a single branch in `process_channel_message()` before the existing system prompt selection.

**Tech Stack:** Rust, existing `build_system_prompt_with_mode_and_autonomy()` infrastructure.

**Spec:** `docs/superpowers/specs/2026-03-20-auto-compact-system-prompt-design.md`

---

### Task 1: Add `is_small_context_provider` function and `COMPACT_CORE_TOOLS` constant

**Files:**
- Modify: `src/channels/mod.rs` (add after line ~1232, near `load_route_overrides`)

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)]` block in `src/channels/mod.rs`:

```rust
#[test]
fn is_small_context_provider_matches_known() {
    assert!(is_small_context_provider("groq"));
    assert!(is_small_context_provider("Groq"));
    assert!(is_small_context_provider("OLLAMA"));
    assert!(is_small_context_provider("ollama"));
    assert!(!is_small_context_provider("minimax"));
    assert!(!is_small_context_provider("openai"));
    assert!(!is_small_context_provider("anthropic"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib channels::tests::is_small_context_provider_matches_known`
Expected: FAIL — `is_small_context_provider` not found

- [ ] **Step 3: Write the implementation**

Add after `load_route_overrides` (line ~1232):

```rust
/// Tools retained in the compact system prompt for small-context providers.
const COMPACT_CORE_TOOLS: &[&str] = &[
    "shell",
    "file_read",
    "file_write",
    "memory_store",
    "memory_recall",
    "memory_forget",
    "model_switch",
    "web_search",
    "http_request",
    "read_skill",
];

/// Returns `true` for providers known to have small context windows
/// (e.g. Groq free tier ~8K tokens, Ollama local models).
fn is_small_context_provider(provider: &str) -> bool {
    matches!(
        provider.to_ascii_lowercase().as_str(),
        "groq" | "ollama"
    )
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib channels::tests::is_small_context_provider_matches_known`
Expected: PASS

- [ ] **Step 5: Run clippy**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: No warnings

- [ ] **Step 6: Commit**

```bash
git add src/channels/mod.rs
git commit -m "feat(channels): add is_small_context_provider + COMPACT_CORE_TOOLS"
```

---

### Task 2: Add `build_compact_system_prompt` function

**Files:**
- Modify: `src/channels/mod.rs` (add after `is_small_context_provider`)

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)]` block:

```rust
#[test]
fn compact_system_prompt_is_small_and_has_core_tools() {
    let workspace = std::env::temp_dir();
    let config = crate::config::Config::default();

    // Build a compact prompt directly
    let tools_for_prompt: Vec<(&str, &str)> = vec![
        ("shell", "Execute terminal commands."),
        ("file_read", "Read file contents."),
        ("file_write", "Write file contents."),
        ("memory_store", "Save to memory."),
        ("memory_recall", "Search memory."),
        ("memory_forget", "Delete a memory entry."),
        ("model_switch", "Switch model."),
        ("web_search", "Web search."),
        ("http_request", "HTTP requests."),
        ("read_skill", "Load skill source."),
    ];

    let prompt = build_system_prompt_with_mode_and_autonomy(
        &workspace,
        "test-model",
        &tools_for_prompt,
        &[],
        Some(&config.identity),
        Some(2000),
        Some(&config.autonomy),
        true,  // native tools
        crate::config::SkillsPromptInjectionMode::Compact,
    );

    // Must be under 5KB
    assert!(
        prompt.len() < 5000,
        "Compact prompt too large: {} bytes",
        prompt.len()
    );
    // Must contain core tools
    assert!(prompt.contains("shell"));
    assert!(prompt.contains("model_switch"));
    // Must NOT contain hardware tools
    assert!(!prompt.contains("gpio_read"));
    assert!(!prompt.contains("arduino_upload"));
}
```

- [ ] **Step 2: Run test to verify it passes** (this tests existing infra, should pass)

Run: `cargo test --lib channels::tests::compact_system_prompt_is_small_and_has_core_tools`
Expected: PASS (validates the compact parameters produce a small prompt)

- [ ] **Step 3: Write `build_compact_system_prompt`**

Add after `is_small_context_provider`:

```rust
/// Builds a compact system prompt for small-context providers.
///
/// Filters tools to [`COMPACT_CORE_TOOLS`], uses compact skill injection,
/// and limits bootstrap files to 2 KB.
fn build_compact_system_prompt(
    ctx: &ChannelRuntimeContext,
    native_tools: bool,
) -> String {
    tracing::debug!(
        "Using compact system prompt for small-context provider"
    );

    // 1. Extract tool descriptions, keeping only core tools
    let all_descs: Vec<(String, String)> = ctx
        .tools_registry
        .iter()
        .map(|t| (t.name().to_string(), t.description().to_string()))
        .collect();
    let tool_descs: Vec<(&str, &str)> = all_descs
        .iter()
        .filter(|(name, _)| COMPACT_CORE_TOOLS.contains(&name.as_str()))
        .map(|(n, d)| (n.as_str(), d.as_str()))
        .collect();

    // 2. Reload skills in compact mode
    let skills = crate::skills::load_skills_with_config(
        ctx.workspace_dir.as_ref(),
        ctx.prompt_config.as_ref(),
    );

    // 3. Build prompt with aggressive limits
    let mut prompt = build_system_prompt_with_mode_and_autonomy(
        ctx.workspace_dir.as_ref(),
        ctx.model.as_str(),
        &tool_descs,
        &skills,
        Some(&ctx.prompt_config.identity),
        Some(2000), // ~500 tokens bootstrap budget
        Some(&ctx.prompt_config.autonomy),
        native_tools,
        crate::config::SkillsPromptInjectionMode::Compact,
    );

    // 4. Append XML tool instructions only when provider lacks native tool calling.
    if !native_tools {
        prompt.push_str(&build_compact_tool_xml(ctx.tools_registry.as_ref()));
    }

    prompt
}
```

- [ ] **Step 4: Add `build_compact_tool_xml` helper** (testable, used by `build_compact_system_prompt`)

Add before `build_compact_system_prompt`:

```rust
/// Builds XML tool-calling instructions for only the compact core tools.
/// Extracted as a separate function so it can be unit-tested without
/// constructing a full `ChannelRuntimeContext`.
fn build_compact_tool_xml(tools_registry: &[Box<dyn Tool>]) -> String {
    use std::fmt::Write;
    let mut xml = String::new();
    xml.push_str("\n## Tool Use Protocol\n\n");
    xml.push_str("To use a tool, wrap a JSON object in <tool_call></tool_call> tags:\n\n");
    xml.push_str("```\n<tool_call>\n{\"name\": \"tool_name\", \"arguments\": {\"param\": \"value\"}}\n</tool_call>\n```\n\n");
    xml.push_str("CRITICAL: Output actual <tool_call> tags—never describe steps or give examples.\n\n");
    xml.push_str("### Available Tools\n\n");
    for tool in tools_registry {
        if COMPACT_CORE_TOOLS.contains(&tool.name()) {
            let _ = writeln!(
                xml,
                "**{}**: {}\nParameters: `{}`\n",
                tool.name(),
                tool.description(),
                tool.parameters_schema()
            );
        }
    }
    xml
}
```

- [ ] **Step 5: Run clippy**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: No warnings

- [ ] **Step 6: Commit**

```bash
git add src/channels/mod.rs
git commit -m "feat(channels): add build_compact_system_prompt for small-context providers"
```

---

### Task 3: Wire compact prompt into `process_channel_message`

**Files:**
- Modify: `src/channels/mod.rs:2751` (the `base_system_prompt` selection)

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)]` block:

```rust
#[test]
fn compact_prompt_no_xml_for_native_tools_provider() {
    let workspace = std::env::temp_dir();
    let config = crate::config::Config::default();
    let tools: Vec<(&str, &str)> = vec![("shell", "Execute commands.")];
    let prompt = build_system_prompt_with_mode_and_autonomy(
        &workspace,
        "test",
        &tools,
        &[],
        Some(&config.identity),
        Some(2000),
        Some(&config.autonomy),
        true,  // native tools = true (Groq)
        crate::config::SkillsPromptInjectionMode::Compact,
    );
    // Native tools provider: no XML tool_call instructions in prompt
    assert!(
        !prompt.contains("<tool_call>"),
        "Native tools prompt should not contain XML tool_call instructions"
    );
}

#[test]
fn compact_tool_xml_contains_only_core_tools() {
    use crate::security::SecurityPolicy;
    let security = std::sync::Arc::new(SecurityPolicy::from_config(
        &crate::config::AutonomyConfig::default(),
        std::path::Path::new("/tmp"),
    ));
    let tools = crate::tools::default_tools(security);
    let xml = build_compact_tool_xml(&tools);

    // Must contain XML tool_call protocol
    assert!(xml.contains("<tool_call>"), "Should contain XML tool_call tags");
    assert!(xml.contains("## Tool Use Protocol"), "Should contain protocol header");

    // Must contain core tools
    assert!(xml.contains("**shell**"), "Should contain shell tool");
    assert!(xml.contains("**memory_store**"), "Should contain memory_store tool");

    // Must NOT contain non-core tools (hardware, browser, git)
    assert!(!xml.contains("**gpio_read**"), "Should NOT contain gpio_read");
    assert!(!xml.contains("**browser_open**"), "Should NOT contain browser_open");
    assert!(!xml.contains("**git_"), "Should NOT contain git tools");
}
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test --lib channels::tests::compact_prompt`
Expected: PASS

- [ ] **Step 3: Modify `process_channel_message` at line ~2751**

Replace:

```rust
    let base_system_prompt = if had_prior_history {
        ctx.system_prompt.as_str().to_string()
    } else {
        refreshed_new_session_system_prompt(ctx.as_ref())
    };
```

With:

```rust
    let base_system_prompt = if is_small_context_provider(&route.provider) {
        build_compact_system_prompt(ctx.as_ref(), active_provider.supports_native_tools())
    } else if had_prior_history {
        ctx.system_prompt.as_str().to_string()
    } else {
        refreshed_new_session_system_prompt(ctx.as_ref())
    };
```

Note: `route` is available from line 2584, `active_provider` from line 2611. Both are in scope before line 2751.

- [ ] **Step 4: Run full test suite**

Run: `cargo test --lib channels`
Expected: All existing tests + new tests PASS

- [ ] **Step 5: Run clippy and fmt**

Run: `cargo fmt --all -- --check && cargo clippy --all-targets -- -D warnings`
Expected: Clean

- [ ] **Step 6: Commit**

```bash
git add src/channels/mod.rs
git commit -m "feat(channels): auto-compact system prompt for small-context providers

When per-chat route override targets groq or ollama, rebuild the system
prompt with 2KB bootstrap limit, compact skills, and core-tools-only
whitelist (~3-5KB instead of ~20KB)."
```

---

### Task 4: Build and E2E validation

**Files:**
- No new files

- [ ] **Step 1: Full build**

Run: `cargo build --release`
Expected: Compiles without errors

- [ ] **Step 2: Restart daemon**

Run: `./dev/restart-daemon.sh`
Expected: Daemon starts successfully

- [ ] **Step 3: E2E test — switch to groq and get a response**

Write and run a Telethon script that:
1. Sends "переключи на groq llama-3.3-70b-versatile" to @zGsR_bot
2. Waits for confirmation reply (should mention model_switch)
3. Sends "привет, какая ты модель?"
4. Waits for reply — should come from groq WITHOUT 413 error

Use session: `~/.zeroclaw/workspace/skills/telegram-reader/.session/zverozabr_session`
API_ID: `38309428`, API_HASH: `1f9a006d55531cfd387246cd0fff83f8`
Venv: `~/.zeroclaw/workspace/.venv/bin/python3`

- [ ] **Step 4: Verify daemon log**

Run: `grep -E "compact system prompt|small-context" /tmp/zeroclaw_daemon.log | tail -5`
Expected: See "Using compact system prompt for small-context provider" log line

- [ ] **Step 5: Switch back to default**

Send "переключи обратно на minimax" via Telethon, verify bot responds with full prompt.

- [ ] **Step 6: Check routes.json**

Run: `cat ~/.zeroclaw/workspace/routes.json`
Expected: Should show/not show groq override depending on current state
