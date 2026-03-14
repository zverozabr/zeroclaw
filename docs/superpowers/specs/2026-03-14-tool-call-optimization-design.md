# Tool Call Optimization & Skill Logic Extraction

## Problem Statement

E2E tests (b1-b10, d1-d3) reveal 5 systemic reliability issues in the agent runtime:

1. **Session contention**: Model spawns 30+ parallel `telegram_search_messages` — all hit "session is busy" lock, waste 120s each
2. **Token overflow**: 30 parallel tool results inflate context to 1.3M tokens (limit 1M) — all providers reject
3. **Malformed tool calls**: On iteration 4+ (history_len=56), model generates XML `<tool_call>` instead of native function calls — Telegram rejects empty text
4. **Raw error dumps**: When all providers fail, bot sends raw `provider=gemini:gemini-1 attempt 1/4: non_retryable...` as user reply
5. **Hardcoded skill logic**: Validation, formatting, and output rules baked into Python scripts instead of declarative SKILL.toml config

## Design

### Block 1: Parallel Tool Call Limits

**Goal**: Prevent session contention and context explosion from too many concurrent tool calls.

**Global limit** in config.toml:
```toml
[agent]
max_parallel_tool_calls = 5
```

**Per-tool override** in SKILL.toml:
```toml
[tools.telegram_search_messages]
max_parallel = 3
```

**Implementation**:
- `loop_.rs`: Before executing tool calls, if count > global limit, split into sequential batches of `max_parallel_tool_calls`
- `tool_handler.rs`: If tool has `max_parallel` set, use a `tokio::sync::Semaphore` to limit concurrent executions of that specific tool
- Priority: per-tool override > global limit
- Config schema: add `max_parallel_tool_calls: Option<usize>` to `AgentConfig`; add `max_parallel: Option<usize>` to `SkillTool`

**Effect**: Instead of 30 parallel search_messages, max 3 run concurrently. Session contention eliminated, token budget stays within limits.

### Block 2: Malformed `<tool_call>` Recovery

**Goal**: When model generates XML tool calls instead of native function calls, parse and execute them rather than sending raw XML to user.

**From logs**, the malformed format:
```
<tool_call>
{"name": "submit_contacts", "arguments": {"contacts_json": "..."}}
```

**Implementation** in `loop_.rs`:
1. If response text contains `<tool_call>` with JSON body — extract and parse as `ToolCall` struct
2. If JSON valid and tool name exists in registry — execute as normal tool call, continue loop
3. If JSON invalid — do not send `<tool_call>` text to user; return generic error "Could not process request"

**Implementation** in `channels/mod.rs`:
- Safety net: strip any remaining `<tool_call>` tags from final response before sending to channel (existing tag stripping partially covers this, extend pattern matching)

**Not changed**: Native function calling path. This is fallback-only for when model switches to XML format on long contexts.

### Block 3: Error Dump Sanitization

**Goal**: Replace raw provider error dumps with clean, actionable user messages.

**Where**: `channels/mod.rs`, before sending to channel.

**Detection**: Text contains patterns like `provider=`, `attempt \d+/\d+`, `non_retryable`, `rate_limited`.

**Error classification and messages**:
| Error type | Detection | User message |
|-----------|-----------|-------------|
| Token overflow | `input token count.*exceeds` | "Request too large - try a more specific question" |
| Rate limit | `rate_limited` or `RESOURCE_EXHAUSTED` | "All providers overloaded, try again in a minute" |
| Auth/model | `model is not supported` or `UNAUTHENTICATED` | "Provider config error: {model} not supported" |
| Unknown | Other error patterns | "Could not process request. Try again later." |

**Also**: If text starts with `(continued)` and contains error patterns — sanitize (split-message with errors).

**Preserved**: Full errors still written to daemon logs for diagnostics.

### Block 4: Tool Result Truncation + Token Budget

**Goal**: Prevent context window overflow through per-result truncation and budget-based compaction.

**Per-result truncation** in `loop_.rs`:
- When writing tool result to history, truncate to `max_tool_result_chars` (default 4000)
- Config: `[agent] max_tool_result_chars = 4000`
- Per-tool override in SKILL.toml: `max_result_chars = 2000`
- Truncation uses `floor_char_boundary()` + appends `\n...(truncated, {original_len} chars total)`

**Token budget compaction** as safety net:
- After assembling all tool results for current iteration, estimate tokens: `total_history_chars / 3`
- If > 800K tokens: aggressive compaction — truncate each history tool result to 1000 chars, keep last 10 messages
- If > 500K tokens: soft compaction — truncate to 2000 chars, keep last 15 messages

**Effect**: 30 search_messages x 4000 chars = 120K chars ~ 40K tokens (vs 1.3M tokens before).

### Block 5: Skill Config Extraction

**Goal**: Move skill-specific validation/output parameters from hardcoded Python to declarative SKILL.toml.

**SKILL.toml additions**:
```toml
[tools.submit_contacts]
terminal = true
max_result_chars = 8000

[tools.submit_contacts.validation]
url_pattern = 'https?://t\.me/(?:c/\d+|[^/]+)/\d+'
url_verify = true
url_verify_timeout_secs = 3
min_phone_digits = 7
reject_sequential_phones = true
reject_same_digit_phones = true
verbatim_gate = true
min_author_message_chars = 30

[tools.submit_contacts.output]
format = "structured"
max_message_text_chars = 300
fields = ["username_or_phone", "description", "date", "source_url", "message_text"]

[tools.telegram_search_messages]
max_parallel = 3
max_result_chars = 2000

[tools.telegram_search_global]
max_result_chars = 3000
```

**What stays in code** (generic, used by other tools):
- `tool_handler.rs`: env sandboxing, command rendering, rate limiting, semaphore enforcement
- `loop_.rs`: early-return, token budget, parallel batching
- `channels/mod.rs`: error sanitization, message splitting, tag stripping
- `reliable.rs`: provider fallback chain, retry logic

**Parameter passing**: `tool_handler.rs` reads SKILL.toml validation/output sections, passes to Python scripts as `SKILL_VALIDATION_*` and `SKILL_OUTPUT_*` env vars. Python reads from env instead of hardcoded values.

## Implementation Order

1 → 4 → 2 → 3 → 5

Blocks 1+4 (parallel limits + token budget) eliminate the infrastructure failures (session contention, token overflow).
Block 2 (malformed tool_call) fixes the XML fallback issue.
Block 3 (error sanitization) cleans up user-facing errors.
Block 5 (skill config) is architectural improvement — SOLID/DRY.

## Verification

Each block verified by full E2E test suite: b1-b10, b_new1, b_new2, d1-d3 (15 tests).
Target: 15/15 stable pass rate without infrastructure-dependent failures.

## Risk Assessment

- **Medium risk**: Blocks 1, 4 (loop_.rs changes affect all tool execution)
- **Low risk**: Blocks 2, 3 (additive parsing/filtering, no behavior change for happy path)
- **Low risk**: Block 5 (config extraction, Python reads env vars as fallback to hardcoded defaults)
