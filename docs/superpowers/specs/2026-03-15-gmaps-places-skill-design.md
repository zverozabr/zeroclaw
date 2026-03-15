# Google Maps Places Skill — Design Spec

**Date:** 2026-03-15
**Status:** Draft
**Skill name:** `gmaps-places`

## Purpose

Live Google Places API tool for ZeroClaw agent. On-demand search and detail retrieval for cafes, restaurants, and other businesses worldwide. No persistent storage — agent combines results with other tools (ERP, Telegram, etc.) at query time.

**Use cases:**
1. Competitive analysis — ratings, reviews, positioning of competitors in any zone
2. B2B lead generation — collect contacts (phone, website, socials) of businesses by area
3. Market research — aggregate data on business density, ratings, price levels by zone

## Architecture

```
User question
  → ZeroClaw agent
    → gmaps_search(query, location, radius, type)  → list of places
    → gmaps_details(place_id)                       → full place info + reviews
    → gmaps_compare(place_ids)                      → side-by-side comparison
  → Agent formats report (combines with erp_sales, telegram, etc.)
  → User gets answer
```

No database. No caching layer. Pure request-response tools.

## Tools

### `gmaps_search`

Text Search or Nearby Search by zone + type.

**Args:**
| Arg | Required | Description |
|-----|----------|-------------|
| `query` | yes | Search text: "coffee shops in Samui", "restaurants Chaweng" |
| `type` | no | Place type filter: cafe, restaurant, bar, hotel, spa (default: none) |
| `radius` | no | Radius in meters for location-biased search (default: 5000) |
| `location` | no | Lat,lng center point. If omitted, Google infers from query text |
| `min_rating` | no | Filter results: only places with rating >= N (default: 0) |
| `limit` | no | Max results to return (default: 20, max: 60 via pagination) |
| `sort` | no | Sort by: relevance (default), rating, reviews_count |

**Returns:**
```json
{
  "success": true,
  "count": 15,
  "places": [
    {
      "place_id": "ChIJ...",
      "name": "Cafe XYZ",
      "address": "123 Beach Rd, Samui",
      "location": {"lat": 9.53, "lng": 100.06},
      "rating": 4.5,
      "reviews_count": 128,
      "price_level": 2,
      "types": ["cafe", "food"],
      "open_now": true
    }
  ]
}
```

**FieldMask** (cost optimization): `places.id,places.displayName,places.formattedAddress,places.location,places.rating,places.userRatingCount,places.priceLevel,places.types,places.currentOpeningHours`

### `gmaps_details`

Full place details by place_id.

**Args:**
| Arg | Required | Description |
|-----|----------|-------------|
| `place_id` | yes | Google Place ID from search results |
| `reviews` | no | Include review texts (default: true) |
| `photos` | no | Include photo URLs (default: false) |
| `max_photos` | no | Max photos to return (default: 3) |

**Returns:**
```json
{
  "success": true,
  "place": {
    "place_id": "ChIJ...",
    "name": "Cafe XYZ",
    "address": "123 Beach Rd, Samui",
    "location": {"lat": 9.53, "lng": 100.06},
    "rating": 4.5,
    "reviews_count": 128,
    "price_level": 2,
    "types": ["cafe", "food"],
    "phone": "+66-77-123456",
    "website": "https://cafexyz.com",
    "google_maps_url": "https://maps.google.com/?cid=...",
    "hours": ["Mon: 8:00-22:00", "Tue: 8:00-22:00"],
    "reviews": [
      {"author": "John", "rating": 5, "text": "Great coffee!", "time": "2026-02-10", "language": "en"}
    ],
    "photos": [
      {"url": "https://places.googleapis.com/v1/places/ChIJ.../photos/...", "width": 1200, "height": 800}
    ]
  }
}
```

**FieldMask**: `id,displayName,formattedAddress,location,rating,userRatingCount,priceLevel,types,nationalPhoneNumber,internationalPhoneNumber,websiteUri,googleMapsUri,currentOpeningHours,reviews,photos`

### `gmaps_compare`

Compare N places side-by-side. Calls details for each, returns structured comparison.

**Args:**
| Arg | Required | Description |
|-----|----------|-------------|
| `place_ids` | yes | Comma-separated place IDs (max 10) |

**Returns:**
```json
{
  "success": true,
  "comparison": [
    {"name": "Cafe A", "rating": 4.5, "reviews_count": 128, "price_level": 2, "phone": "...", "website": "..."},
    {"name": "Cafe B", "rating": 4.2, "reviews_count": 85, "price_level": 1, "phone": "...", "website": "..."}
  ],
  "summary": {
    "highest_rated": "Cafe A",
    "most_reviewed": "Cafe A",
    "cheapest": "Cafe B"
  }
}
```

## Implementation

### File structure

```
~/.zeroclaw/workspace/skills/gmaps-places/
├── SKILL.toml          # Tool definitions
├── scripts/
│   ├── gmaps_client.py # Google Places API (New) client
│   ├── gmaps_queries.py # Pure functions: filtering, sorting, comparison
│   └── gmaps_places.py # CLI entrypoint (argparse subcommands)
└── tests/
    ├── conftest.py
    ├── test_client.py   # API response parsing tests (mocked)
    └── test_queries.py  # Pure function tests
```

Same pattern as `erp-analyst`: client / queries (pure) / CLI entrypoint.

### API: Google Places API (New)

Uses the newer `https://places.googleapis.com/v1/` endpoints (not legacy):

- **Text Search**: `POST /v1/places:searchText`
- **Nearby Search**: `POST /v1/places:searchNearby`
- **Place Details**: `GET /v1/places/{place_id}`
- **Place Photos**: `GET /v1/places/{place_id}/photos/{photo_reference}/media`

Auth: `X-Goog-Api-Key` header with `GOOGLE_API_KEY` env var.

### Cost control

Places API (New) charges per field requested. Strategy:
- Search: minimal FieldMask (no reviews, no photos) — cheapest tier
- Details: full FieldMask only when explicitly requested
- Compare: reuses details, no extra search calls
- `max_result_chars = 4000` in SKILL.toml truncates oversized responses

Estimated cost per query type (as of 2026):
- Text Search (Basic): ~$0.032 per request
- Place Details (Advanced, with reviews): ~$0.025 per place
- Photos: ~$0.007 per photo

### Config

**Environment:** `GOOGLE_API_KEY` added to `shell_env_passthrough` in config.toml.

**Agent config** (`config.toml`): add `gmaps_search`, `gmaps_details`, `gmaps_compare` to erp_analyst's `allowed_tools` so CFO agent can combine financial + competitive data. Also usable standalone by any agent.

### Error handling

- Missing `GOOGLE_API_KEY` → exit 0 with `{"success": false, "error": "GOOGLE_API_KEY not set"}`
- API errors (quota, invalid key) → exit 0 with error message (soft failure, same pattern as erp-analyst)
- Zero results → `{"success": true, "count": 0, "places": []}`

### Testing

- `test_queries.py`: pure function tests for filtering (min_rating), sorting, comparison summary
- `test_client.py`: mocked API responses, FieldMask construction, pagination token handling
- Target: 15+ tests
- No live API tests in CI (cost); manual smoke tests documented

## Agent integration examples

```
User: "Какие кафе рядом с Sweet & Salty и какой у них рейтинг?"
Agent: gmaps_search(query="cafe near Maenam Samui", radius=2000) → list
       → formats competitive overview

User: "Сравни нас с Coco Tam's и The Jungle Club"
Agent: gmaps_search(query="Coco Tam's Samui") → place_id
       gmaps_search(query="The Jungle Club Samui") → place_id
       gmaps_compare(place_ids="id1,id2") → comparison
       erp_sales(source=ss, period=month) → own revenue
       → formats comparison report

User: "Найди все рестораны в Чиангмае с рейтингом 4.5+ для партнёрства"
Agent: gmaps_search(query="restaurant Chiang Mai", min_rating=4.5, limit=40)
       → formats lead list with contacts
```

## Out of scope

- Persistent storage / caching
- Scheduled monitoring (cron) — possible later as separate concern
- Google Maps JavaScript API / frontend
- Photo downloading (only URLs returned)
- Review sentiment analysis
