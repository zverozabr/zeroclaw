"""
Pi Multi-Instance Isolation Test
Tests that two forum topics in the same private chat get INDEPENDENT Pi instances.

Run: ~/.zeroclaw/workspace/.venv/bin/python3 test_pi_isolation.py
"""
import asyncio
import pathlib
import random
import re
import time

from telethon import TelegramClient
from telethon.tl.functions.messages import (
    CreateForumTopicRequest,
    GetForumTopicsRequest,
    DeleteTopicHistoryRequest,
)

SESSION_PATH = str(
    pathlib.Path.home()
    / ".zeroclaw/workspace/skills/telegram-reader/.session/zverozabr_session"
)
API_ID = 38309428
API_HASH = "1f9a006d55531cfd387246cd0fff83f8"
BOT_ID = 8527746065  # @zGsR_bot
LOG_PATH = "/tmp/zeroclaw_daemon.log"

TIMEOUT = 120  # seconds


def step(label: str, ok: bool, detail: str = "") -> bool:
    mark = "PASS" if ok else "FAIL"
    print(f"  [{mark}] {label}" + (f" — {detail}" if detail else ""))
    return ok


async def get_or_create_topic(client, bot, name: str) -> int:
    """Find existing topic by name or create a new one. Returns topic_id."""
    try:
        result = await client(
            GetForumTopicsRequest(
                peer=bot,
                offset_date=0,
                offset_id=0,
                offset_topic=0,
                limit=100,
                q=name,
            )
        )
        for topic in result.topics:
            if hasattr(topic, "title") and topic.title == name:
                print(f"    [found existing topic] {name!r} → id={topic.id}")
                return topic.id
    except Exception as e:
        print(f"    [topic search error] {e}")
    # Create new
    result = await client(
        CreateForumTopicRequest(
            peer=bot,
            title=name,
            random_id=random.randint(1, 2**31),
        )
    )
    for update in result.updates:
        if hasattr(update, "id") and type(update).__name__ == "UpdateMessageID":
            print(f"    [created topic] {name!r} → id={update.id}")
            return update.id
    raise RuntimeError(f"Could not get topic_id from: {result}")


def _is_final_reply(msg_text: str) -> bool:
    if not msg_text:
        return False
    if "⏳" in msg_text:
        return False
    if msg_text.startswith("⏳") or msg_text.startswith("⚙"):
        return False
    return True


async def send_and_wait(client, bot, topic_id: int, text: str, timeout=TIMEOUT) -> str:
    """Send message in topic thread, wait for final bot reply. Returns reply text."""
    sent = await client.send_message(bot, text, reply_to=topic_id)
    deadline = time.monotonic() + timeout
    last_bot_msg_id = None
    while time.monotonic() < deadline:
        await asyncio.sleep(5)
        msgs = await client.get_messages(bot, limit=15, min_id=sent.id)
        for msg in reversed(msgs):
            if msg.id <= sent.id:
                continue
            sender = await msg.get_sender()
            if not (sender and getattr(sender, "bot", False)):
                continue
            reply_to = getattr(msg, "reply_to", None)
            if reply_to:
                top = getattr(reply_to, "reply_to_top_id", None) or getattr(
                    reply_to, "reply_to_msg_id", None
                )
                if top != topic_id:
                    continue
            if last_bot_msg_id is None or msg.id > last_bot_msg_id:
                last_bot_msg_id = msg.id
        if last_bot_msg_id is not None:
            await asyncio.sleep(5)
            msgs = await client.get_messages(bot, limit=5, min_id=sent.id)
            for msg in reversed(msgs):
                if msg.id == last_bot_msg_id:
                    if _is_final_reply(msg.text or ""):
                        return msg.text or ""
                    break  # still in-progress, keep waiting
    return ""


async def delete_topic(client, bot, topic_id: int, name: str) -> None:
    try:
        await client(DeleteTopicHistoryRequest(peer=bot, top_msg_id=topic_id))
        print(f"    [cleanup] Deleted topic {name!r} (id={topic_id})")
    except Exception as e:
        print(f"    [cleanup] Could not delete topic {name!r}: {e}")


def grep_log_for_history_keys(log_path: str, since_line: int) -> list[str]:
    """Extract unique history_keys seen in the daemon log from `since_line` onward."""
    try:
        with open(log_path, "r", errors="replace") as f:
            lines = f.readlines()
        new_lines = lines[since_line:]
        keys = set()
        for line in new_lines:
            m = re.search(r'history_key="([^"]+)"', line)
            if m:
                keys.add(m.group(1))
        return sorted(keys)
    except Exception as e:
        print(f"    [log error] {e}")
        return []


def get_log_line_count(log_path: str) -> int:
    try:
        with open(log_path, "r", errors="replace") as f:
            return sum(1 for _ in f)
    except Exception:
        return 0


async def run_test():
    print("=" * 60)
    print("Pi Multi-Instance Isolation Test")
    print("=" * 60)

    results = {}

    client = TelegramClient(SESSION_PATH, API_ID, API_HASH)
    await client.connect()
    bot = await client.get_entity(BOT_ID)
    print(f"Connected as: {(await client.get_me()).username}")
    print(f"Bot entity: {bot.username} (id={bot.id})")

    # Record log position before test
    log_start_line = get_log_line_count(LOG_PATH)
    print(f"\nDaemon log current size: {log_start_line} lines")

    # Step 1: Create/find two forum topics
    print("\n[STEP 1] Create/find forum topics pi-test-A and pi-test-B")
    topic_a = await get_or_create_topic(client, bot, "pi-test-A")
    topic_b = await get_or_create_topic(client, bot, "pi-test-B")
    results["s1_topics"] = step(
        "Topics created",
        topic_a != topic_b,
        f"A={topic_a}, B={topic_b}",
    )

    # Step 2: Activate Pi in Topic A
    print("\n[STEP 2] Activate Pi in Topic A")
    reply_a_models = await send_and_wait(client, bot, topic_a, "/models pi", timeout=60)
    print(f"    Topic A /models pi reply: {reply_a_models[:200]!r}")
    results["s2_pi_a"] = step(
        "Pi activated in Topic A",
        len(reply_a_models) > 0,
        reply_a_models[:100],
    )

    # Step 3: Activate Pi in Topic B
    print("\n[STEP 3] Activate Pi in Topic B")
    reply_b_models = await send_and_wait(client, bot, topic_b, "/models pi", timeout=60)
    print(f"    Topic B /models pi reply: {reply_b_models[:200]!r}")
    results["s3_pi_b"] = step(
        "Pi activated in Topic B",
        len(reply_b_models) > 0,
        reply_b_models[:100],
    )

    # Step 4: Store word in Topic A
    print("\n[STEP 4] Topic A: запомни слово ALPHA-TOPIC-A")
    reply_a_store = await send_and_wait(
        client, bot, topic_a, "запомни слово ALPHA-TOPIC-A"
    )
    print(f"    Topic A store reply: {reply_a_store[:200]!r}")
    results["s4_store_a"] = step(
        "Topic A accepted store command",
        len(reply_a_store) > 0,
        reply_a_store[:100],
    )

    # Step 5: Store word in Topic B
    print("\n[STEP 5] Topic B: запомни слово BETA-TOPIC-B")
    reply_b_store = await send_and_wait(
        client, bot, topic_b, "запомни слово BETA-TOPIC-B"
    )
    print(f"    Topic B store reply: {reply_b_store[:200]!r}")
    results["s5_store_b"] = step(
        "Topic B accepted store command",
        len(reply_b_store) > 0,
        reply_b_store[:100],
    )

    # Step 6: Recall from Topic A — must have ALPHA, must NOT have BETA
    print("\n[STEP 6] Topic A: какое слово ты запомнил?")
    reply_a_recall = await send_and_wait(
        client, bot, topic_a, "какое слово ты запомнил?"
    )
    print(f"    Topic A recall reply: {reply_a_recall[:300]!r}")
    has_alpha = "ALPHA" in reply_a_recall.upper() or "alpha" in reply_a_recall.lower()
    has_beta_in_a = "BETA" in reply_a_recall.upper()
    results["s6a_alpha"] = step(
        "Topic A recalls ALPHA",
        has_alpha,
        f"found={'yes' if has_alpha else 'NO'}",
    )
    results["s6b_no_beta"] = step(
        "Topic A does NOT recall BETA",
        not has_beta_in_a,
        f"beta_found={'YES (FAIL)' if has_beta_in_a else 'no'}",
    )

    # Step 7: Recall from Topic B — must have BETA, must NOT have ALPHA
    print("\n[STEP 7] Topic B: какое слово ты запомнил?")
    reply_b_recall = await send_and_wait(
        client, bot, topic_b, "какое слово ты запомнил?"
    )
    print(f"    Topic B recall reply: {reply_b_recall[:300]!r}")
    has_beta = "BETA" in reply_b_recall.upper() or "beta" in reply_b_recall.lower()
    has_alpha_in_b = "ALPHA" in reply_b_recall.upper()
    results["s7a_beta"] = step(
        "Topic B recalls BETA",
        has_beta,
        f"found={'yes' if has_beta else 'NO'}",
    )
    results["s7b_no_alpha"] = step(
        "Topic B does NOT recall ALPHA",
        not has_alpha_in_b,
        f"alpha_found={'YES (FAIL)' if has_alpha_in_b else 'no'}",
    )

    # Step 8: Deactivate Pi in both topics
    print("\n[STEP 8] Deactivate Pi in both topics")
    reply_a_deact = await send_and_wait(client, bot, topic_a, "/models minimax", timeout=60)
    print(f"    Topic A deactivate reply: {reply_a_deact[:150]!r}")
    reply_b_deact = await send_and_wait(client, bot, topic_b, "/models minimax", timeout=60)
    print(f"    Topic B deactivate reply: {reply_b_deact[:150]!r}")
    results["s8_deactivate"] = step(
        "Deactivated Pi in both topics",
        len(reply_a_deact) > 0 and len(reply_b_deact) > 0,
        f"A={reply_a_deact[:50]!r}, B={reply_b_deact[:50]!r}",
    )

    # Step 9: Check daemon log for two different history_keys
    print("\n[STEP 9] Checking daemon log for two distinct Pi history_keys")
    await asyncio.sleep(3)  # let log flush
    history_keys = grep_log_for_history_keys(LOG_PATH, log_start_line)
    print(f"    history_keys seen in this test run:")
    for k in history_keys:
        print(f"      - {k!r}")
    # Filter for pi-related keys (should contain topic_a/topic_b ids as part of key)
    # The history_key format: telegram_{chat_id}:{thread_id}_{thread_id}_{sender}
    # We expect two distinct keys, one containing topic_a id, one containing topic_b id
    topic_a_keys = [k for k in history_keys if str(topic_a) in k]
    topic_b_keys = [k for k in history_keys if str(topic_b) in k]
    two_distinct = len(topic_a_keys) >= 1 and len(topic_b_keys) >= 1 and set(topic_a_keys).isdisjoint(set(topic_b_keys))
    results["s9_distinct_keys"] = step(
        "Two distinct history_keys in daemon log",
        two_distinct,
        f"A-keys={topic_a_keys}, B-keys={topic_b_keys}",
    )

    # Cleanup
    print("\n[CLEANUP] Deleting test topics")
    await delete_topic(client, bot, topic_a, "pi-test-A")
    await delete_topic(client, bot, topic_b, "pi-test-B")

    await client.disconnect()

    # Summary
    print("\n" + "=" * 60)
    print("SUMMARY")
    print("=" * 60)
    all_passed = True
    for key, passed in results.items():
        mark = "PASS" if passed else "FAIL"
        print(f"  [{mark}] {key}")
        if not passed:
            all_passed = False
    print()
    print(f"OVERALL: {'ALL PASS' if all_passed else 'SOME FAILURES'}")
    print("=" * 60)


if __name__ == "__main__":
    asyncio.run(run_test())
