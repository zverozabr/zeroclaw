# Per-Request Scoped Signals Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate race conditions in `LAST_APPLIED_MODEL_SWITCH` and `LAST_PROVIDER_FALLBACK` by scoping them per-request instead of using process-wide globals.

**Architecture:** Replace global statics with per-request `Arc<Mutex<Option<...>>>` instances created in `process_channel_message` and threaded through to `run_tool_call_loop` / `ReliableProvider`. This mirrors the existing `model_switch_callback` parameter pattern. Each concurrent request gets its own isolated slot — no cross-request leakage.

**Tech Stack:** Rust, `Arc<Mutex<>>`, existing parameter-passing pattern

---

## The Problem

Both `LAST_APPLIED_MODEL_SWITCH` (in `src/agent/loop_.rs`) and `LAST_PROVIDER_FALLBACK` (in `src/providers/reliable.rs`) are process-wide global statics. When two users send messages concurrently:

- User A's model_switch could be read by User B's post-agent check
- User A's fallback info could appear as a footer in User B's response

The existing `MODEL_SWITCH_REQUEST` global has the same issue and should also be replaced.

### Why not tokio::task_local?

task_locals are only accessible within their `.scope()`. The post-loop reads in `process_channel_message` happen AFTER `run_tool_call_loop` returns — outside the scope. We would need to wrap the entire function body in a scope, which is complex and fragile. Per-request `Arc<Mutex<>>` is simpler and correct.

---

## File Map

| File | Action | Change |
|------|--------|--------|
| `src/providers/reliable.rs` | Modify | Remove `LAST_PROVIDER_FALLBACK` global. Add `fallback_slot` field to `ReliableProvider`. Write to it on fallback success. |
| `src/providers/mod.rs` | Modify | Thread `fallback_slot` through provider creation. |
| `src/agent/loop_.rs` | Modify | Remove `LAST_APPLIED_MODEL_SWITCH` global. Add param to `run_tool_call_loop`. Write to it on model switch. Remove `MODEL_SWITCH_REQUEST` global — use the callback param (already exists but unused by channels). |
| `src/channels/mod.rs` | Modify | Create per-request slots, pass through, read after loop. Update tests. |
| `src/tools/model_switch.rs` | Modify | Accept callback slot instead of reading global. |

---

### Task 1: Add fallback_slot to ReliableProvider

**Files:**
- Modify: `src/providers/reliable.rs:11-55` (remove global, add field)
- Modify: `src/providers/reliable.rs` (4 success points)

- [ ] **Step 1: Add FallbackSlot type and field to ReliableProvider**

```rust
// Remove: static LAST_PROVIDER_FALLBACK global, take_last_provider_fallback(), record_provider_fallback()

/// Shared slot for recording a provider fallback event per-request.
pub type FallbackSlot = Arc<Mutex<Option<ProviderFallbackInfo>>>;

// Add field to ReliableProvider struct:
pub(crate) fallback_slot: Option<FallbackSlot>,
```

- [ ] **Step 2: Update ReliableProvider::new to accept fallback_slot**

Add `fallback_slot: Option<FallbackSlot>` param to `new()`. Store it.

- [ ] **Step 3: Update 4 success points to write to self.fallback_slot**

Replace `record_provider_fallback(primary, model, provider_name, current_model)` with:

```rust
if let Some(ref slot) = self.fallback_slot {
    *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(ProviderFallbackInfo {
        requested_provider: primary.to_string(),
        requested_model: model.to_string(),
        actual_provider: provider_name.to_string(),
        actual_model: current_model.to_string(),
    });
}
```

- [ ] **Step 4: Add public take function**

```rust
/// Take fallback info from a slot (used by channel code after the loop).
pub fn take_fallback(slot: &FallbackSlot) -> Option<ProviderFallbackInfo> {
    slot.lock().unwrap_or_else(|e| e.into_inner()).take()
}
```

- [ ] **Step 5: Update all `ReliableProvider::new(...)` call sites**

Add `None` for `fallback_slot` at every existing call site (search for `ReliableProvider::new(`). Production code in `mod.rs` will pass `Some(slot)` — done in Task 3.

- [ ] **Step 6: Update test `fallback_records_provider_fallback_info`**

Create a slot, pass to `ReliableProvider::new`, read from slot after call.

- [ ] **Step 7: Run tests**

Run: `cargo test --lib`
Expected: all pass

- [ ] **Step 8: Commit**

```bash
git add src/providers/reliable.rs src/providers/mod.rs
git commit -m "refactor(providers): per-request fallback_slot replaces global LAST_PROVIDER_FALLBACK"
```

---

### Task 2: Thread model_switch callback from channels into run_tool_call_loop

**Files:**
- Modify: `src/agent/loop_.rs:44-64` (remove LAST_APPLIED + MODEL_SWITCH_REQUEST globals)
- Modify: `src/agent/loop_.rs` (3 writer sites + 1 reader site)
- Modify: `src/tools/model_switch.rs` (accept callback instead of global)
- Modify: `src/channels/mod.rs` (create per-request callback, pass it)

- [ ] **Step 1: Remove LAST_APPLIED_MODEL_SWITCH global**

Remove the static and `take_last_applied_model_switch()`. The 3 writer sites now write to `model_switch_callback` (already a parameter of `run_tool_call_loop`).

- [ ] **Step 2: Update 3 writer sites in loop_.rs**

Replace `*LAST_APPLIED_MODEL_SWITCH.lock()...` with writing to `model_switch_callback`:

```rust
if let Some(ref cb) = model_switch_callback {
    *cb.lock().unwrap_or_else(|e| e.into_inner()) =
        Some((provider_name.clone(), model_name.clone()));
}
```

The normal-exit writer (~line 3107) needs access to `model_switch_callback`. Check it's in scope — it's a parameter, so yes.

- [ ] **Step 3: Make model_switch tool write to callback instead of global**

`model_switch.rs` currently calls `get_model_switch_state()` to get the global. Instead, it should write to the same `model_switch_callback` that `run_tool_call_loop` uses.

Approach: use the existing `TOOL_LOOP_SENDER_KEY` task_local pattern. Add a new task_local for the callback:

```rust
// In loop_.rs:
tokio::task_local! {
    pub(crate) static MODEL_SWITCH_SLOT: Option<ModelSwitchCallback>;
}
```

The `run_tool_call_loop` wraps its body in `MODEL_SWITCH_SLOT.scope(model_switch_callback.clone(), ...)`.

The `model_switch` tool reads `MODEL_SWITCH_SLOT` via `try_with` instead of `get_model_switch_state()`.

**But:** task_local requires `.scope()` wrapping which increases future size (clippy box_size). Simpler: keep `MODEL_SWITCH_REQUEST` global BUT clear+read it atomically with a per-request guard.

**Simplest correct approach:** Keep `MODEL_SWITCH_REQUEST` global. In `process_channel_message`, create a per-request `ModelSwitchCallback` (`Arc<Mutex<Option<...>>>`), pass it as the `model_switch_callback` parameter (currently `None`). In `run_tool_call_loop`, when the normal-exit writer fires, write to `model_switch_callback`. The 3 writer sites already write to `model_switch_callback` or `LAST_APPLIED_MODEL_SWITCH` — unify to just `model_switch_callback`.

For the `model_switch` tool: it still writes to `MODEL_SWITCH_REQUEST` global. The `run_tool_call_loop` inner check (line 2712) reads from `model_switch_callback` which IS `MODEL_SWITCH_REQUEST` in CLI mode and is a per-request slot in channel mode. BUT the tool writes to the global, not to the per-request slot.

**Final clean approach:** Pass the callback to the tool via `ToolExecutionContext` or similar. But tools don't receive the callback.

**Pragmatic fix:** In channels mode, `model_switch_callback` is per-request. `clear_model_switch_request()` at line 3203 clears the global before the loop. The tool writes to the global. The loop inner check (line 2712) checks `model_switch_callback` (per-request, which is empty) — miss. But the normal-exit check (new code at ~3107) reads from `model_switch_callback` (empty) AND from global `.take()`. Post-loop check reads from `model_switch_callback` first, then global as fallback. This is the current pattern!

**The real fix is simpler than I thought:** Just pass a fresh `Arc<Mutex<None>>` as `model_switch_callback` from channels. The model_switch tool writes to the global. The normal-exit code (line ~3107) already reads from global and copies into `model_switch_callback` (currently `LAST_APPLIED_MODEL_SWITCH`). Change it to write into `model_switch_callback` param instead. Post-loop code reads from the per-request callback.

- [ ] **Step 4: In channels, pass per-request callback**

Replace `None` (last param of `run_tool_call_loop`) with `Some(model_switch_slot.clone())`:

```rust
let model_switch_slot: ModelSwitchCallback = Arc::new(Mutex::new(None));
// ... pass Some(model_switch_slot.clone()) to run_tool_call_loop ...
```

- [ ] **Step 5: In post-loop, read from per-request slot**

Replace:
```rust
let pending = take_last_applied_model_switch()
    .or_else(|| get_model_switch_state().lock().unwrap().take());
```

With:
```rust
let pending = model_switch_slot.lock().unwrap_or_else(|e| e.into_inner()).take()
    .or_else(|| get_model_switch_state().lock().unwrap().take());
```

The global fallback remains for safety (model_switch tool writes there).

- [ ] **Step 6: Remove LAST_APPLIED_MODEL_SWITCH**

Now unused — remove static, `take_last_applied_model_switch()`, and `pub(crate)` visibility.

- [ ] **Step 7: Update tests**

Update `take_last_applied_model_switch_returns_and_clears` — test now uses per-request slot.

- [ ] **Step 8: Run all tests**

Run: `cargo test --lib`
Expected: all pass

- [ ] **Step 9: Commit**

```bash
git add src/agent/loop_.rs src/channels/mod.rs
git commit -m "refactor(agent): per-request model_switch_callback replaces LAST_APPLIED global"
```

---

### Task 3: Thread fallback_slot from channels through provider creation

**Files:**
- Modify: `src/providers/mod.rs` (provider creation functions)
- Modify: `src/channels/mod.rs` (create slot, pass to provider, read after loop)

- [ ] **Step 1: Add fallback_slot param to create_routed_provider_with_options**

This is the function channels use to create providers. Add `fallback_slot: Option<FallbackSlot>` and pass through to `ReliableProvider::new`.

- [ ] **Step 2: In channels, create per-request FallbackSlot**

```rust
let fallback_slot: FallbackSlot = Arc::new(Mutex::new(None));
```

Pass `Some(fallback_slot.clone())` when creating the provider for this request.

- [ ] **Step 3: In post-loop, read from per-request slot**

Replace:
```rust
if let Some(fb) = crate::providers::reliable::take_last_provider_fallback() {
```

With:
```rust
if let Some(fb) = crate::providers::reliable::take_fallback(&fallback_slot) {
```

- [ ] **Step 4: Run all tests**

Run: `cargo test --lib`
Expected: all pass

- [ ] **Step 5: Commit**

```bash
git add src/providers/mod.rs src/providers/reliable.rs src/channels/mod.rs
git commit -m "feat(channels): per-request fallback_slot threaded through provider creation"
```

---

### Task 4: Fix pre-existing clippy warning

**Files:**
- Modify: `src/channels/mod.rs:7178`

- [ ] **Step 1: Rename `_guard_key` to `guard_key`**

The variable is used (`.remove(_guard_key)`), so the underscore prefix triggers clippy.

- [ ] **Step 2: Run clippy**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: this specific warning gone

- [ ] **Step 3: Commit**

```bash
git add src/channels/mod.rs
git commit -m "chore: fix clippy underscore-prefixed binding warning"
```

---

### Task 5: E2E verification

- [ ] **Step 1: Build and restart daemon**

```bash
cargo build --release && ./dev/restart-daemon.sh
```

- [ ] **Step 2: Verify routes.json reset**

Send via Telegram:
1. `switch model to google gemini-2.0-flash` → verify routes.json has google entry
2. `switch model to minimax MiniMax-M2.7-highspeed` → verify routes.json is `{}` (not `{"provider":"minimax",...}`)

- [ ] **Step 3: Verify fallback notification**

Wait for a natural fallback (happens ~2171 times in logs) or force one by temporarily setting an invalid API key for primary provider. Verify footer appears.

- [ ] **Step 4: Verify no cross-request leakage**

Send two messages rapidly from the same user. Verify no stale fallback footers appear on non-fallback responses.
