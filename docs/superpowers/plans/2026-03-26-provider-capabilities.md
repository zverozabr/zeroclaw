# Provider Capabilities Discovery — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Test every live key across all known providers (Moonshot, MiniMax, Zhipu, Zai) against all known endpoints, produce per-key capability maps in `data/capabilities/{provider}.json`.

**Architecture:** Single Python script `test_provider_capabilities.py` that loads live keys from `data/valid/{provider}.json`, probes each endpoint with a minimal request, classifies the response, and saves structured JSON. Supports 4 providers across 6 capability categories.

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
- **Endpoint:** `https://api.minimax.chat/v1/t2a_v2` (speech TTS)
- **Endpoint:** `https://api.minimaxi.com/v1/music_generation` (music, requires lyrics param)
- **Auth:** `Bearer {key}` (format: `sk-cp-...` for text/coding, `sk-api-...` for video)
- **Keys:** 178 total, 15 active (sk-cp-)
- **Text models (4):** MiniMax-M2.7, MiniMax-M2.5, MiniMax-M2.1, MiniMax-M2
- **Speech TTS (1):** speech-2.8-hd (with voice_id required)
- **Video models:** MiniMax-Hailuo-02 (duration MUST be 6, sk-api- keys only — currently dead)
- **Image:** `api.minimaxi.com` — inaccessible (timeout)
- **Music:** requires lyrics param

### Zhipu (BigModel / bigmodel.cn)
- **Endpoint:** `https://open.bigmodel.cn/api/paas/v4/chat/completions`
- **Auth:** `Bearer {key}` (format: `xxx.yyy` — hex.api_secret)
- **Keys:** 66 total, ? active (0 found so far — all have insufficient_balance)
- **Models:** GLM-4.7, GLM-5, GLM-5-Turbo, GLM-4.5-air, GLM-4, glm-5, glm-5-turbo
- **Coding endpoint:** N/A (no separate coding plan)
- **Note:** Many keys may be expired/depleted — needs careful testing with fresh keys

### Zai
- **Endpoint:** `https://api.z.ai/api/coding/paas/v4/chat/completions` (coding plan)
- **Auth:** `Bearer {key}` (format: `hex.api_secret`)
- **Keys:** 36 total, 19+ active
- **Models:** GLM-4.7, GLM-5-Turbo, GLM-4.5-Air
- **Status:** 19 working keys confirmed, GLM-5-Turbo confirmed working

---

## File Structure

```
~/.zeroclaw/workspace/skills/github-grep/
  scripts/
    test_provider_capabilities.py   # main test script (DONE)
  data/
    capabilities/                   # output directory (DONE)
      moonshot.json               # DONE — 57 keys tested
      minimax.json                # DONE — 5-key sample tested
      zhipu.json                  # TODO
      zai.json                    # TODO
    valid/
      moonshot.json               # UPDATED — 8 active, 48 no_quota
      minimax.json               # UPDATED — 15 active (sk-cp-), 163 no_quota
      zhipu.json                 # TODO — needs full retest
      zai.json                   # TODO — needs full retest
```

---

## Task 1: Create capabilities output directory ✅
- [x] Done

## Task 2: Write test_provider_capabilities.py ✅
- [x] Done — committed as `3446ecb`

## Task 3: Create capabilities output directory ✅
- [x] Done

## Task 4: Add capabilities action to key_store.py ✅
- [x] Done — committed as `29a58c3`

## Task 5: Run Moonshot capabilities batch ✅
- [x] Done — 57 keys, committed as `ddec003`
- [x] Updated valid/moonshot.json with live test results

## Task 6: Run MiniMax capabilities batch
- [x] 5-key sample done — committed
- [x] 15 working sk-cp- keys identified (text 4 models + speech TTS)
- [ ] Full 178-key batch — script crashes, need --limit approach or fix
- [ ] Video keys (sk-api-) — all dead, need new working video key

## Task 7: Run Zai capabilities batch (NEW)
- [ ] Test all 36 keys for GLM-4.7 and GLM-5-Turbo
- [ ] Test which models work per key
- [ ] Commit results to data/capabilities/zai.json
- [ ] Update data/valid/zai.json

## Task 8: Run Zhipu capabilities batch (NEW)
- [ ] Test all 66 keys with proper auth (Bearer token)
- [ ] Try multiple models (GLM-4.7, GLM-5-Turbo, GLM-4, GLM-5)
- [ ] Many keys have insufficient_balance — identify working ones
- [ ] Commit results to data/capabilities/zhipu.json
- [ ] Update data/valid/zhipu.json

## Task 9: Review and verify capabilities output
- [ ] Spot-check all 4 provider output files
- [ ] Verify model counts match live tests
- [ ] Commit final data files

---

## Zai Batch Test Commands

```bash
# Test all Zai keys for GLM-5-Turbo
~/.zeroclaw/workspace/.venv/bin/python3 scripts/test_provider_capabilities.py --provider zai --limit 36
```

## Zhipu Batch Test Commands

```bash
# Test all Zhipu keys
~/.zeroclaw/workspace/.venv/bin/python3 scripts/test_provider_capabilities.py --provider zhipu --limit 66
```

Note: `test_provider_capabilities.py` needs Zhipu and Zai provider configs added first.
