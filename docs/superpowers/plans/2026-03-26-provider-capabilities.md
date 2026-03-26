# Provider Capabilities Discovery — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Test every live key across all known providers against all known endpoints, produce per-key capability maps in `data/capabilities/{provider}.json`.

**Architecture:** Single Python script `test_provider_capabilities.py` that loads live keys from `data/valid/{provider}.json`, probes each endpoint with a minimal request, classifies the response, and saves structured JSON. Supports 5 providers across multiple capability categories.

---

## Known Provider Configurations (as of 2026-03-26)

### Moonshot (Kimi)
- **Endpoint:** `https://api.moonshot.cn/v1/chat/completions`
- **Auth:** `Bearer {key}` (format: `sk-...`)
- **Keys:** 57 total, 8 active
- **Models (13):** kimi-k2.5, kimi-k2-thinking, kimi-k2-thinking-turbo, kimi-k2-0905-preview, kimi-k2-0711-preview, kimi-k2-turbo-preview, moonshot-v1-8k, moonshot-v1-32k, moonshot-v1-128k, moonshot-v1-auto, moonshot-v1-8k-vision-preview, moonshot-v1-32k-vision-preview, moonshot-v1-128k-vision-preview
- **Status:** ALL 8 active keys support ALL 13 models ✅

### MiniMax
- **Endpoint:** `https://api.minimax.chat/v1/text/chatcompletion_v2` (text)
- **Endpoint:** `https://api.minimax.io/v1/video_generation` (video, duration=6 required)
- **Endpoint:** `https://api.minimax.chat/v1/speech-generation/t2a` (speech TTS)
- **Auth:** `Bearer {key}` (format: `sk-cp-...` for text/coding, `sk-api-...` for video)
- **Keys:** 178 total, 15 active (sk-cp-)
- **Text models (4):** MiniMax-M2.7, MiniMax-M2.5, MiniMax-M2.1, MiniMax-M2
- **Speech TTS (1):** speech-2.8-hd (with voice_id required)
- **Video models:** MiniMax-Hailuo-02 (duration MUST be 6, sk-api- keys only — currently dead)
- **Image:** `api.minimaxi.com` — inaccessible (timeout)

### Zai (GLM Coding Plan)
- **Endpoint:** `https://api.z.ai/api/coding/paas/v4/chat/completions` (coding plan)
- **Auth:** `Bearer {key}` (format: `hex.api_secret`)
- **Keys:** 36 total, 20 active
- **Models (3):** GLM-4.7, GLM-5-Turbo, GLM-4.5-Air
- **Status:** 20 working keys confirmed ✅

### Zhipu (BigModel / bigmodel.cn)
- **Endpoint:** `https://open.bigmodel.cn/api/paas/v4/chat/completions`
- **Auth:** `Bearer {key}` (format: `xxx.yyy` — hex.api_secret)
- **Keys:** 66 total, 3 active
- **Models (6+):** GLM-4, GLM-4.7, GLM-5, GLM-5-Turbo, GLM-4.5-Air, glm-4, glm-5, glm-5-turbo
- **Status:** 3 working keys, 62 rate_limited, 1 no_access ✅

### Alibaba (DashScope International)
- **Endpoint:** `https://dashscope-intl.aliyuncs.com/compatible-mode/v1/chat/completions`
- **Auth:** `Bearer {key}` (format: `sk-...`)
- **Keys:** 240 total, 5 active
- **Models (30+):** qwen-plus, qwen3-coder-next, qwen3-vl-flash, qwen3.5-flash, qwen3.5-plus, qwen3-asr-flash, qwen3-tts-instruct-flash, qwen-image-2.0, etc.
- **Capabilities (working):** text ✅, coder ✅, vision ✅
- **Capabilities (separate API):** ASR/TTS/image — separate endpoints not yet resolved
- **Status:** 5 working keys, 235 dead or invalid ✅ (partial — 240-key batch in progress)

---

## File Structure

```
~/.zeroclaw/workspace/skills/github-grep/
  scripts/
    test_provider_capabilities.py   # main test script (DONE — 5 providers)
  data/
    capabilities/                   # output directory (DONE)
      moonshot.json               # ✅ 57 keys — all 13 models
      minimax.json                # ✅ 15 keys — text 4 models + speech_tts
      zai.json                    # ✅ 36 keys — 3 models
      zhipu.json                  # ✅ 66 keys — 3 working
      alibaba.json                # ⏳ 3 keys tested, 240-key batch in progress
    valid/
      moonshot.json               # ✅ 8 active, 48 no_quota, 1 timeout
      minimax.json               # ✅ 15 active (sk-cp-), 163 no_quota
      zai.json                   # ✅ 20 active, 13 rate_limited, 3 error
      zhipu.json                 # ✅ 3 active, 62 rate_limited, 1 no_access
      alibaba.json               # ⏳ 5 active, 235 no_quota (in progress)
```

---

## Task Status

### Completed
- [x] Task 1: Create capabilities output directory
- [x] Task 2: Write test_provider_capabilities.py (`3446ecb`)
- [x] Task 3: Create capabilities output directory
- [x] Task 4: Add capabilities action to key_store.py (`29a58c3`)
- [x] Task 5: Run Moonshot capabilities batch — 57 keys, all 13 models ✅ (`ddec003`)
- [x] Task 6: Run MiniMax capabilities batch — 15 keys, 4 text models + speech_tts ✅ (`5bfa9b3`)
- [x] Task 7: Run Zai capabilities batch — 36 keys, 3 models ✅ (`e5ab187`)
- [x] Task 8: Run Zhipu capabilities batch — 66 keys, 3 working ✅ (`e5ab187`)
- [x] Task 9: Add Alibaba to test script ✅ (`8a61684`)
- [x] Task 10: Run Alibaba initial batch — 5 working keys ✅ (`8a61684`)

### In Progress
- [ ] Task 11: Alibaba 240-key full batch — 60/240 in background (PID 141330)
- [ ] Task 12: Research Alibaba ASR/TTS/image endpoints — separate API, task field format unknown

### Pending
- [ ] Task 13: Run Google capabilities batch (1802 keys)
- [ ] Task 14: Run deepseek capabilities batch (216 keys)
- [ ] Task 15: Run mistral capabilities batch (183 keys)
- [ ] Task 16: Run cohere capabilities batch (228 keys)
- [ ] Task 17: Add remaining providers to test script (deepseek, cohere, mistral, google, groq, etc.)
- [ ] Task 18: Final verification and documentation

---

## Provider Coverage Summary

| Provider | Total Keys | Working | Capabilities |
|----------|-----------|---------|--------------|
| moonshot | 57 | 8 | text 13 models ✅ |
| minimax | 178 | 15 (sk-cp-) | text 4 models + speech_tts ✅ |
| zai | 36 | 20 | GLM-4.7 + GLM-5-Turbo + GLM-4.5-Air ✅ |
| zhipu | 66 | 3 | GLM-4/4.7/5/5-Turbo ✅ |
| alibaba | 240 | 5 | text + coder + vision ✅ (batch in progress) |
| **Remaining** | | | |
| google | 1802 | TBD | — |
| google_gemini_pro | 1027 | TBD | — |
| google_sa | 663 | TBD | — |
| cohere | 228 | TBD | — |
| deepseek | 216 | TBD | — |
| mistral | 183 | TBD | — |
| aws | 75 | TBD | — |
| openrouter | 48 | TBD | — |
| openai | 31 | TBD | — |
| sambanova | 31 | TBD | — |
| replicate | 20 | TBD | — |
| together | 16 | TBD | — |
| groq | 10 | TBD | — |
| perplexity | 10 | TBD | — |
| huggingface | 4 | TBD | — |

---

## Batch Test Commands (Completed)

```bash
# Moonshot — 57 keys ✅
~/.zeroclaw/workspace/.venv/bin/python3 scripts/test_provider_capabilities.py --provider moonshot --limit 57

# MiniMax — 15 active sk-cp- keys ✅
~/.zeroclaw/workspace/.venv/bin/python3 scripts/test_provider_capabilities.py --provider minimax --limit 15

# Zai — 36 keys ✅
~/.zeroclaw/workspace/.venv/bin/python3 scripts/test_provider_capabilities.py --provider zai --limit 36

# Zhipu — 66 keys ✅
~/.zeroclaw/workspace/.venv/bin/python3 scripts/test_provider_capabilities.py --provider zhipu --limit 66

# Alibaba — 240 keys (in progress) ⏳
~/.zeroclaw/workspace/.venv/bin/python3 scripts/test_provider_capabilities.py --provider alibaba --limit 240
```

## Next Providers to Add to Script

- google (needs `model` list discovery)
- deepseek (`https://api.deepseek.com/v1/chat/completions`)
- cohere
- mistral
- groq, together, openrouter, etc.
