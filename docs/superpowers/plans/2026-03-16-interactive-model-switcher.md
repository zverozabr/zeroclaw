# Interactive Model Switcher Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace `/models` + `/model` with a single interactive `/models` command using numbered menus, default-model-per-provider persistence, and merged hardcoded+fetched model lists.

**Architecture:** Three layers — (1) provider defaults persistence in `wizard.rs`, (2) pending selection state + model list builder in `channels/mod.rs`, (3) rewritten command parser and handler in `channels/mod.rs`. Each layer is independently testable.

**Tech Stack:** Rust, tokio, serde_json, existing `Config`/`ModelRouteConfig` types.

**Spec:** `docs/superpowers/specs/2026-03-16-interactive-model-switcher.md`

---

## Chunk 1: Provider Defaults Persistence

### Task 1: Add provider_defaults load/save to wizard.rs

**Files:**
- Modify: `src/onboard/wizard.rs` (add after `save_model_cache_state`, ~line 1626)
- Modify: `src/onboard/mod.rs` (add re-exports)

- [ ] **Step 1: Write tests for provider defaults**

Add to the `#[cfg(test)] mod tests` block at the bottom of `wizard.rs`:

```rust
#[tokio::test]
async fn provider_defaults_roundtrip() {
    let tmp = tempfile::TempDir::new().unwrap();
    let ws = tmp.path().to_path_buf();

    // Initially empty
    let defaults = load_provider_defaults(&ws).await.unwrap();
    assert!(defaults.is_empty());

    // Save one
    save_provider_default(&ws, "gemini", "gemini-3-flash-preview")
        .await
        .unwrap();
    let defaults = load_provider_defaults(&ws).await.unwrap();
    assert_eq!(
        defaults.get("gemini").map(String::as_str),
        Some("gemini-3-flash-preview")
    );

    // Overwrite
    save_provider_default(&ws, "gemini", "gemini-2.5-pro")
        .await
        .unwrap();
    let defaults = load_provider_defaults(&ws).await.unwrap();
    assert_eq!(
        defaults.get("gemini").map(String::as_str),
        Some("gemini-2.5-pro")
    );

    // Multiple providers
    save_provider_default(&ws, "openai-codex", "gpt-5.1-codex")
        .await
        .unwrap();
    let defaults = load_provider_defaults(&ws).await.unwrap();
    assert_eq!(defaults.len(), 2);
}

#[tokio::test]
async fn resolve_default_model_uses_explicit_then_routes_then_cache() {
    let tmp = tempfile::TempDir::new().unwrap();
    let ws = tmp.path().to_path_buf();

    let routes = vec![crate::config::ModelRouteConfig {
        hint: "flash".into(),
        provider: "gemini".into(),
        model: "gemini-3-flash-preview".into(),
        api_key: None,
    }];

    // No explicit default → falls back to first route
    let model = resolve_default_model_for_provider(&ws, "gemini", &routes).await;
    assert_eq!(model, Some("gemini-3-flash-preview".to_string()));

    // Explicit default overrides route
    save_provider_default(&ws, "gemini", "gemini-2.5-pro")
        .await
        .unwrap();
    let model = resolve_default_model_for_provider(&ws, "gemini", &routes).await;
    assert_eq!(model, Some("gemini-2.5-pro".to_string()));

    // Provider with no routes and no default → None
    let model = resolve_default_model_for_provider(&ws, "unknown", &[]).await;
    assert_eq!(model, None);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test provider_defaults_roundtrip -- --nocapture`
Expected: FAIL — `load_provider_defaults` not found.

- [ ] **Step 3: Implement provider defaults functions**

Add to `wizard.rs` after `save_model_cache_state` (~line 1626):

```rust
const PROVIDER_DEFAULTS_FILE: &str = "provider_defaults.json";

fn provider_defaults_path(workspace_dir: &Path) -> PathBuf {
    workspace_dir.join("state").join(PROVIDER_DEFAULTS_FILE)
}

pub async fn load_provider_defaults(
    workspace_dir: &Path,
) -> Result<HashMap<String, String>> {
    let path = provider_defaults_path(workspace_dir);
    if !path.exists() {
        return Ok(HashMap::new());
    }

    let raw = fs::read_to_string(&path)
        .await
        .with_context(|| format!("failed to read provider defaults at {}", path.display()))?;

    match serde_json::from_str::<HashMap<String, String>>(&raw) {
        Ok(map) => Ok(map),
        Err(_) => Ok(HashMap::new()),
    }
}

pub async fn save_provider_default(
    workspace_dir: &Path,
    provider: &str,
    model: &str,
) -> Result<()> {
    let path = provider_defaults_path(workspace_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await?;
    }

    let mut defaults = load_provider_defaults(workspace_dir).await?;
    defaults.insert(provider.to_string(), model.to_string());

    let json = serde_json::to_vec_pretty(&defaults)
        .context("failed to serialize provider defaults")?;
    fs::write(&path, json)
        .await
        .with_context(|| format!("failed to write provider defaults at {}", path.display()))?;

    Ok(())
}

/// Resolve the default model for a provider.
/// Priority: explicit default > first model_route > first cached model > None.
pub async fn resolve_default_model_for_provider(
    workspace_dir: &Path,
    provider: &str,
    model_routes: &[crate::config::ModelRouteConfig],
) -> Option<String> {
    // 1. Explicit default
    if let Ok(defaults) = load_provider_defaults(workspace_dir).await {
        if let Some(model) = defaults.get(provider) {
            return Some(model.clone());
        }
    }

    // 2. First model_route for this provider
    if let Some(route) = model_routes.iter().find(|r| r.provider == provider) {
        return Some(route.model.clone());
    }

    // 3. First model from cache
    let cache_path = workspace_dir.join("state").join(MODEL_CACHE_FILE);
    if let Ok(raw) = tokio::fs::read_to_string(&cache_path).await {
        if let Ok(state) = serde_json::from_str::<ModelCacheState>(&raw) {
            if let Some(entry) = state.entries.iter().find(|e| e.provider == provider) {
                return entry.models.first().cloned();
            }
        }
    }

    None
}
```

Add `use std::collections::HashMap;` to imports at top of wizard.rs (only `BTreeMap` is currently imported, `HashMap` is needed for `load_provider_defaults`).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test provider_defaults_roundtrip resolve_default_model -- --nocapture`
Expected: PASS

- [ ] **Step 5: Add re-exports to onboard/mod.rs**

In `src/onboard/mod.rs`, add to the `pub use wizard::` block:

```rust
pub use wizard::{
    load_provider_defaults, refresh_models_quiet, resolve_default_model_for_provider,
    run_channels_repair_wizard, run_models_list, run_models_refresh, run_models_refresh_all,
    run_models_set, run_models_status, run_quick_setup, save_provider_default,
    MODEL_CACHE_TTL_SECS,
};
```

Update the reexport test to add:
```rust
assert_reexport_exists(load_provider_defaults);
assert_reexport_exists(save_provider_default);
assert_reexport_exists(resolve_default_model_for_provider);
```

- [ ] **Step 6: Run clippy + tests**

Run: `cargo clippy --all-targets -- -D warnings && cargo test`
Expected: all pass

- [ ] **Step 7: Commit**

```bash
git add src/onboard/wizard.rs src/onboard/mod.rs
git commit -m "feat: add provider_defaults persistence (load/save/resolve)"
```

---

## Chunk 2: Pending Selection State + Model List Builder

### Task 2: Add PendingSelection type and model list builder

**Files:**
- Modify: `src/channels/mod.rs`

- [ ] **Step 1: Write tests for model list building**

Add to the `#[cfg(test)] mod tests` block in `channels/mod.rs`:

```rust
#[test]
fn build_merged_model_list_deduplicates_routes_and_cache() {
    let routes = vec![
        crate::config::ModelRouteConfig {
            hint: "flash".into(),
            provider: "gemini".into(),
            model: "gemini-3-flash-preview".into(),
            api_key: None,
        },
        crate::config::ModelRouteConfig {
            hint: "pro".into(),
            provider: "gemini".into(),
            model: "gemini-2.5-pro".into(),
            api_key: None,
        },
    ];
    let cached = vec![
        "gemini-2.5-pro".to_string(),       // duplicate — should be skipped
        "gemini-2.0-flash".to_string(),      // new — should appear in fetched
    ];

    let (hardcoded, fetched) = build_merged_model_list("gemini", &routes, &cached);
    assert_eq!(hardcoded, vec!["gemini-3-flash-preview", "gemini-2.5-pro"]);
    assert_eq!(fetched, vec!["gemini-2.0-flash"]);
}

#[test]
fn build_merged_model_list_empty_cache() {
    let routes = vec![crate::config::ModelRouteConfig {
        hint: "flash".into(),
        provider: "gemini".into(),
        model: "gemini-3-flash-preview".into(),
        api_key: None,
    }];

    let (hardcoded, fetched) = build_merged_model_list("gemini", &routes, &[]);
    assert_eq!(hardcoded, vec!["gemini-3-flash-preview"]);
    assert!(fetched.is_empty());
}

#[test]
fn build_merged_model_list_no_routes() {
    let cached = vec!["model-a".to_string(), "model-b".to_string()];
    let (hardcoded, fetched) = build_merged_model_list("custom", &[], &cached);
    assert!(hardcoded.is_empty());
    assert_eq!(fetched, vec!["model-a", "model-b"]);
}

#[test]
fn collect_active_providers_deduplicates_and_sorts() {
    let routes = vec![
        crate::config::ModelRouteConfig {
            hint: "flash".into(),
            provider: "gemini".into(),
            model: "m1".into(),
            api_key: None,
        },
        crate::config::ModelRouteConfig {
            hint: "codex".into(),
            provider: "openai-codex".into(),
            model: "m2".into(),
            api_key: None,
        },
    ];
    let fallback = vec!["gemini:gemini-1".to_string(), "gemini:gemini-2".to_string()];
    let providers = collect_active_providers(Some("gemini"), &routes, &fallback);
    assert_eq!(providers, vec!["gemini", "openai-codex"]);
}

#[test]
fn pending_selection_expires_after_timeout() {
    let entry = PendingSelectionEntry {
        kind: PendingSelectionKind::AwaitingProvider(vec!["gemini".into()]),
        created_at: Instant::now() - Duration::from_secs(61),
    };
    assert!(entry.is_expired());

    let fresh = PendingSelectionEntry {
        kind: PendingSelectionKind::AwaitingProvider(vec!["gemini".into()]),
        created_at: Instant::now(),
    };
    assert!(!fresh.is_expired());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test build_merged_model_list -- --nocapture`
Expected: FAIL — function not found.

- [ ] **Step 3: Add PendingSelection types**

Add after the `RouteSelectionMap` type alias (~line 234):

```rust
type PendingSelectionMap = Arc<Mutex<HashMap<String, PendingSelectionEntry>>>;

const PENDING_SELECTION_TIMEOUT_SECS: u64 = 60;

#[derive(Debug, Clone)]
struct PendingSelectionEntry {
    kind: PendingSelectionKind,
    created_at: Instant,
}

#[derive(Debug, Clone)]
enum PendingSelectionKind {
    AwaitingProvider(Vec<String>),
    AwaitingModel(String, Vec<String>),
}

impl PendingSelectionEntry {
    fn is_expired(&self) -> bool {
        self.created_at.elapsed() > Duration::from_secs(PENDING_SELECTION_TIMEOUT_SECS)
    }
}
```

Add `pending_selections: PendingSelectionMap,` to `ChannelRuntimeContext` struct (~line 394, after `route_overrides`).

- [ ] **Step 4: Add collect_active_providers and build_merged_model_list**

Add after `build_providers_help_response` (~line 1344):

```rust
fn collect_active_providers(
    default_provider: Option<&str>,
    model_routes: &[crate::config::ModelRouteConfig],
    fallback_providers: &[String],
) -> Vec<String> {
    let mut providers: Vec<String> = Vec::new();

    if let Some(p) = default_provider {
        providers.push(p.to_string());
    }

    for entry in fallback_providers {
        if let Some(p) = entry.split(':').next() {
            let p = p.trim().to_string();
            if !p.is_empty() {
                providers.push(p);
            }
        }
    }

    for route in model_routes {
        providers.push(route.provider.clone());
    }

    providers.sort();
    providers.dedup();
    providers
}

/// Split models into (hardcoded_from_routes, fetched_only).
/// Hardcoded = models from model_routes for this provider.
/// Fetched = cached models NOT in routes (deduplicated).
fn build_merged_model_list(
    provider: &str,
    model_routes: &[crate::config::ModelRouteConfig],
    cached_models: &[String],
) -> (Vec<String>, Vec<String>) {
    let hardcoded: Vec<String> = model_routes
        .iter()
        .filter(|r| r.provider == provider)
        .map(|r| r.model.clone())
        .collect();

    let hardcoded_set: HashSet<&str> = hardcoded.iter().map(String::as_str).collect();

    let fetched: Vec<String> = cached_models
        .iter()
        .filter(|m| !hardcoded_set.contains(m.as_str()))
        .cloned()
        .collect();

    (hardcoded, fetched)
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test build_merged_model_list collect_active_providers pending_selection_expires -- --nocapture`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add src/channels/mod.rs
git commit -m "feat: add PendingSelection state and model list builder"
```

---

## Chunk 3: Rewrite Command Parser and Handler

### Task 3: Replace old commands with new Models command

**Files:**
- Modify: `src/channels/mod.rs`

- [ ] **Step 1: Write tests for new parser**

Add to tests in `channels/mod.rs`:

```rust
#[test]
fn parse_models_command() {
    assert_eq!(
        parse_runtime_command("telegram", "/models"),
        Some(ChannelRuntimeCommand::Models(None))
    );
    assert_eq!(
        parse_runtime_command("telegram", "/model"),
        Some(ChannelRuntimeCommand::Models(None))
    );
    assert_eq!(
        parse_runtime_command("telegram", "/models flash"),
        Some(ChannelRuntimeCommand::Models(Some("flash".to_string())))
    );
}

#[test]
fn parse_new_session_unchanged() {
    assert_eq!(
        parse_runtime_command("telegram", "/new"),
        Some(ChannelRuntimeCommand::NewSession)
    );
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test parse_models_command -- --nocapture`
Expected: FAIL — `Models` variant not found.

- [ ] **Step 3: Replace enum variants**

Change `ChannelRuntimeCommand` (~line 296):

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
enum ChannelRuntimeCommand {
    Models(Option<String>),  // arg: hint, number, or None
    NewSession,
}
```

Remove old variants `ShowProviders`, `SetProvider`, `ShowModel`, `SetModel`.

- [ ] **Step 4: Rewrite parse_runtime_command**

Replace the body of `parse_runtime_command` (~line 769-789):

```rust
fn parse_runtime_command(channel_name: &str, content: &str) -> Option<ChannelRuntimeCommand> {
    if !supports_runtime_model_switch(channel_name) {
        return None;
    }

    let trimmed = content.trim();
    if !trimmed.starts_with('/') {
        return None;
    }

    let mut parts = trimmed.split_whitespace();
    let command_token = parts.next()?;
    let base_command = command_token
        .split('@')
        .next()
        .unwrap_or(command_token)
        .to_ascii_lowercase();

    match base_command.as_str() {
        "/models" | "/model" => {
            let arg = parts.collect::<Vec<_>>().join(" ").trim().to_string();
            if arg.is_empty() {
                Some(ChannelRuntimeCommand::Models(None))
            } else {
                Some(ChannelRuntimeCommand::Models(Some(arg)))
            }
        }
        "/new" => Some(ChannelRuntimeCommand::NewSession),
        _ => None,
    }
}
```

- [ ] **Step 5: Run parser tests**

Run: `cargo test parse_models_command parse_new_session -- --nocapture`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add src/channels/mod.rs
git commit -m "refactor: replace ShowProviders/SetProvider/ShowModel/SetModel with Models"
```

### Task 4: Rewrite handler and response builders

**Files:**
- Modify: `src/channels/mod.rs`

- [ ] **Step 1: Add pending_selections field initialization**

Find where `ChannelRuntimeContext` is constructed (~line 4116) and add:

```rust
pending_selections: Arc::new(Mutex::new(HashMap::new())),
```

Do the same in all test helper functions that build `ChannelRuntimeContext` (search for `model_routes: Arc::new(Vec::new())` — there are ~7 places).

- [ ] **Step 2: Add response builder functions**

Replace `build_models_help_response` and `build_providers_help_response` with:

```rust
fn build_provider_list_response(
    current: &ChannelRouteSelection,
    active_providers: &[String],
) -> String {
    let mut response = format!(
        "\u{1f50c} Provider: {} | Model: {}\n\n",
        current.provider, current.model
    );

    for (i, provider) in active_providers.iter().enumerate() {
        let marker = if *provider == current.provider {
            " \u{2713}"
        } else {
            ""
        };
        let _ = writeln!(response, "{}. {}{}", i + 1, provider, marker);
    }

    response.push_str("\nReply with number to switch provider:");
    response
}

fn build_model_list_response(
    provider: &str,
    hardcoded: &[String],
    fetched: &[String],
    default_model: Option<&str>,
) -> String {
    let mut response = format!("\u{1f4e6} {} models:\n\n", provider);
    let mut index = 1usize;

    for model in hardcoded {
        let marker = if default_model == Some(model.as_str()) {
            " \u{2605}"
        } else {
            ""
        };
        let _ = writeln!(response, "{}. {}{}", index, model, marker);
        index += 1;
    }

    if !fetched.is_empty() {
        response.push_str("\u{2500}\u{2500} fetched \u{2500}\u{2500}\n");
        for model in fetched {
            let marker = if default_model == Some(model.as_str()) {
                " \u{2605}"
            } else {
                ""
            };
            let _ = writeln!(response, "{}. {}{}", index, model, marker);
            index += 1;
        }
    }

    response.push_str("\nReply with number, or \"default N\" to set default:");
    response
}
```

- [ ] **Step 3: Load cached models for model list (replace load_cached_model_preview)**

Replace `load_cached_model_preview` with a version that returns ALL models (no limit):

```rust
fn load_all_cached_models(workspace_dir: &Path, provider_name: &str) -> Vec<String> {
    let cache_path = workspace_dir.join("state").join(MODEL_CACHE_FILE);
    let Ok(raw) = std::fs::read_to_string(cache_path) else {
        return Vec::new();
    };
    let Ok(state) = serde_json::from_str::<ModelCacheState>(&raw) else {
        return Vec::new();
    };

    state
        .entries
        .into_iter()
        .find(|entry| entry.provider == provider_name)
        .map(|entry| entry.models)
        .unwrap_or_default()
}
```

Remove old `load_cached_model_preview`.

- [ ] **Step 4: Rewrite handle_runtime_command_if_needed**

Replace the entire body of `handle_runtime_command_if_needed`:

```rust
async fn handle_runtime_command_if_needed(
    ctx: &ChannelRuntimeContext,
    msg: &traits::ChannelMessage,
    target_channel: Option<&Arc<dyn Channel>>,
) -> bool {
    let sender_key = conversation_history_key(msg);

    // Check for pending selection first (bare number or "default N")
    let pending_response = try_handle_pending_selection(ctx, msg, &sender_key).await;
    if let Some(response) = pending_response {
        if let Some(channel) = target_channel {
            let _ = channel
                .send(
                    &SendMessage::new(response, &msg.reply_target)
                        .in_thread(msg.thread_ts.clone())
                        .reply_to(msg.reply_to_message_id.clone()),
                )
                .await;
        }
        return true;
    }

    // Parse slash command
    let Some(command) = parse_runtime_command(&msg.channel, &msg.content) else {
        // Not a command — clear pending selection if any
        clear_pending_selection(ctx, &sender_key);
        return false;
    };

    let Some(channel) = target_channel else {
        return true;
    };

    let mut current = get_route_selection(ctx, &sender_key);

    let response = match command {
        ChannelRuntimeCommand::Models(arg) => {
            handle_models_command(ctx, &sender_key, &mut current, arg.as_deref()).await
        }
        ChannelRuntimeCommand::NewSession => {
            clear_sender_history(ctx, &sender_key);
            clear_pending_selection(ctx, &sender_key);
            "Conversation history cleared. Starting fresh.".to_string()
        }
    };

    if let Err(err) = channel
        .send(
            &SendMessage::new(response, &msg.reply_target)
                .in_thread(msg.thread_ts.clone())
                .reply_to(msg.reply_to_message_id.clone()),
        )
        .await
    {
        tracing::warn!(
            "Failed to send runtime command response on {}: {err}",
            channel.name()
        );
    }

    true
}
```

- [ ] **Step 5: Implement helper functions**

```rust
fn clear_pending_selection(ctx: &ChannelRuntimeContext, sender_key: &str) {
    ctx.pending_selections
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(sender_key);
}

fn set_pending_selection(ctx: &ChannelRuntimeContext, sender_key: &str, kind: PendingSelectionKind) {
    ctx.pending_selections
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(
            sender_key.to_string(),
            PendingSelectionEntry {
                kind,
                created_at: Instant::now(),
            },
        );
}

fn take_pending_selection(
    ctx: &ChannelRuntimeContext,
    sender_key: &str,
) -> Option<PendingSelectionKind> {
    let mut map = ctx
        .pending_selections
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let entry = map.remove(sender_key)?;
    if entry.is_expired() {
        return None;
    }
    Some(entry.kind)
}

async fn try_handle_pending_selection(
    ctx: &ChannelRuntimeContext,
    msg: &traits::ChannelMessage,
    sender_key: &str,
) -> Option<String> {
    let trimmed = msg.content.trim();

    // "default N" pattern
    if let Some(rest) = trimmed.strip_prefix("default ") {
        let rest = rest.trim();
        if let Ok(n) = rest.parse::<usize>() {
            if let Some(PendingSelectionKind::AwaitingModel(provider, models)) =
                take_pending_selection(ctx, sender_key)
            {
                if (1..=models.len()).contains(&n) {
                    let model = &models[n - 1];
                    let _ = crate::onboard::save_provider_default(
                        &ctx.workspace_dir,
                        &provider,
                        model,
                    )
                    .await;
                    return Some(format!(
                        "\u{2705} Default model for {} set to {}",
                        provider, model
                    ));
                }
                // Re-insert so user can retry
                set_pending_selection(
                    ctx,
                    sender_key,
                    PendingSelectionKind::AwaitingModel(provider, models.clone()),
                );
                return Some(format!(
                    "Invalid index. Pick 1-{}.",
                    models.len()
                ));
            }
        }
        // Not a valid "default N" — clear and fall through
        clear_pending_selection(ctx, sender_key);
        return None;
    }

    // Bare number
    if let Ok(n) = trimmed.parse::<usize>() {
        let pending = take_pending_selection(ctx, sender_key)?;
        match pending {
            PendingSelectionKind::AwaitingProvider(providers) => {
                if (1..=providers.len()).contains(&n) {
                    let provider = &providers[n - 1];
                    let mut current = get_route_selection(ctx, sender_key);

                    // Resolve default model for this provider
                    let default_model = crate::onboard::resolve_default_model_for_provider(
                        &ctx.workspace_dir,
                        provider,
                        &ctx.model_routes,
                    )
                    .await;

                    current.provider = provider.clone();
                    if let Some(ref model) = default_model {
                        current.model = model.clone();
                    }
                    set_route_selection(ctx, sender_key, current);

                    // Now show model list for the selected provider
                    let cached = load_all_cached_models(&ctx.workspace_dir, provider);
                    let (hardcoded, fetched) =
                        build_merged_model_list(provider, &ctx.model_routes, &cached);

                    // Fire-and-forget refresh
                    let ws = Arc::clone(&ctx.workspace_dir);
                    let api_key = ctx.api_key.clone();
                    let api_url = ctx.api_url.clone();
                    let p = provider.clone();
                    tokio::spawn(async move {
                        let _ = crate::onboard::refresh_models_quiet(
                            &ws,
                            &p,
                            api_key.as_deref(),
                            api_url.as_deref(),
                            false,
                        )
                        .await;
                    });

                    // Set pending to AwaitingModel
                    let all_models: Vec<String> = hardcoded
                        .iter()
                        .chain(fetched.iter())
                        .cloned()
                        .collect();
                    set_pending_selection(
                        ctx,
                        sender_key,
                        PendingSelectionKind::AwaitingModel(
                            provider.clone(),
                            all_models,
                        ),
                    );

                    return Some(build_model_list_response(
                        provider,
                        &hardcoded,
                        &fetched,
                        default_model.as_deref(),
                    ));
                }
                // Re-insert so user can retry
                set_pending_selection(
                    ctx,
                    sender_key,
                    PendingSelectionKind::AwaitingProvider(providers.clone()),
                );
                return Some(format!(
                    "Invalid index. Pick 1-{}.",
                    providers.len()
                ));
            }
            PendingSelectionKind::AwaitingModel(provider, models) => {
                if (1..=models.len()).contains(&n) {
                    let model = &models[n - 1];
                    let mut current = get_route_selection(ctx, sender_key);

                    // Also switch provider if model route specifies one
                    if let Some(route) = ctx.model_routes.iter().find(|r| r.model == *model) {
                        current.provider = route.provider.clone();
                    } else {
                        current.provider = provider;
                    }
                    current.model = model.clone();
                    set_route_selection(ctx, sender_key, current.clone());

                    return Some(format!(
                        "\u{2705} Switched to {} ({})",
                        model, current.provider
                    ));
                }
                // Re-insert so user can retry
                set_pending_selection(
                    ctx,
                    sender_key,
                    PendingSelectionKind::AwaitingModel(provider, models.clone()),
                );
                return Some(format!(
                    "Invalid index. Pick 1-{}.",
                    models.len()
                ));
            }
        }
    }

    // Not a number — clear pending if any existed
    clear_pending_selection(ctx, sender_key);
    None
}

async fn handle_models_command(
    ctx: &ChannelRuntimeContext,
    sender_key: &str,
    current: &mut ChannelRouteSelection,
    arg: Option<&str>,
) -> String {
    match arg {
        // /models flash — hint shortcut
        Some(hint) => {
            // Try as model_routes hint
            if let Some(route) = ctx.model_routes.iter().find(|r| {
                r.hint.eq_ignore_ascii_case(hint) || r.model.eq_ignore_ascii_case(hint)
            }) {
                current.provider = route.provider.clone();
                current.model = route.model.clone();
                set_route_selection(ctx, sender_key, current.clone());
                format!(
                    "\u{2705} Switched to {} ({})",
                    current.model, current.provider
                )
            } else {
                format!(
                    "Unknown hint `{}`. Use `/models` to see available options.",
                    hint
                )
            }
        }
        // /models — show provider list
        None => {
            let defaults = runtime_defaults_snapshot(ctx);
            let active = collect_active_providers(
                Some(defaults.default_provider.as_str()),
                &ctx.model_routes,
                &ctx.reliability.fallback_providers,
            );

            if active.is_empty() {
                return "No providers configured.".to_string();
            }

            set_pending_selection(
                ctx,
                sender_key,
                PendingSelectionKind::AwaitingProvider(active.clone()),
            );

            build_provider_list_response(current, &active)
        }
    }
}
```

- [ ] **Step 6: Remove old dead code**

Delete:
- `build_models_help_response` function
- `build_providers_help_response` function
- `load_cached_model_preview` function
- `MODEL_CACHE_PREVIEW_LIMIT` constant

Remove old test functions that test `ShowProviders`, `SetProvider`, `ShowModel`, `SetModel` variants (search for these names in the test module and update/remove).

- [ ] **Step 7: Fix compilation — update all test helpers**

Search for `model_routes: Arc::new(Vec::new())` in test code and ensure `pending_selections: Arc::new(Mutex::new(HashMap::new())),` is added next to each.

- [ ] **Step 8: Run clippy + all tests**

Run: `cargo clippy --all-targets -- -D warnings && cargo test`
Expected: all pass

- [ ] **Step 9: Commit**

```bash
git add src/channels/mod.rs
git commit -m "feat: interactive /models command with numbered menus"
```

---

## Chunk 4: Build, Deploy, E2E Verification

### Task 5: Build, deploy, and verify E2E

**Files:** none (operational steps)

- [ ] **Step 1: Build release binary**

```bash
cargo build --release
cp target/release/zeroclaw ~/.local/bin/zeroclaw
```

- [ ] **Step 2: Kill stale processes and restart**

```bash
pkill -f './target/release/zeroclaw daemon' || true
systemctl --user restart zeroclaw.service
```

- [ ] **Step 3: E2E — provider list**

Send `/models` in Telegram. Expect numbered provider list with current provider marked ✓.

- [ ] **Step 4: E2E — select provider**

Reply `1` (or whichever is gemini). Expect model list with ★ on default, hardcoded models listed first, fetched section if available.

- [ ] **Step 5: E2E — select model**

Reply `2`. Expect "Switched to <model> (<provider>)".

- [ ] **Step 6: E2E — set default**

Send `/models`, pick provider, then `default 3`. Expect "Default model for <provider> set to <model>".

Verify persistence: restart daemon, send `/models`, pick same provider — ★ should be on model #3.

- [ ] **Step 7: E2E — hint shortcut**

Send `/models flash`. Expect "Switched to gemini-3-flash-preview (gemini)".

- [ ] **Step 8: E2E — non-number doesn't intercept**

Send `/models`, then type a regular message (not a number). Expect it to go to LLM, not be intercepted.

- [ ] **Step 9: Commit any fixes**

If E2E revealed issues, fix and commit.
