# Search Pipeline Verbatim Gate Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate hallucinated contacts by adding `contact_candidates` (regex-extracted) to search tool output and a verbatim gate in `submit_contacts.py` that rejects any contact not found literally in its source message text.

**Architecture:** Three-layer defence — (1) search tools expose `contact_candidates` so the LLM has clear evidence to work from; (2) `submit_contacts.py` rejects contacts not found verbatim in `message_text` or equal to `author_contact`; (3) a new E2E test (b10) and unit test (u8) prove the gate works end-to-end. Root cause confirmed in daemon logs: model fires `submit_contacts` in the same parallel batch as search tools, so it hallucinates before any results arrive — verbatim gate is the only structural defence that catches this.

**Tech Stack:** Python 3 (telegram_reader.py, submit_contacts.py), Rust (telegram_search_quality.rs), Telethon, regex

---

## Chunk 1: contact_candidates in search tool output

### Task 1: Add `contact_candidates` field to telegram_reader.py

**Files:**
- Modify: `~/.zeroclaw/workspace/skills/telegram-reader/scripts/telegram_reader.py`

Background: `search_global` and `search_messages` return message objects. We add Python-side regex extraction of `@usernames` and phone numbers from message text. This gives the LLM (and the verbatim gate) an authoritative list of what's actually in each message.

- [ ] **Step 1.1: Read the message-building code in telegram_reader.py**

```bash
grep -n "message_link\|author_contact\|\"text\"" \
  ~/.zeroclaw/workspace/skills/telegram-reader/scripts/telegram_reader.py | head -40
```

Find the dict that builds each result entry (used in both `_search_global_impl` and `search_messages`). It looks like:
```python
result = {
    "id": msg.id,
    "date": ...,
    "text": msg.text or "",
    ...
    "message_link": _make_message_link(...),
    "author_contact": ...,
}
```

- [ ] **Step 1.2: Add `_extract_contact_candidates()` function**

Add this function near the top of telegram_reader.py, after the imports section (before the first class or function definition):

```python
import re as _re

_CANDIDATE_USERNAME_RE = _re.compile(r'@([A-Za-z][A-Za-z0-9_]{3,32})')
_CANDIDATE_PHONE_RE = _re.compile(
    r'(?<!\d)(\+?[0-9][\d\s\-\(\)]{6,}[0-9])(?!\d)'
)


def _extract_contact_candidates(text: str) -> list:
    """Extract @usernames and phone numbers verbatim from message text.

    Returns a list of strings like ['@Garyxz', '+66812345678'].
    Used downstream by submit_contacts.py for verbatim validation.
    """
    if not text:
        return []
    usernames = [f"@{m}" for m in _CANDIDATE_USERNAME_RE.findall(text)]
    raw_phones = _CANDIDATE_PHONE_RE.findall(text)
    phones = [_re.sub(r'[\s\-\(\)]', '', p) for p in raw_phones if len(_re.sub(r'\D', '', p)) >= 7]
    # Deduplicate preserving order
    seen = set()
    out = []
    for c in usernames + phones:
        if c not in seen:
            seen.add(c)
            out.append(c)
    return out
```

- [ ] **Step 1.3: Add `contact_candidates` to every result dict in `_build_result_entry` (or inline)**

Find where result dicts are built (likely a helper or inline in `_search_global_impl` and `search_messages`). Add one line:

```python
"contact_candidates": _extract_contact_candidates(msg.text or ""),
```

Place it directly after `"author_contact": ...` so it appears in JSON output.

If there is no shared helper and the dict is built in two places, add the line to both (DRY violation acceptable since it's one line — rule of three not yet reached).

- [ ] **Step 1.4: Verify output manually**

```bash
cd ~/.zeroclaw/workspace/skills/telegram-reader
set -a && source /home/spex/work/erp/zeroclaws/.env && set +a
python3 scripts/telegram_reader.py search_global \
  --query "сантехник" --limit 3 --account research 2>/dev/null \
  | python3 -m json.tool | grep -A5 "contact_candidates"
```

Expected output — at least one result with non-empty `contact_candidates`:
```json
"contact_candidates": [
    "@Garyxz"
],
```

- [ ] **Step 1.5: Verify search_messages also has the field**

```bash
python3 scripts/telegram_reader.py search_messages \
  --contact-name "samui0" --query "сантехник" --limit 3 --account research 2>/dev/null \
  | python3 -m json.tool | grep -A5 "contact_candidates"
```

- [ ] **Step 1.6: Commit**

```bash
cd ~/.zeroclaw
git add workspace/skills/telegram-reader/scripts/telegram_reader.py
git commit -m "feat(telegram-reader): add contact_candidates field to search_global and search_messages output"
```

---

## Chunk 2: Verbatim gate in submit_contacts.py

### Task 2: Refactor format_contact → validate_and_format, add verbatim gate

**Files:**
- Modify: `~/.zeroclaw/workspace/skills/telegram-reader/scripts/submit_contacts.py`

The gate rule:
- If `username_or_phone` starts with `@`: check it appears (case-insensitive, without `@` prefix) in `message_text`. If not, and it's not equal to `author_contact` → **reject entirely** (return None, not ⚠).
- If `username_or_phone` is a phone: check it appears in `message_text` (strip non-digits for comparison). If not, and it's not in `author_contact` → **reject entirely**.
- `author_contact` as contact (when it IS the source): allow even if not in body — but require non-empty `message_text` (≥ 30 chars) as evidence of real message.

Rejection means: contact is dropped from output silently (logged to stderr). Not shown to user at all — no ⚠ label.

- [ ] **Step 2.1: Add `_contact_in_text()` helper**

Add after `_is_plausible_phone()`:

```python
def _contact_in_text(contact, message_text):
    """Return True if contact identifier appears literally in message_text.

    Comparison is case-insensitive. For @usernames, strips the @ prefix.
    For phones, strips non-digit characters before comparing.
    """
    if not message_text or not contact:
        return False
    text = message_text.lower()
    if contact.startswith("@"):
        return contact[1:].lower() in text
    # Phone: compare digit-only form
    digits = re.sub(r"\D", "", contact)
    return digits in re.sub(r"\D", "", text) if len(digits) >= 7 else True
```

- [ ] **Step 2.2: Refactor format_contact → validate_and_format_contact**

Rename the function and change its return type to `Optional[str]` (returns `None` when the contact is rejected). Update call sites in `main()`.

```python
def validate_and_format_contact(c):
    """Validate contact against source message, then format for display.

    Returns formatted string, or None if the contact is rejected.
    """
    u = c.get("username_or_phone") or "?"
    desc = c.get("description") or ""
    date = c.get("date") or "неизвестно"
    src_url = c.get("source_url")
    msg_text = c.get("message_text") or ""
    author = c.get("author_contact") or ""

    # ── source_url validation ──────────────────────────────────────────
    if not _is_valid_message_link(src_url):
        src_url = None
    if src_url and not _verify_message_link(src_url):
        print(f"[pipeline] REJECTED hallucinated URL: {src_url}", file=sys.stderr, flush=True)
        src_url = None

    # ── verbatim gate ──────────────────────────────────────────────────
    is_author_contact = (u.lstrip("@").lower() == author.lstrip("@").lower()) if author else False
    in_text = _contact_in_text(u, msg_text)

    if not in_text and not is_author_contact:
        print(
            f"[pipeline] REJECTED verbatim-missing: {u!r} not in message_text "
            f"(len={len(msg_text)}) and not author_contact={author!r}",
            file=sys.stderr, flush=True,
        )
        return None  # Hard reject — not shown to user

    if is_author_contact and len(msg_text.strip()) < 30:
        print(
            f"[pipeline] REJECTED author-no-quote: {u!r} is author_contact but "
            f"message_text too short (len={len(msg_text)})",
            file=sys.stderr, flush=True,
        )
        return None

    # ── @username HTTP verify (existing, keep) ─────────────────────────
    if u.startswith("@") and not _verify_username(u):
        print(f"[pipeline] REJECTED fake username: {u}", file=sys.stderr, flush=True)
        return None  # Also hard reject now (was ⚠ before)

    # ── phone plausibility (existing, keep) ───────────────────────────
    if not u.startswith("@") and not _is_plausible_phone(u):
        print(f"[pipeline] REJECTED suspicious phone: {u}", file=sys.stderr, flush=True)
        return None

    # ── quote check ───────────────────────────────────────────────────
    has_quote = bool(msg_text and len(msg_text.strip()) >= 20)
    if not has_quote:
        print(f"[pipeline] WARNING no-quote: {u!r}", file=sys.stderr, flush=True)

    # ── format ────────────────────────────────────────────────────────
    lines = [f"**{u}** — {desc}"]
    if msg_text:
        lines.append(_quote_lines(msg_text))
    if not has_quote:
        lines.append("[без цитаты — возможна неточность]")
    lines.append(f"Дата: {date}")
    if src_url:
        lines.append(f"Источник: {src_url}")
    else:
        src_line = "Источник: недоступна"
        if author:
            src_line += f" | Автор: {author}"
        lines.append(src_line)

    print(f"[pipeline] ACCEPTED: {u!r} date={date}", file=sys.stderr, flush=True)
    return "\n".join(lines)
```

- [ ] **Step 2.3: Update main() to use new function and filter None**

In `main()`, replace:
```python
contact_text = format_contact(c)
```
With:
```python
contact_text = validate_and_format_contact(c)
if contact_text is None:
    continue  # hard-rejected by verbatim gate or verification
```

Do this for the loop that classifies contacts into `text_parts / media_queue / private_media_queue`.

- [ ] **Step 2.4: Add pipeline summary log at end of main()**

At the end of `main()`, before the final print, add:

```python
accepted = sent_text + sent_media
rejected = len(contacts) - accepted
print(
    f"[pipeline] SUMMARY: total={len(contacts)} accepted={accepted} rejected={rejected}",
    file=sys.stderr, flush=True,
)
```

- [ ] **Step 2.5: Smoke-test manually with a fabricated contact**

```bash
cd ~/.zeroclaw/workspace/skills/telegram-reader
set -a && source /home/spex/work/erp/zeroclaws/.env && set +a
python3 scripts/submit_contacts.py '{
  "contacts": [{
    "username_or_phone": "@fake_username_xyz_not_real",
    "description": "тест",
    "date": "2026-03-11",
    "source_url": null,
    "message_text": "Ищу сантехника, звоните.",
    "author_contact": null
  }]
}' 2>&1
```

Expected stderr: `[pipeline] REJECTED verbatim-missing: '@fake_username_xyz_not_real'`
Expected stdout: `Не найдено подходящих контактов по запросу.`

- [ ] **Step 2.6: Smoke-test with a real contact (in text)**

```bash
python3 scripts/submit_contacts.py '{
  "contacts": [{
    "username_or_phone": "@Garyxz",
    "description": "Мастер на час",
    "date": "2026-03-05",
    "source_url": "https://t.me/samui0/118579",
    "message_text": "Строительные работы. Пишите в лс @Garyxz. Работаем на Пхукете.",
    "author_contact": "@Garyxz"
  }]
}' 2>&1
```

Expected stderr: `[pipeline] ACCEPTED: '@Garyxz'`

- [ ] **Step 2.7: Smoke-test author_contact path (not in body)**

```bash
python3 scripts/submit_contacts.py '{
  "contacts": [{
    "username_or_phone": "@Olga_Posha",
    "description": "Ищет сантехника",
    "date": "2026-02-20",
    "source_url": "https://t.me/samuir/152789",
    "message_text": "Друзья, нужна помощь! Нужен сантехник или человек с руками. Локация Самуи Маенам. Пишите в лс)",
    "author_contact": "@Olga_Posha"
  }]
}' 2>&1
```

Expected: ACCEPTED (author_contact matches, message_text ≥ 30 chars).

- [ ] **Step 2.8: Commit**

```bash
cd ~/.zeroclaw
git add workspace/skills/telegram-reader/scripts/submit_contacts.py
git commit -m "feat(submit-contacts): verbatim gate — reject contacts not found in message_text"
```

---

## Chunk 3: Unit test u8 — verbatim gate in isolation

### Task 3: Add u8 unit test for submit_contacts.py verbatim gate

**Files:**
- Modify: `tests/telegram_search_quality.rs`

Unit tests u1–u7 call telegram_mirror.py via subprocess. u8 calls submit_contacts.py the same way. No daemon needed, no network.

- [ ] **Step 3.1: Find where unit tests are defined**

```bash
grep -n "^async fn u[0-9]" tests/telegram_search_quality.rs | head -10
```

Note the pattern — each u-test calls a subprocess and checks stdout/stderr.

- [ ] **Step 3.2: Add helper for running submit_contacts.py**

Find the subprocess helper used by u1–u7 (likely `run_skill_tool` or similar). If one doesn't exist for submit_contacts, we'll call it directly.

Check:
```bash
grep -n "run_skill\|Command::new\|telegram_mirror" \
  tests/telegram_search_quality.rs | head -20
```

- [ ] **Step 3.3: Add u8 — verbatim gate rejects fabricated contact**

Add after the last u-test (before the first #[ignore] b-test):

```rust
/// u8: submit_contacts.py rejects a contact whose username does not appear in message_text
/// and is not author_contact. No network calls needed (username won't reach HTTP verify
/// because verbatim gate fires first).
///
/// Uses SUBMIT_CONTACTS_SKIP_VERIFY=1 to disable HTTP verification so the test is
/// deterministic (no t.me network dependency).
#[tokio::test]
async fn u8_verbatim_gate_rejects_contact_not_in_message_text() {
    let skill_dir = std::path::PathBuf::from(
        std::env::var("HOME").unwrap_or_else(|_| "/root".into()),
    )
    .join(".zeroclaw/workspace/skills/telegram-reader");

    let contacts_json = serde_json::json!({
        "contacts": [{
            "username_or_phone": "@totally_fake_user_xyz_123",
            "description": "тест",
            "date": "2026-03-11",
            "source_url": null,
            "message_text": "Ищу сантехника, никаких контактов здесь нет.",
            "author_contact": null,
            "media": null
        }]
    })
    .to_string();

    let output = tokio::process::Command::new("python3")
        .arg("scripts/submit_contacts.py")
        .arg(&contacts_json)
        .current_dir(&skill_dir)
        .env("SUBMIT_CONTACTS_SKIP_VERIFY", "1")
        .env("TELEGRAM_BOT_TOKEN", "")
        .env("TELEGRAM_OPERATOR_CHAT_ID", "")
        .output()
        .await
        .expect("failed to run submit_contacts.py");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stderr.contains("REJECTED verbatim-missing"),
        "Expected verbatim gate rejection in stderr, got:\nstderr: {stderr}\nstdout: {stdout}"
    );
    assert!(
        !stdout.contains("@totally_fake_user_xyz_123"),
        "Rejected contact must not appear in stdout, got:\n{stdout}"
    );
}

/// u9: submit_contacts.py accepts a contact whose username appears verbatim in message_text
#[tokio::test]
async fn u9_verbatim_gate_accepts_contact_in_message_text() {
    let skill_dir = std::path::PathBuf::from(
        std::env::var("HOME").unwrap_or_else(|_| "/root".into()),
    )
    .join(".zeroclaw/workspace/skills/telegram-reader");

    let contacts_json = serde_json::json!({
        "contacts": [{
            "username_or_phone": "@Garyxz",
            "description": "Мастер на час",
            "date": "2026-03-05",
            "source_url": null,
            "message_text": "Строительные работы. Пишите в лс @Garyxz. Пхукет.",
            "author_contact": "@Garyxz",
            "media": null
        }]
    })
    .to_string();

    let output = tokio::process::Command::new("python3")
        .arg("scripts/submit_contacts.py")
        .arg(&contacts_json)
        .current_dir(&skill_dir)
        .env("SUBMIT_CONTACTS_SKIP_VERIFY", "1")
        .env("TELEGRAM_BOT_TOKEN", "")
        .env("TELEGRAM_OPERATOR_CHAT_ID", "")
        .output()
        .await
        .expect("failed to run submit_contacts.py");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stderr.contains("ACCEPTED"),
        "Expected ACCEPTED in stderr, got:\nstderr: {stderr}\nstdout: {stdout}"
    );
    assert!(
        stdout.contains("@Garyxz"),
        "Accepted contact must appear in stdout, got:\n{stdout}"
    );
    assert!(
        stdout.contains("Дата: 2026-03-05"),
        "Output must contain Дата: field, got:\n{stdout}"
    );
}
```

- [ ] **Step 3.4: Run u8 and u9**

```bash
cd /home/spex/work/erp/zeroclaws
cargo test --test telegram_search_quality -- u8 u9 --nocapture 2>&1
```

Expected:
```
test u8_verbatim_gate_rejects_contact_not_in_message_text ... ok
test u9_verbatim_gate_accepts_contact_in_message_text ... ok
```

If u8 fails with "REJECTED verbatim-missing not in stderr" — the verbatim gate function wasn't reached. Check that `validate_and_format_contact` is called (not the old `format_contact`).

- [ ] **Step 3.5: Run all unit tests to verify no regression**

```bash
cargo test --test telegram_search_quality -- u1 u2 u3 u4 u5 u6 u7 u8 u9 --nocapture 2>&1
```

Expected: 9 passed, 0 failed.

- [ ] **Step 3.6: Commit**

```bash
cd /home/spex/work/erp/zeroclaws
git add tests/telegram_search_quality.rs
git commit -m "test(telegram): add u8+u9 unit tests for submit_contacts verbatim gate"
```

---

## Chunk 4: E2E test b10 — bot contacts are verbatim in quotes

### Task 4: Add b10 E2E test

**Files:**
- Modify: `tests/telegram_search_quality.rs`

b10 sends a real query to the bot, waits for reply, then asserts that every `@username` and phone number in the response appears literally in the quote block (`> ...`) directly below it.

- [ ] **Step 4.1: Add helper `assert_contacts_verbatim_in_quotes`**

Add this helper function near the other helpers at the top of telegram_search_quality.rs:

```rust
/// Parse bot reply and verify each contact (@username or phone) appears verbatim
/// in the quote block ("> ...") immediately following it.
///
/// Format expected:
///   **@username** — description
///   > quote line 1
///   > quote line 2
///   Дата: YYYY-MM-DD
///   Источник: ...
fn assert_contacts_verbatim_in_quotes(text: &str) {
    // Split into contact blocks: delimiter is blank line
    let blocks: Vec<&str> = text.split("\n\n").collect();

    let username_re = regex::Regex::new(r"\*\*(@[A-Za-z][A-Za-z0-9_]{3,})\*\*").unwrap();
    let phone_re = regex::Regex::new(r"\*\*(\+?[0-9]{7,})\*\*").unwrap();

    let mut checked = 0;

    for block in &blocks {
        // Extract contact identifier from first line
        let first_line = block.lines().next().unwrap_or("");
        let contact = if let Some(cap) = username_re.captures(first_line) {
            cap[1].to_string()
        } else if let Some(cap) = phone_re.captures(first_line) {
            cap[1].to_string()
        } else {
            continue; // not a contact block
        };

        // Collect quote lines ("> ...")
        let quote: String = block
            .lines()
            .filter(|l| l.starts_with("> "))
            .map(|l| &l[2..])
            .collect::<Vec<_>>()
            .join(" ");

        if quote.is_empty() {
            // No quote block — might be author_contact path, skip verbatim check
            // but log for visibility
            println!("b10: contact {contact} has no quote block — skipping verbatim check");
            continue;
        }

        let contact_clean = contact.trim_start_matches('@').to_lowercase();
        let quote_lower = quote.to_lowercase();
        let digits_contact = contact.chars().filter(|c| c.is_ascii_digit()).collect::<String>();

        let found = if contact.starts_with('@') {
            quote_lower.contains(&contact_clean)
        } else if digits_contact.len() >= 7 {
            let quote_digits: String = quote.chars().filter(|c| c.is_ascii_digit()).collect();
            quote_digits.contains(&digits_contact)
        } else {
            true // too short to judge
        };

        assert!(
            found,
            "Contact {contact:?} not found verbatim in quote block:\n{quote}\n\nFull block:\n{block}"
        );
        checked += 1;
    }

    assert!(
        checked > 0,
        "No contact blocks found in reply — cannot verify verbatim property.\nFull reply:\n{text}"
    );

    println!("b10: verified {checked} contact(s) verbatim in quotes ✓");
}
```

Note: this requires adding `regex` to `[dev-dependencies]` if not already present. Check:

```bash
grep "regex" /home/spex/work/erp/zeroclaws/Cargo.toml
```

If missing, add:
```toml
[dev-dependencies]
regex = "1"
```

- [ ] **Step 4.2: Add b10 test**

```rust
/// b10: every contact in bot reply appears verbatim in its quote block.
///
/// This is the structural E2E test for the verbatim gate. If the model
/// hallucinated a contact and fabricated a non-matching quote, this test fails.
///
/// Uses Самуи сантехник as the query because we have joined channels with real data.
#[tokio::test]
#[ignore = "requires live daemon + authorized zverozabr_session"]
async fn b10_contacts_are_verbatim_in_quote_blocks() {
    let bot = "zGsR_bot";
    let query = "Найди сантехника на Самуи. Нужны контакты с цитатой объявления.";

    println!("Sending to @{bot}: {query}");
    let sent_id = send_to_bot(bot, query).await;
    println!("Sent message id={sent_id}");

    let start = std::time::Instant::now();
    let reply = wait_for_bot_reply(bot, sent_id, Duration::from_secs(900)).await;
    println!("Elapsed: {}s", start.elapsed().as_secs());

    let text = reply.unwrap_or_else(|| {
        panic!(
            "Bot @{bot} did not reply within 900s after message id={sent_id}. \
             Check daemon logs: /tmp/zeroclaw_daemon.log"
        )
    });

    println!("Bot reply:\n{text}");

    // Must have at least one contact
    let has_contact = text.contains('@') || contains_phone_number(&text);
    assert!(
        has_contact,
        "Bot reply must contain at least one contact, got:\n{text}"
    );

    // Must have date field
    assert!(
        has_date_field(&text),
        "Bot reply must contain Дата: field, got:\n{text}"
    );

    // Core assertion: every contact is verbatim in its quote
    assert_contacts_verbatim_in_quotes(&text);
}
```

- [ ] **Step 4.3: Compile check (no run)**

```bash
cd /home/spex/work/erp/zeroclaws
cargo test --test telegram_search_quality --no-run 2>&1
```

Expected: compiles without errors. Fix any Rust type/lifetime errors before continuing.

- [ ] **Step 4.4: Update system_prompt in config.toml to reference contact_candidates**

File: `~/.zeroclaw/config.toml`, inside `[agents.telegram_searcher]` system_prompt.

Find the verbatim data rules block and add:

```
- contact_candidates: each search result now includes a pre-extracted list of @usernames and phones found in message text. Use ONLY values from contact_candidates as username_or_phone. If contact_candidates is empty, use author_contact.
- NEVER invent a username_or_phone that does not appear in contact_candidates OR author_contact of the source message.
```

Edit the relevant section:

```bash
grep -n "contact_candidates\|NEVER invent" ~/.zeroclaw/config.toml
```

If `contact_candidates` not yet mentioned, add after "username_or_phone: MUST appear literally":

```
- contact_candidates: field in each search result lists @usernames and phones extracted verbatim from message text. Use ONLY those values (or author_contact) as username_or_phone.
```

- [ ] **Step 4.5: Restart daemon**

```bash
pkill zeroclaw; sleep 2
set -a && source /home/spex/work/erp/zeroclaws/.env && set +a
nohup /home/spex/work/erp/zeroclaws/target/release/zeroclaw daemon \
  >> /tmp/zeroclaw_daemon.log 2>&1 &
sleep 3 && pgrep -a zeroclaw
```

- [ ] **Step 4.6: Run b10 live**

```bash
cd /home/spex/work/erp/zeroclaws
set -a && source .env && set +a
cargo test --test telegram_search_quality -- --ignored b10 --test-threads=1 --nocapture 2>&1
```

Expected: PASS with output like:
```
b10: verified 2 contact(s) verbatim in quotes ✓
test b10_contacts_are_verbatim_in_quote_blocks ... ok
```

If FAIL with "Contact @X not found verbatim in quote block" — the verbatim gate in submit_contacts.py is not deployed or daemon wasn't restarted. Check `[pipeline] REJECTED` lines in `/tmp/zeroclaw_daemon.log`.

- [ ] **Step 4.7: Commit all**

```bash
cd /home/spex/work/erp/zeroclaws
git add tests/telegram_search_quality.rs Cargo.toml
git commit -m "test(telegram): add b10 E2E test — contacts verbatim in quote blocks"
```

---

## Chunk 5: Timeout fix for b6 Phuket

### Task 5: Fix b6 — mark as known-slow, add to regression suite

**Files:**
- Modify: `tests/telegram_search_quality.rs`

b6 failed at 600s (now 900s after our earlier fix). Root cause: Phuket has few joined channels (only `JUNGCEYLON`, `itphuket`) → search takes many iterations. The timeout fix is already done; this task adds a comment and a check that the reply doesn't contain Самуи contacts.

- [ ] **Step 5.1: Add geo-mismatch assertion to b6**

Find `b6_phuket_search_returns_contacts`. After the existing `has_contact` assert, add:

```rust
// Geo check: if contacts found, none should come from Самуи-specific channels
// (soft check — just log, don't fail, since it's hard to enforce via text parsing)
if text.contains("SamuiGroup") || text.contains("samui0") || text.contains("samui3") {
    println!(
        "WARNING b6: reply mentions Самуи channels — possible geo mismatch:\n{text}"
    );
}
```

This makes the issue visible in test output without making the test flaky.

- [ ] **Step 5.2: Commit**

```bash
cd /home/spex/work/erp/zeroclaws
git add tests/telegram_search_quality.rs
git commit -m "test(telegram): b6 add geo-mismatch warning, timeout already 900s"
```

---

---

## Chunk 6: Blocking pipeline — staged execution in agent loop (ROOT CAUSE FIX)

### Task 6: Staged tool execution — search tools before terminal tools

**Files:**
- Modify: `src/agent/loop_/execution.rs`

**Root cause confirmed in daemon logs:**
```
07:51:40.145  invoke  submit_contacts   ← same batch as 10 search_global calls
07:51:42.250  done    submit_contacts   ← 2s later, searches still running
07:51:44.865  done    search_global     ← 4.7s later — real results IGNORED
```

The fix: when a parallel batch contains both "search-phase" tools and "terminal" tools (`submit_contacts`), execute them in two stages — searches first, terminal after. This is a structural guarantee independent of LLM behaviour or prompt instructions.

**Definitions:**
- **Search-phase tools** (prefix match): `telegram_search_`, `telegram_list_`, `telegram_join_`, `bg_run`, `bg_status`
- **Terminal tools** (exact match): `submit_contacts`

When a batch contains only search tools, or only terminal tools → existing behaviour unchanged. Only mixed batches are staged.

- [ ] **Step 6.1: Read execute_tools_parallel in execution.rs**

```bash
grep -n "execute_tools_parallel\|join_all\|fn is_" \
  src/agent/loop_/execution.rs
```

Confirm the function signature and imports (`futures_util`).

- [ ] **Step 6.2: Add `is_terminal_tool` and `is_search_phase_tool` predicates**

Add after `find_tool`:

```rust
/// Tools that must run AFTER all search-phase tools in the same batch complete.
/// Calling these in parallel with searches causes hallucination (model fabricates
/// contacts before search results arrive).
fn is_terminal_tool(name: &str) -> bool {
    matches!(name, "submit_contacts")
}

/// Tools that gather data for the current agent turn.
fn is_search_phase_tool(name: &str) -> bool {
    name.starts_with("telegram_search_")
        || name.starts_with("telegram_list_")
        || name.starts_with("telegram_join_")
        || name.starts_with("bg_")
}
```

- [ ] **Step 6.3: Add `execute_tools_staged` function**

Add after `execute_tools_sequential`:

```rust
/// Staged execution: run search-phase tools first (in parallel), then terminal tools.
///
/// Prevents submit_contacts from running before search results arrive when the
/// model fires both in the same parallel batch.
pub(super) async fn execute_tools_staged(
    tool_calls: &[ParsedToolCall],
    tools_registry: &[Box<dyn Tool>],
    observer: &dyn Observer,
    cancellation_token: Option<&CancellationToken>,
    session_recorder: Option<&SessionRecorder>,
) -> Result<Vec<ToolExecutionOutcome>> {
    // Partition: search-phase tools first, terminal tools second.
    // Unknown tools (neither category) run in stage 1 alongside searches.
    let (terminal, non_terminal): (Vec<_>, Vec<_>) =
        tool_calls.iter().partition(|c| is_terminal_tool(&c.name));

    // Stage 1: all non-terminal tools in parallel
    let mut outcomes: Vec<(usize, ToolExecutionOutcome)> = Vec::new();

    if !non_terminal.is_empty() {
        let stage1_futures: Vec<_> = non_terminal
            .iter()
            .map(|call| {
                execute_one_tool(
                    &call.name,
                    call.arguments.clone(),
                    tools_registry,
                    observer,
                    cancellation_token,
                    session_recorder,
                )
            })
            .collect();
        let stage1_results = futures_util::future::join_all(stage1_futures).await;
        for (call, result) in non_terminal.iter().zip(stage1_results) {
            // Find original index to preserve result ordering
            let orig_idx = tool_calls
                .iter()
                .position(|c| std::ptr::eq(*c, *call))
                .unwrap_or(0);
            outcomes.push((orig_idx, result?));
        }
    }

    // Stage 2: terminal tools sequentially (after all searches complete)
    for call in &terminal {
        let orig_idx = tool_calls
            .iter()
            .position(|c| std::ptr::eq(*c, call))
            .unwrap_or(tool_calls.len());
        let result = execute_one_tool(
            &call.name,
            call.arguments.clone(),
            tools_registry,
            observer,
            cancellation_token,
            session_recorder,
        )
        .await?;
        outcomes.push((orig_idx, result));
    }

    // Restore original ordering so tool_use_id mapping is correct
    outcomes.sort_by_key(|(idx, _)| *idx);
    Ok(outcomes.into_iter().map(|(_, o)| o).collect())
}
```

- [ ] **Step 6.4: Wire staged execution in `should_execute_tools_in_parallel` or call site**

Find where `execute_tools_parallel` is called (likely in `loop_.rs`). The call site checks `should_execute_tools_in_parallel` first.

```bash
grep -n "execute_tools_parallel\|execute_tools_sequential\|should_execute" \
  src/agent/loop_.rs | head -20
```

Modify the dispatch logic. Currently it's likely:
```rust
if should_execute_tools_in_parallel(&tool_calls, approval.as_ref()) {
    execute_tools_parallel(...).await?
} else {
    execute_tools_sequential(...).await?
}
```

Change to:
```rust
let has_terminal = tool_calls.iter().any(|c| is_terminal_tool(&c.name));
let has_search = tool_calls.iter().any(|c| is_search_phase_tool(&c.name));

if has_terminal && has_search && tool_calls.len() > 1 {
    // Mixed batch: search tools must complete before submit_contacts runs
    tracing::info!(
        "tool.staged_execution mixed_batch=true terminal={} search={}",
        tool_calls.iter().filter(|c| is_terminal_tool(&c.name)).count(),
        tool_calls.iter().filter(|c| is_search_phase_tool(&c.name)).count(),
    );
    execute_tools_staged(&tool_calls, tools_registry, observer, cancellation_token, session_recorder).await?
} else if should_execute_tools_in_parallel(&tool_calls, approval.as_ref()) {
    execute_tools_parallel(&tool_calls, tools_registry, observer, cancellation_token, session_recorder).await?
} else {
    execute_tools_sequential(&tool_calls, tools_registry, observer, cancellation_token, session_recorder).await?
}
```

Note: `is_terminal_tool` and `is_search_phase_tool` must be pub(super) or in scope. Add `pub(super)` to them if the call site is in loop_.rs.

- [ ] **Step 6.5: Compile**

```bash
cd /home/spex/work/erp/zeroclaws
cargo build --release 2>&1 | tail -20
```

Expected: compiles without errors. Common error: `is_terminal_tool` not in scope — add `use super::execution::{is_terminal_tool, is_search_phase_tool, execute_tools_staged};` in loop_.rs.

- [ ] **Step 6.6: Run unit tests**

```bash
cargo test --test telegram_search_quality -- u1 u2 u3 u4 u5 u6 u7 u8 u9 --nocapture 2>&1
```

Expected: 9 passed.

- [ ] **Step 6.7: Restart daemon and verify staged execution in logs**

```bash
pkill zeroclaw; sleep 2
set -a && source .env && set +a
nohup ./target/release/zeroclaw daemon >> /tmp/zeroclaw_daemon.log 2>&1 &
sleep 3 && pgrep -a zeroclaw
```

Send a quick test via bot that would trigger mixed batch. Then check logs:

```bash
grep "tool.staged_execution\|staged" /tmp/zeroclaw_daemon.log | tail -5
```

Expected: `tool.staged_execution mixed_batch=true terminal=1 search=N`

- [ ] **Step 6.8: Commit**

```bash
cd /home/spex/work/erp/zeroclaws
git add src/agent/loop_/execution.rs src/agent/loop_.rs
git commit -m "fix(agent): staged tool execution — search tools complete before submit_contacts runs"
```

---

## Validation

After all chunks complete, run the full unit suite and b10. Two independent defence layers now active: (1) staged execution blocks submit_contacts until searches finish; (2) verbatim gate rejects any contact not in message_text.



```bash
cd /home/spex/work/erp/zeroclaws

# Unit tests — no daemon needed
cargo test --test telegram_search_quality -- u1 u2 u3 u4 u5 u6 u7 u8 u9 --nocapture

# E2E — daemon must be running
set -a && source .env && set +a
cargo test --test telegram_search_quality -- --ignored b10 --test-threads=1 --nocapture
```

Expected final state:
- u1–u9: 9 passed
- b10: PASS (contacts verbatim in quotes)
- Daemon log shows `[pipeline] REJECTED verbatim-missing` for any fabricated contacts
- Daemon log shows `[pipeline] SUMMARY: total=N accepted=M rejected=K`

---

## Risk

| Risk | Mitigation |
|------|-----------|
| `contact_candidates` regex misses phone formats | Conservative regex; phones missed → fall back to author_contact path |
| Verbatim gate rejects real contact with short message | Real messages from search tools are typically >50 chars |
| b10 flaky if Gemini rate-limited | Run with 90s pause after other tests; 900s timeout |
| Model still fires parallel batch | Verbatim gate now rejects fabricated data regardless |

## Rollback

```bash
# If verbatim gate breaks production:
cd ~/.zeroclaw
git revert HEAD  # reverts submit_contacts.py change
pkill zeroclaw && nohup ./target/release/zeroclaw daemon >> /tmp/zeroclaw_daemon.log 2>&1 &
```
