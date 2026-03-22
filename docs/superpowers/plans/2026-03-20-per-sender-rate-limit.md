# Per-Sender Rate Limiting Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the global `ActionTracker` in `SecurityPolicy` with a per-sender tracker so that each Telegram thread/chat has its own independent hourly action budget, preventing one chat from exhausting the limit for all others.

**Architecture:** Add a `PerSenderTracker` struct (a `parking_lot::Mutex<HashMap<String, ActionTracker>>`) inside `SecurityPolicy` alongside the existing global `tracker`. The `record_action()` and `is_rate_limited()` methods on `SecurityPolicy` read the current sender key from the `TOOL_LOOP_THREAD_ID` task-local (already set by the channel loop for every tool call). When a sender key is present, the per-sender bucket is used; when absent (cron, CLI, tests), the global tracker is used as fallback. Zero changes needed in tool call sites — only `policy.rs` changes.

**Tech Stack:** Rust, `parking_lot::Mutex` (already a dependency), `std::collections::HashMap`

---

## File Structure

| File | Change |
|------|--------|
| `src/security/policy.rs` | Add `PerSenderTracker` struct; replace `tracker: ActionTracker` with `tracker: PerSenderTracker`; update `record_action`, `is_rate_limited`, `validate_tool_operation`, `prompt_summary` |
| `src/security/policy.rs` tests | New unit tests for per-sender isolation and fallback |

No other files need to change — all call sites call `security.record_action()` / `security.is_rate_limited()` and the sender resolution is done inside the policy.

---

### Task 1: Add `PerSenderTracker` and wire it into `SecurityPolicy`

**Files:**
- Modify: `src/security/policy.rs:1–10` (imports)
- Modify: `src/security/policy.rs:38–100` (ActionTracker + SecurityPolicy struct)
- Modify: `src/security/policy.rs:1298–1308` (record_action + is_rate_limited)
- Modify: `src/security/policy.rs:1377` (from_config constructor)

- [ ] **Step 1: Write failing tests**

Add to the bottom of `src/security/policy.rs` inside the existing `#[cfg(test)]` mod:

```rust
#[test]
fn per_sender_tracker_isolates_counts() {
    let t = PerSenderTracker::new();
    // sender A hits limit=2 on 3rd call
    assert!(t.record_within("chat_a", 2));  // count=1 ≤ 2 → ok
    assert!(t.record_within("chat_a", 2));  // count=2 ≤ 2 → ok
    assert!(!t.record_within("chat_a", 2)); // count=3 > 2 → blocked
    // sender B is unaffected — its bucket is empty
    assert!(t.record_within("chat_b", 2));  // count=1 ≤ 2 → ok
    assert!(t.record_within("chat_b", 2));  // count=2 ≤ 2 → ok
    assert!(!t.record_within("chat_b", 2)); // count=3 > 2 → blocked
}

#[test]
fn per_sender_tracker_global_key_fallback() {
    // GLOBAL_KEY bucket works the same as any named bucket
    let t = PerSenderTracker::new();
    // is_exhausted uses strict > so max=0 means 0 actions allowed;
    // before any recording, count=0 which is NOT > 0, so not exhausted
    assert!(!t.is_exhausted(PerSenderTracker::GLOBAL_KEY, 1));
    t.record_within(PerSenderTracker::GLOBAL_KEY, u32::MAX);
    // after 1 action, count=1 ≥ 1 → exhausted at max=1
    assert!(t.is_exhausted(PerSenderTracker::GLOBAL_KEY, 1));
}

#[test]
fn per_sender_tracker_is_exhausted_reads_without_spurious_insert() {
    // is_exhausted on unknown key should not insert an empty bucket
    let t = PerSenderTracker::new();
    // Key "ghost" has never been recorded — should not be exhausted at max=1
    assert!(!t.is_exhausted("ghost", 1));
}
```

- [ ] **Step 2: Run failing tests**

```bash
cargo test --lib security::policy::tests::per_sender_tracker 2>&1 | tail -20
cargo test --lib security::policy::tests::security_policy_per_sender 2>&1 | tail -20
```

Expected: FAIL — `PerSenderTracker` not defined.

- [ ] **Step 3: Implement `PerSenderTracker`**

Replace the `ActionTracker` field in `SecurityPolicy` and add the new struct. Edit `src/security/policy.rs`:

**3a. Add import at top (line 1, after existing imports):**
```rust
use std::collections::HashMap;
```

**3b. Add `PerSenderTracker` struct after `ActionTracker` (after line ~75):**
```rust
/// Per-sender sliding-window rate limiter.
///
/// Each unique sender key (Telegram thread ID, Discord channel, etc.) gets
/// its own independent [`ActionTracker`] bucket. When no sender is in scope
/// (cron jobs, CLI), the [`GLOBAL_KEY`] bucket is used.
#[derive(Debug)]
pub struct PerSenderTracker {
    buckets: parking_lot::Mutex<HashMap<String, ActionTracker>>,
}

impl PerSenderTracker {
    /// Bucket key used when no per-sender context is available (cron, CLI).
    pub const GLOBAL_KEY: &'static str = "__global__";

    pub fn new() -> Self {
        Self {
            buckets: parking_lot::Mutex::new(HashMap::new()),
        }
    }

    /// Resolve the current sender key from the task-local, falling back to GLOBAL_KEY.
    fn current_key() -> String {
        crate::agent::loop_::TOOL_LOOP_THREAD_ID
            .try_with(|v| v.clone())
            .ok()
            .flatten()
            .unwrap_or_else(|| Self::GLOBAL_KEY.to_string())
    }

    /// Record one action for the current sender. Returns `true` if allowed
    /// (count after recording <= max), `false` if the budget is exhausted.
    pub fn record_for_current(&self, max: u32) -> bool {
        let key = Self::current_key();
        self.record_within(&key, max)
    }

    /// Record one action for `key`. Returns `true` if count <= max.
    pub fn record_within(&self, key: &str, max: u32) -> bool {
        let mut buckets = self.buckets.lock();
        let tracker = buckets.entry(key.to_string()).or_insert_with(ActionTracker::new);
        let count = tracker.record();
        count <= max as usize
    }

    /// Record one action for `key`. Returns the new count (for tests).
    pub fn record(&self, key: &str) -> bool {
        // Use a very large max so this always returns true (for test helpers)
        self.record_within(key, u32::MAX)
    }

    /// Check if the current sender is at or over the limit (without recording).
    pub fn is_limited_for_current(&self, max: u32) -> bool {
        let key = Self::current_key();
        self.is_exhausted(&key, max)
    }

    /// Check if `key` is at or over `max` (without recording).
    /// Does NOT insert a bucket for unseen keys.
    pub fn is_exhausted(&self, key: &str, max: u32) -> bool {
        let mut buckets = self.buckets.lock();
        match buckets.get_mut(key) {
            Some(tracker) => tracker.count() >= max as usize,
            None => false, // no actions recorded → cannot be exhausted
        }
    }
}

impl Clone for PerSenderTracker {
    fn clone(&self) -> Self {
        let buckets = self.buckets.lock();
        Self {
            buckets: parking_lot::Mutex::new(buckets.clone()),
        }
    }
}

impl Default for PerSenderTracker {
    fn default() -> Self {
        Self::new()
    }
}
```

**3c. Replace `tracker: ActionTracker` with `tracker: PerSenderTracker` in `SecurityPolicy` struct (around line 97):**
```rust
pub tracker: PerSenderTracker,
```

**3d. Update `record_action` and `is_rate_limited` (around lines 1300–1308):**
```rust
/// Record an action for the current sender and check if the rate limit has been exceeded.
/// Returns `true` if the action is allowed, `false` if rate-limited.
pub fn record_action(&self) -> bool {
    self.tracker.record_for_current(self.max_actions_per_hour)
}

/// Check if the current sender would be rate-limited without recording.
pub fn is_rate_limited(&self) -> bool {
    self.tracker.is_limited_for_current(self.max_actions_per_hour)
}
```

**3e. Update `from_config` constructor (around line 1377):**
```rust
tracker: PerSenderTracker::new(),
```

**3f. Fix `tracker:` literals inside `src/security/policy.rs`**

Run:
```bash
grep -n "tracker: ActionTracker" src/security/policy.rs
```

There are exactly two occurrences (line 206 `impl Default` and line 1377 `from_config`). Replace both:
```rust
tracker: PerSenderTracker::new(),
```

**3g. Fix `policy.tracker.count()` direct call at line 2576**

This test directly calls `policy.tracker.count()` which no longer exists on `PerSenderTracker`. Replace:
```rust
// OLD (line 2576):
assert_eq!(policy.tracker.count(), 0);
// NEW:
assert!(!policy.is_rate_limited()); // equivalent check via public API
```

**3h. Verify no `tracker: ActionTracker` in other files**

`src/tools/` test helpers use `..SecurityPolicy::default()` struct update syntax — they do NOT need changes. Confirm:
```bash
grep -rn "tracker: ActionTracker" src/ --include="*.rs"
```
Expected: zero results (only `policy.rs` had these, already fixed above).

- [ ] **Step 4: Confirm no other files need changes**

```bash
grep -rn "tracker: ActionTracker" src/ --include="*.rs"
# Expected: (no output)
grep -rn "\.tracker\." src/ --include="*.rs" | grep -v "policy\.rs\|//\|test"
# Expected: (no output — all access goes through record_action/is_rate_limited)
```

- [ ] **Step 5: Run tests**

```bash
cargo test --lib security::policy 2>&1 | tail -30
```

Expected: all existing policy tests pass + new tests pass.

- [ ] **Step 6: Build to catch compile errors across all tools**

```bash
cargo build 2>&1 | grep "^error" | head -30
```

Expected: no errors. If there are `tracker: ActionTracker::new()` in other files, fix them.

- [ ] **Step 7: Run full test suite**

```bash
cargo test --lib 2>&1 | tail -20
```

Expected: all pass.

- [ ] **Step 8: Commit**

```bash
git add src/security/policy.rs
git commit -m "feat(security): per-sender rate limiting via PerSenderTracker

Replace global ActionTracker in SecurityPolicy with PerSenderTracker,
which maintains a separate sliding-window bucket per TOOL_LOOP_THREAD_ID
(Telegram thread, Discord channel, etc.). When no sender context is
present (cron, CLI), falls back to a shared GLOBAL_KEY bucket.

All tool call sites unchanged — sender resolution is encapsulated
inside record_action() / is_rate_limited() on SecurityPolicy."
```

---

### Task 2: Update `prompt_summary` to reflect per-sender limits

**Files:**
- Modify: `src/security/policy.rs:1387–1470` (prompt_summary method)

- [ ] **Step 1: Find the rate limit line in prompt_summary**

```bash
grep -n "actions per hour\|Rate limit\|max_actions" src/security/policy.rs | grep -v "assert\|test\|//"
```

- [ ] **Step 2: Update the description**

Find this line (around 1461):
```rust
"**Rate limit**: max {} actions per hour.",
self.max_actions_per_hour
```

Replace with:
```rust
"**Rate limit**: max {} actions per hour per chat (each conversation has its own independent budget).",
self.max_actions_per_hour
```

- [ ] **Step 3: Run test**

```bash
cargo test --lib security::policy::tests::prompt_summary 2>&1 | tail -10
```

Expected: PASS (test checks `contains("actions per hour")` — still true).

- [ ] **Step 4: Commit**

```bash
git add src/security/policy.rs
git commit -m "docs(security): clarify per-sender rate limit in prompt summary"
```

---

## Verification

```bash
# Full build + all tests
cargo build
cargo test --lib

# Restart daemon and observe per-chat isolation:
./dev/restart-daemon.sh

# In Telegram: send the coder skill 60+ shell commands from chat A
# Check: chat B is not affected (can still run commands)
# Check daemon log for "Rate limit exceeded" — should show sender key context
```

## Notes

- `PerSenderTracker` buckets accumulate in memory for the daemon lifetime. For 1500 senders × ~50 Instant entries each = negligible (~300KB). No pruning needed.
- Cron jobs share the `__global__` bucket — if many cron jobs fire simultaneously they share the global budget, which is correct (cron is system-level, not user-level).
- The `max_actions_per_hour` config stays as-is — it's now per-chat, so you likely want to lower it from 1500 to something like 200–500. That's a config change, not code.
