# Provider Capabilities Discovery — Design Spec

## Overview

Systematically discover all API capabilities (models, endpoints, modalities) for Moonshot (Kimi) and MiniMax providers. For each live key, determine: text generation, token counting, file/vision, image generation, video, audio/speech, and balance/rate-limit capabilities.

**Phase 1: Research documentation** — enumerate all known endpoints from official docs.
**Phase 2: Scripted testing** — for each live key, probe each endpoint, record results.
**Phase 3: Capabilities JSON** — persist per-key capability maps to `data/capabilities/{provider}.json`.

---

## Scope

| Provider | Base URL | Docs |
|---|---|---|
| Moonshot (Kimi) | `https://api.moonshot.cn` | https://platform.moonshot.cn/docs |
| MiniMax | `https://api.minimax.chat` (text) / `https://api.minimax.io` (video) | https://www.minimaxi.com/document |

**6 capability categories to check per key:**

1. **Text models** — chat completion + model list
2. **Token count** — token counting endpoints
3. **File/vision** — file upload, vision capabilities
4. **Image generation** — text-to-image, image editing
5. **Video / Audio / Speech** — video generation, TTS, STT, voice cloning
6. **Balance / rate-limit** — quota remaining, rate limits

---

## Phase 1: Research — Endpoint Inventory

### Moonshot (Kimi) — Known Endpoints to Verify

```
Base: https://api.moonshot.cn

/v1/models                          GET   — list available models
/v1/chat/completions                POST  — text chat
/v1/tokens/count                    POST  — token counting
/v1/files                           POST  — file upload
/v1/files/{id}                     GET   — retrieve file
/v1/files/{id}/content              GET   — download file content
/v1/images/generations              POST  — image generation
/v1/audio/speech                    POST  — TTS
/v1/audio/transcriptions            POST  — STT (Whisper)
/v1/balance                         GET   — account balance
/v1/rate_limit                      GET   — rate limit status
```

### MiniMax — Known Endpoints to Verify

```
Text API Base: https://api.minimax.chat
Video API Base: https://api.minimax.io

/v1/text/chatcompletion_v2          POST  — text chat (MiniMax-Text-01)
/v1/models                          GET   — list models
/v1/tokens/count                    POST  — token counting

/v1/files                          POST  — file upload
/v1/files/{id}                     GET   — retrieve file

/v1/images/generations              POST  — image generation (MiniMax-Image-01)

/v1/video_generation                POST  — video generation (MiniMax-Hailuo-2.3)
/v1/video_generation/Query          POST  — query video task status

/v1/speech-generation/t2a           POST  — TTS
/v1/speech-recognition/a2t          POST  — STT

/voice/cloning                     POST  — voice cloning
/voice/design                      POST  — voice design

/v1/balance                         GET   — balance check (MiniMax-Text API)
/v1/rate_limit                      GET   — rate limit
```

Research phase confirms exact paths, HTTP methods, and request/response shapes for each.

---

## Phase 2: Test Script

### Script: `scripts/test_provider_capabilities.py`

**Input:** provider name (`moonshot` or `minimax`), key to test
**Output:** capabilities map + raw endpoint responses

**Logic flow:**

```
1. Load keys from data/valid/{provider}.json
2. For each live key:
   a. Probe /v1/models → extract model list
   b. For each capability category (1-6 above):
        - Build minimal test request
        - Send to endpoint, record (code, response_preview, task_id if async)
        - Classify: success / auth_error / permission_error / not_found / insufficient_balance
   c. Aggregate per-key capabilities object
3. Save to data/capabilities/{provider}.json
```

**Endpoint test patterns:**

| Capability | Moonshot Test | MiniMax Test |
|---|---|---|
| Text | POST `/v1/chat/completions` model=moonshot-v1-8k | POST `/v1/text/chatcompletion_v2` model=MiniMax-Text-01 |
| Token count | POST `/v1/tokens/count` | POST `/v1/tokens/count` |
| File upload | POST `/v1/files` (small dummy file) | POST `/v1/files` |
| Image gen | POST `/v1/images/generations` | POST `/v1/images/generations` |
| Video gen | (not available for Kimi) | POST `/v1/video_generation` |
| Audio/TTS | POST `/v1/audio/speech` | POST `/v1/speech-generation/t2a` |
| Balance | GET `/v1/balance` | GET `/v1/balance` |

**Minimal requests only** — no actual generation, just endpoint availability + auth check.

---

## Phase 3: Output Format

**File:** `data/capabilities/{provider}.json`

```json
{
  "provider": "moonshot",
  "checked_at": "2026-03-26T12:00:00",
  "keys": {
    "<full_key>": {
      "masked": "sk-...xxxx",
      "capabilities": {
        "text": {
          "status": "working",
          "models": ["moonshot-v1-8k", "moonshot-v1-32k", "moonshot-v1-128k"],
          "detail": "chat completion ok"
        },
        "token_count": {
          "status": "working",
          "detail": "token counting ok"
        },
        "file_vision": {
          "status": "working",
          "detail": "file upload ok, vision capable"
        },
        "image_generation": {
          "status": "no_access",
          "code": 403,
          "detail": "subscription required"
        },
        "video_generation": {
          "status": "not_available",
          "detail": "endpoint not found"
        },
        "audio_speech": {
          "status": "working",
          "models": ["speech-01"],
          "detail": "tts ok"
        },
        "balance": {
          "status": "working",
          "limit": "1000",
          "remaining": "847",
          "detail": "rate limit ok"
        }
      }
    }
  }
}
```

---

## Process

1. **Research docs** → write endpoint inventory (this spec, sections above)
2. **Write script** `test_provider_capabilities.py`
3. **Run moonshot batch** → check all 57 live moonshot keys
4. **Run minimax batch** → check all live minimax keys
5. **Review output** → validate capability maps make sense
6. **Update key_store.py** → add `capabilities` action to browse results

---

## Notes

- Minimal requests only — avoid generating content, just probe endpoints
- Rate limit: 0.5s between requests per key to avoid hitting limits
- Background execution: script should run with `run_in_background=true`
- For async endpoints (video generation): task_id returned = confirmed working, insufficient_balance = key valid but no quota
