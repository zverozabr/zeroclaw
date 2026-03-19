#!/usr/bin/env python3
"""
Telegram progress E2E helper for pm_24_telegram_progress_trimming.

Sends a message to the bot via Telegram, monitors edited messages,
and checks that progress trimming (... +N) is applied.

Usage:
    python3 tests/telegram_progress_e2e.py

Env vars:
    TELEGRAM_API_ID, TELEGRAM_API_HASH — Telegram app credentials
    TELEGRAM_SESSION — path to .session file

Outputs JSON to stdout:
    {"progress_trimmed": true/false, "progress_edits": N, "saw_plus_marker": true/false, ...}
"""

import asyncio
import json
import os
from pathlib import Path

from telethon import TelegramClient, events

BOT_USERNAME = "zGsR_bot"
# Task that triggers >10 tool calls across multiple iterations.
# Uses memory_store/recall pairs that chain — each recall depends on previous store.
MESSAGE = (
    "Выполни ровно 15 шагов, каждый — ОДИН вызов инструмента:\n"
    "1. memory_store key='test_step' value='1'\n"
    "2. memory_recall query='test_step'\n"
    "3. memory_store key='test_step' value='2'\n"
    "4. memory_recall query='test_step'\n"
    "5. memory_store key='test_step' value='3'\n"
    "6. memory_recall query='test_step'\n"
    "7. memory_store key='test_step' value='4'\n"
    "8. memory_recall query='test_step'\n"
    "9. memory_store key='test_step' value='5'\n"
    "10. memory_recall query='test_step'\n"
    "11. memory_store key='test_step' value='6'\n"
    "12. memory_recall query='test_step'\n"
    "13. provider_status\n"
    "14. provider_health\n"
    "15. memory_store key='test_step' value='done'\n"
    "ВАЖНО: выполняй СТРОГО по одному, НЕ группируй вызовы. "
    "После каждого шага жди результат перед следующим. "
    "В конце напиши 'Выполнено 15 шагов'."
)
TIMEOUT_SECS = 600
# After last edit/message, wait this long before declaring "done"
IDLE_SECS = 60


async def main():
    api_id = int(os.environ["TELEGRAM_API_ID"])
    api_hash = os.environ["TELEGRAM_API_HASH"]
    session_path = os.environ.get(
        "TELEGRAM_SESSION",
        str(
            Path.home()
            / ".zeroclaw/workspace/skills/telegram-reader/.session/zverozabr_session.session"
        ),
    )
    if session_path.endswith(".session"):
        session_path = session_path[: -len(".session")]

    result = {
        "progress_trimmed": False,
        "progress_edits": 0,
        "total_edits": 0,
        "max_progress_lines": 0,
        "saw_plus_marker": False,
        "final_text": "",
        "error": None,
    }

    client = TelegramClient(session_path, api_id, api_hash)
    await client.start()

    try:
        bot = await client.get_entity(BOT_USERNAME)

        progress_edits = 0
        total_edits = 0
        saw_plus = False
        max_progress_lines = 0
        done_event = asyncio.Event()
        final_text = ""
        last_activity = asyncio.get_event_loop().time()

        def is_progress_text(text: str) -> bool:
            """Progress messages contain ⏳ or ✅ tool-call status lines."""
            return "⏳" in text or "✅" in text

        @client.on(events.MessageEdited(chats=bot))
        async def on_edit(event):
            nonlocal total_edits, progress_edits, saw_plus, max_progress_lines
            nonlocal final_text, last_activity
            text = event.message.text or ""
            total_edits += 1
            last_activity = asyncio.get_event_loop().time()

            if is_progress_text(text):
                progress_edits += 1
                lines = text.strip().split("\n")
                if len(lines) > max_progress_lines:
                    max_progress_lines = len(lines)
                if "... +" in text:
                    saw_plus = True
            else:
                # Non-progress edit = final answer replaced progress message
                final_text = text
                done_event.set()

        @client.on(events.NewMessage(chats=bot, incoming=True))
        async def on_new(event):
            nonlocal final_text, last_activity
            text = event.message.text or ""
            last_activity = asyncio.get_event_loop().time()
            if not is_progress_text(text):
                final_text = text
                done_event.set()

        # Send the task
        await client.send_message(bot, MESSAGE)

        # Wait for completion: either done_event or idle timeout after activity
        deadline = asyncio.get_event_loop().time() + TIMEOUT_SECS
        while asyncio.get_event_loop().time() < deadline:
            try:
                remaining = min(IDLE_SECS, deadline - asyncio.get_event_loop().time())
                await asyncio.wait_for(done_event.wait(), timeout=remaining)
                # Got done signal, wait a bit more for any trailing edits
                await asyncio.sleep(3)
                break
            except asyncio.TimeoutError:
                # Check if we've been idle long enough after seeing progress
                now = asyncio.get_event_loop().time()
                if progress_edits > 0 and (now - last_activity) > IDLE_SECS:
                    break
                continue

        if progress_edits == 0 and total_edits == 0:
            result["error"] = "timeout: no edits seen from bot"

        result["progress_edits"] = progress_edits
        result["total_edits"] = total_edits
        result["max_progress_lines"] = max_progress_lines
        result["saw_plus_marker"] = saw_plus
        result["progress_trimmed"] = saw_plus
        result["final_text"] = final_text[:500] if final_text else ""

    except Exception as e:
        result["error"] = str(e)
    finally:
        await client.disconnect()

    print(json.dumps(result, ensure_ascii=False))


if __name__ == "__main__":
    asyncio.run(main())
