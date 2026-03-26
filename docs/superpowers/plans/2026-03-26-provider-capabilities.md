# Provider Capabilities Discovery — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** For Moonshot (Kimi) and MiniMax — test every live key against all known endpoints, produce per-key capability maps in `data/capabilities/{provider}.json`.

**Architecture:** Single Python script `test_provider_capabilities.py` that loads live keys from `data/valid/{provider}.json`, probes each endpoint with a minimal request, classifies the response, and saves structured JSON. Supports Moonshot and MiniMax providers across 6 capability categories.

**Tech Stack:** Python 3, `urllib`/`urllib.request`, `json`, `argparse`, `time` — no external deps beyond stdlib.

---

## File Structure

```
~/.zeroclaw/workspace/skills/github-grep/
  scripts/
    test_provider_capabilities.py   # CREATE — main test script
  data/
    capabilities/                   # CREATE — output directory
      moonshot.json                 #   per-key capability maps
      minimax.json
    valid/
      moonshot.json                  # READ — live keys input
      minimax.json
```

**key_store.py** — add `--action capabilities` sub-command (modify in Task 6).

---

## Task 1: Create capabilities output directory

**Files:** None (filesystem only)

- [ ] **Step 1: Create directory**

```bash
mkdir -p /home/spex/.zeroclaw/workspace/skills/github-grep/data/capabilities
```

- [ ] **Step 2: Verify**

```bash
ls /home/spex/.zeroclaw/workspace/skills/github-grep/data/capabilities/
```

Expected: empty output (directory created, no files yet)

---

## Task 2: Write endpoint patterns and response classifiers

**Files:**
- Create: `~/.zeroclaw/workspace/skills/github-grep/scripts/test_provider_capabilities.py` (stub + endpoint data)

- [ ] **Step 1: Write stub script with provider configs and endpoint definitions**

```python
#!/usr/bin/env python3
"""
Test provider API capabilities — probe all known endpoints per live key.
Produces data/capabilities/{provider}.json with per-key capability maps.
"""

import argparse
import json
import os
import time
import urllib.error
import urllib.request

DATA_DIR = "/home/spex/.zeroclaw/workspace/skills/github-grep/data"
VALID_DIR = os.path.join(DATA_DIR, "valid")
OUT_DIR = os.path.join(DATA_DIR, "capabilities")


# ─── Provider endpoint definitions ────────────────────────────────────────────

ENDPOINTS = {
    "moonshot": {
        "base": "https://api.moonshot.cn",
        "text": "https://api.moonshot.cn/v1/chat/completions",
        "models": "https://api.moonshot.cn/v1/models",
        "token_count": "https://api.moonshot.cn/v1/tokens/count",
        "file_upload": "https://api.moonshot.cn/v1/files",
        "image_gen": "https://api.moonshot.cn/v1/images/generations",
        "audio_speech": "https://api.moonshot.cn/v1/audio/speech",
        "audio_stt": "https://api.moonshot.cn/v1/audio/transcriptions",
        "balance": "https://api.moonshot.cn/v1/balance",
        "rate_limit": "https://api.moonshot.cn/v1/rate_limit",
    },
    "minimax": {
        "base": "https://api.minimax.chat",
        "text": "https://api.minimax.chat/v1/text/chatcompletion_v2",
        "models": "https://api.minimax.chat/v1/models",
        "token_count": "https://api.minimax.chat/v1/tokens/count",
        "file_upload": "https://api.minimax.chat/v1/files",
        "image_gen": "https://api.minimax.chat/v1/images/generations",
        "video_gen": "https://api.minimax.io/v1/video_generation",
        "audio_tts": "https://api.minimax.chat/v1/speech-generation/t2a",
        "audio_stt": "https://api.minimax.chat/v1/speech-recognition/a2t",
        "voice_cloning": "https://api.minimax.chat/voice/cloning",
        "voice_design": "https://api.minimax.chat/voice/design",
        "balance": "https://api.minimax.chat/v1/balance",
        "rate_limit": "https://api.minimax.chat/v1/rate_limit",
    },
}

# Minimal test requests — just enough to verify endpoint exists and auth works
MINIMAL_REQUESTS = {
    "moonshot": {
        "text": {
            "method": "POST",
            "url": "https://api.moonshot.cn/v1/chat/completions",
            "headers": {"Authorization": "Bearer {key}", "Content-Type": "application/json"},
            "body": '{"model":"moonshot-v1-8k","messages":[{"role":"user","content":"hi"}],"max_tokens":1}',
        },
        "models": {
            "method": "GET",
            "url": "https://api.moonshot.cn/v1/models",
            "headers": {"Authorization": "Bearer {key}"},
            "body": None,
        },
        "token_count": {
            "method": "POST",
            "url": "https://api.moonshot.cn/v1/tokens/count",
            "headers": {"Authorization": "Bearer {key}", "Content-Type": "application/json"},
            "body": '{"model":"moonshot-v1-8k","messages":[{"role":"user","content":"hi"}]}',
        },
        "file_upload": {
            "method": "POST",
            "url": "https://api.moonshot.cn/v1/files",
            "headers": {"Authorization": "Bearer {key}"},
            "body": b"test",  # dummy bytes
        },
        "image_gen": {
            "method": "POST",
            "url": "https://api.moonshot.cn/v1/images/generations",
            "headers": {"Authorization": "Bearer {key}", "Content-Type": "application/json"},
            "body": '{"model":"moonshot-01","prompt":"test","size":"256x256","n":1}',
        },
        "audio_speech": {
            "method": "POST",
            "url": "https://api.moonshot.cn/v1/audio/speech",
            "headers": {"Authorization": "Bearer {key}", "Content-Type": "application/json"},
            "body": '{"model":"speech-01","input":"hi","voice":"male-qn-qingse"}',
        },
        "audio_stt": {
            "method": "POST",
            "url": "https://api.moonshot.cn/v1/audio/transcriptions",
            "headers": {"Authorization": "Bearer {key}"},
            "body": b"test",
        },
        "balance": {
            "method": "GET",
            "url": "https://api.moonshot.cn/v1/balance",
            "headers": {"Authorization": "Bearer {key}"},
            "body": None,
        },
        "rate_limit": {
            "method": "GET",
            "url": "https://api.moonshot.cn/v1/rate_limit",
            "headers": {"Authorization": "Bearer {key}"},
            "body": None,
        },
    },
    "minimax": {
        "text": {
            "method": "POST",
            "url": "https://api.minimax.chat/v1/text/chatcompletion_v2",
            "headers": {"Authorization": "Bearer {key}", "Content-Type": "application/json"},
            "body": '{"model":"MiniMax-Text-01","messages":[{"role":"user","content":"hi"}],"max_tokens":1}',
        },
        "models": {
            "method": "GET",
            "url": "https://api.minimax.chat/v1/models",
            "headers": {"Authorization": "Bearer {key}"},
            "body": None,
        },
        "token_count": {
            "method": "POST",
            "url": "https://api.minimax.chat/v1/tokens/count",
            "headers": {"Authorization": "Bearer {key}", "Content-Type": "application/json"},
            "body": '{"model":"MiniMax-Text-01","messages":[{"role":"user","content":"hi"}]}',
        },
        "file_upload": {
            "method": "POST",
            "url": "https://api.minimax.chat/v1/files",
            "headers": {"Authorization": "Bearer {key}"},
            "body": b"test",
        },
        "image_gen": {
            "method": "POST",
            "url": "https://api.minimax.chat/v1/images/generations",
            "headers": {"Authorization": "Bearer {key}", "Content-Type": "application/json"},
            "body": '{"model":"MiniMax-Image-01","prompt":"test"}',
        },
        "video_gen": {
            "method": "POST",
            "url": "https://api.minimax.io/v1/video_generation",
            "headers": {"Authorization": "Bearer {key}", "Content-Type": "application/json"},
            "body": '{"model":"MiniMax-Hailuo-2.3","first_frame_image":"https://example.com/test.jpg","prompt":"test","duration":6,"resolution":"768P"}',
        },
        "audio_tts": {
            "method": "POST",
            "url": "https://api.minimax.chat/v1/speech-generation/t2a",
            "headers": {"Authorization": "Bearer {key}", "Content-Type": "application/json"},
            "body": '{"model":"speech-01","text":"hi"}',
        },
        "audio_stt": {
            "method": "POST",
            "url": "https://api.minimax.chat/v1/speech-recognition/a2t",
            "headers": {"Authorization": "Bearer {key}"},
            "body": b"test",
        },
        "voice_cloning": {
            "method": "POST",
            "url": "https://api.minimax.chat/voice/cloning",
            "headers": {"Authorization": "Bearer {key}"},
            "body": b"test",
        },
        "voice_design": {
            "method": "POST",
            "url": "https://api.minimax.chat/voice/design",
            "headers": {"Authorization": "Bearer {key}"},
            "body": b"test",
        },
        "balance": {
            "method": "GET",
            "url": "https://api.minimax.chat/v1/balance",
            "headers": {"Authorization": "Bearer {key}"},
            "body": None,
        },
        "rate_limit": {
            "method": "GET",
            "url": "https://api.minimax.chat/v1/rate_limit",
            "headers": {"Authorization": "Bearer {key}"},
            "body": None,
        },
    },
}


def mask_key(key: str) -> str:
    """Show first 8 and last 4 chars."""
    return key[:8] + "..." + key[-4:] if len(key) > 14 else key[:4] + "..."


def _classify_response(code: int, body: str, endpoint_key: str) -> tuple[str, str]:
    """Classify response into: working | invalid | insufficient_balance | no_access | not_found | error"""
    if code in (200, 201):
        try:
            resp = json.loads(body)
            # Async video: task_id + code=0 = working
            if endpoint_key == "video_gen" and resp.get("base_resp", {}).get("status_code") == 0:
                if resp.get("base_resp", {}).get("status_msg") == "insufficient balance":
                    return "insufficient_balance", "insufficient balance"
                return "working", resp.get("base_resp", {}).get("status_msg", "ok")
            # Balance responses
            if endpoint_key in ("balance", "rate_limit"):
                if resp.get("code") == 0 or resp.get("balance_available") is not None:
                    return "working", str(resp)[:100]
                if "insufficient" in str(resp).lower():
                    return "insufficient_balance", str(resp)[:100]
            # Model list
            if endpoint_key == "models":
                if "data" in resp:
                    return "working", str(resp)[:100]
                return "working", str(resp)[:100]
            return "working", str(resp)[:100]
        except json.JSONDecodeError:
            return "working", body[:100] if body else "empty"
    if code == 401 or code == 403:
        body_lower = body.lower()
        if "invalid" in body_lower or "unauthorized" in body_lower:
            return "invalid", body[:100]
        if "insufficient" in body_lower or "balance" in body_lower:
            return "insufficient_balance", body[:100]
        return "no_access", body[:100]
    if code == 402:
        return "insufficient_balance", body[:100]
    if code == 404:
        return "not_found", "endpoint not found"
    if code == 429:
        return "rate_limited", body[:100]
    if code >= 500:
        return "error", f"server error {code}"
    return "error", f"http {code}"


def _do_request(method: str, url: str, headers: dict, body: bytes | None) -> tuple[int, str]:
    """Execute HTTP request, return (status_code, response_body)."""
    hdrs = dict(headers)
    hdrs["User-Agent"] = "ZeroClaw/1.0"
    req = urllib.request.Request(url, headers=hdrs, method=method, data=body)
    try:
        with urllib.request.urlopen(req, timeout=15) as resp:
            return resp.status, resp.read().decode()
    except urllib.error.HTTPError as exc:
        return exc.code, exc.read().decode() if exc.fp else ""
    except Exception as exc:
        return 0, str(exc)


def test_key(provider: str, key: str) -> dict:
    """Test all endpoints for a single key. Returns capability map."""
    requests = MINIMAL_REQUESTS[provider]
    result = {}

    for ep_key, req_cfg in requests.items():
        method = req_cfg["method"]
        url = req_cfg["url"]
        hdrs = {k: v.format(key=key) for k, v in req_cfg["headers"].items()}
        body = req_cfg["body"]
        body_bytes = body.encode() if isinstance(body, str) else body

        code, resp_body = _do_request(method, url, hdrs, body_bytes)
        status, detail = _classify_response(code, resp_body, ep_key)

        result[ep_key] = {
            "status": status,
            "code": code,
            "detail": detail,
        }

        time.sleep(0.5)  # rate limit

    return result


def test_provider(provider: str, limit: int | None = None):
    """Test all live keys for a provider."""
    valid_path = os.path.join(VALID_DIR, f"{provider}.json")
    if not os.path.exists(valid_path):
        print(f"No valid keys for {provider}")
        return

    with open(valid_path) as f:
        valid_keys = json.load(f)

    live_keys = [k for k, v in valid_keys.items() if v.get("status") == "active"]
    if limit:
        live_keys = live_keys[:limit]

    print(f"Testing {len(live_keys)} live keys for {provider}...")

    output = {
        "provider": provider,
        "checked_at": time.strftime("%Y-%m-%dT%H:%M:%S"),
        "keys": {},
    }

    for i, key in enumerate(live_keys):
        print(f"  [{i+1}/{len(live_keys)}] {mask_key(key)}", flush=True)
        caps = test_key(provider, key)
        output["keys"][key] = {
            "masked": mask_key(key),
            "capabilities": caps,
        }

    out_path = os.path.join(OUT_DIR, f"{provider}.json")
    with open(out_path, "w") as f:
        json.dump(output, f, indent=2, ensure_ascii=False)

    print(f"Saved to {out_path}")
    return output


def main():
    parser = argparse.ArgumentParser(description="Test provider API capabilities")
    parser.add_argument("--provider", required=True, choices=["moonshot", "minimax", "all"],
                        help="Provider to test")
    parser.add_argument("--limit", type=int, default=None,
                        help="Limit number of keys to test (for testing)")
    args = parser.parse_args()

    providers = ["moonshot", "minimax"] if args.provider == "all" else [args.provider]
    for prov in providers:
        test_provider(prov, limit=args.limit)


if __name__ == "__main__":
    main()
```

- [ ] **Step 2: Make executable**

```bash
chmod +x /home/spex/.zeroclaw/workspace/skills/github-grep/scripts/test_provider_capabilities.py
```

- [ ] **Step 3: Quick dry-run test with one key**

```bash
cd /home/spex/.zeroclaw/workspace/skills/github-grep && \
~/.zeroclaw/workspace/.venv/bin/python3 scripts/test_provider_capabilities.py --provider moonshot --limit 1
```

Expected: outputs JSON for 1 key, saved to `data/capabilities/moonshot.json`

---

## Task 3: Run Moonshot batch

**Files:**
- Read: `~/.zeroclaw/workspace/skills/github-grep/data/valid/moonshot.json` (57 live keys)
- Write: `~/.zeroclaw/workspace/skills/github-grep/data/capabilities/moonshot.json`

- [ ] **Step 1: Run full moonshot batch**

```bash
cd /home/spex/.zeroclaw/workspace/skills/github-grep && \
~/.zeroclaw/workspace/.venv/bin/python3 scripts/test_provider_capabilities.py --provider moonshot
```

Expected: runs in background (57 keys × ~9 endpoints × 0.5s ≈ 4-5 min). Progress shown per-key.

**Run in background:**
```bash
nohup ~/.zeroclaw/workspace/.venv/bin/python3 scripts/test_provider_capabilities.py --provider moonshot > /tmp/moonshot_capabilities.log 2>&1 &
echo $!
```

- [ ] **Step 2: Wait and verify output**

```bash
# Check when done
sleep 300 && cat /home/spex/.zeroclaw/workspace/skills/github-grep/data/capabilities/moonshot.json | python3 -c "import json,sys; d=json.load(sys.stdin); print(f'Keys: {len(d[\"keys\"])}')"
```

Expected: `Keys: 57`

---

## Task 4: Run MiniMax batch

**Files:**
- Read: `~/.zeroclaw/workspace/skills/github-grep/data/valid/minimax.json`
- Write: `~/.zeroclaw/workspace/skills/github-grep/data/capabilities/minimax.json`

- [ ] **Step 1: Run minimax batch**

```bash
cd /home/spex/.zeroclaw/workspace/skills/github-grep && \
~/.zeroclaw/workspace/.venv/bin/python3 scripts/test_provider_capabilities.py --provider minimax
```

- [ ] **Step 2: Verify output**

```bash
cat /home/spex/.zeroclaw/workspace/skills/github-grep/data/capabilities/minimax.json | python3 -c "import json,sys; d=json.load(sys.stdin); print(f'Keys: {len(d[\"keys\"])}')"
```

Expected: `Keys: N` where N = number of live minimax keys

---

## Task 5: Review output — spot-check results

**Files:**
- Read: `~/.zeroclaw/workspace/skills/github-grep/data/capabilities/moonshot.json`
- Read: `~/.zeroclaw/workspace/skills/github-grep/data/capabilities/minimax.json`

- [ ] **Step 1: Count capability distribution for moonshot**

```python
python3 -c "
import json
with open('/home/spex/.zeroclaw/workspace/skills/github-grep/data/capabilities/moonshot.json') as f:
    d = json.load(f)
counts = {}
for key, val in d['keys'].items():
    for cap, res in val['capabilities'].items():
        s = res['status']
        counts[cap] = counts.get(cap, {})
        counts[cap][s] = counts[cap].get(s, 0) + 1
for cap, st in sorted(counts.items()):
    print(f'{cap}: {st}')
"
```

Expected: readable table showing working/no_access/not_found per capability

- [ ] **Step 2: Spot-check minimax video keys**

```python
python3 -c "
import json
with open('/home/spex/.zeroclaw/workspace/skills/github-grep/data/capabilities/minimax.json') as f:
    d = json.load(f)
for key, val in d['keys'].items():
    vg = val['capabilities'].get('video_gen', {})
    if vg.get('status') == 'working':
        print(f'VIDEO WORKING: {val[\"masked\"]}  detail={vg[\"detail\"]}')
"
```

Expected: the 4 video-working keys we found earlier appear here

---

## Task 6: Add `capabilities` action to key_store.py

**Files:**
- Modify: `~/.zeroclaw/workspace/skills/github-grep/scripts/key_store.py`

- [ ] **Step 1: Add `_print_capabilities` helper function after `_print_key` (~line 140)**

```python
def _print_capabilities(caps: dict, indent: int = 2):
    """Print capability summary for a key."""
    cats = {
        "text": "Text",
        "models": "Models list",
        "token_count": "Token count",
        "file_upload": "File/vision",
        "image_gen": "Image gen",
        "video_gen": "Video gen",
        "audio_tts": "TTS",
        "audio_stt": "STT",
        "voice_cloning": "Voice clone",
        "voice_design": "Voice design",
        "balance": "Balance",
        "rate_limit": "Rate limit",
    }
    for cap_key, label in cats.items():
        if cap_key in caps:
            res = caps[cap_key]
            status = res.get("status", "?")
            icon = {"working": "✅", "invalid": "❌", "no_access": "🔒",
                    "insufficient_balance": "💰", "not_found": "🚫",
                    "rate_limited": "⏳", "error": "⚠️"}.get(status, "❓")
            detail = res.get("detail", "")[:50]
            print(f"{' '*indent}{icon} {label:15s} {status:20s} {detail}")
```

- [ ] **Step 2: Add `capabilities` action to `main()` (~line 422)**

In `main()`, add before the `else` clause:

```python
elif args.action == "capabilities":
    import os
    cap_dir = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "data", "capabilities")
    if args.provider == "all":
        for prov in ["moonshot", "minimax"]:
            cap_file = os.path.join(cap_dir, f"{prov}.json")
            if os.path.exists(cap_file):
                with open(cap_file) as f:
                    d = json.load(f)
                print(f"\n=== {prov} ({len(d['keys'])} keys) ===")
                for key, val in list(d["keys"].items())[:5]:  # first 5
                    print(f"\n  Key: {val['masked']}")
                    _print_capabilities(val["capabilities"])
                if len(d["keys"]) > 5:
                    print(f"\n  ... and {len(d['keys'])-5} more keys")
            else:
                print(f"  {prov}: no capabilities data (run test_provider_capabilities.py first)")
    else:
        cap_file = os.path.join(cap_dir, f"{args.provider}.json")
        if not os.path.exists(cap_file):
            print(f"No capabilities data for {args.provider}. Run test_provider_capabilities.py first.")
            return
        with open(cap_file) as f:
            d = json.load(f)
        print(f"\n=== {args.provider} ({len(d['keys'])} keys) ===")
        for key, val in d["keys"].items():
            print(f"\n  Key: {val['masked']}")
            _print_capabilities(val["capabilities"])
```

- [ ] **Step 3: Test capabilities action**

```bash
cd /home/spex/.zeroclaw/workspace/skills/github-grep && \
~/.zeroclaw/workspace/.venv/bin/python3 scripts/key_store.py --action capabilities --provider moonshot
```

Expected: prints first 5 moonshot keys with their capability breakdown

- [ ] **Step 4: Commit**

```bash
cd /home/spex/work/erp/zeroclaws && \
git add docs/superpowers/plans/2026-03-26-provider-capabilities.md && \
git commit -m "feat(github-grep): add provider capabilities discovery plan"
```

---

## Self-Review Checklist

1. **Spec coverage:** All 6 capability categories from spec → tested in `MINIMAL_REQUESTS`
2. **Moonshot endpoints:** text, models, token_count, file_upload, image_gen, audio_speech, audio_stt, balance, rate_limit — all covered
3. **MiniMax endpoints:** text, models, token_count, file_upload, image_gen, video_gen, audio_tts, audio_stt, voice_cloning, voice_design, balance, rate_limit — all covered
4. **Placeholder scan:** No "TBD", no "TODO", no vague requirements — all code is concrete
5. **Type consistency:** `_classify_response` returns `tuple[str, str]` — used consistently
6. **Output format:** matches spec JSON structure with `provider`, `checked_at`, `keys[full_key]{masked, capabilities{}}`
