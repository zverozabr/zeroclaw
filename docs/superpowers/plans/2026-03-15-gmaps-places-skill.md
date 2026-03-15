# Google Maps Places Skill — Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a ZeroClaw skill that queries Google Places API (New) for business search and details, with daily budget control and E2E validation.

**Architecture:** Three-file Python skill (client/queries/entrypoint) matching erp-analyst pattern. Text Search + Place Details endpoints only. Client-side filtering/sorting in pure functions. File-based daily budget counter.

**Tech Stack:** Python 3, `requests`, Google Places API (New) v1, pytest

**Spec:** `docs/superpowers/specs/2026-03-15-gmaps-places-skill-design.md`

---

## File Map

| File | Responsibility |
|------|---------------|
| `~/.zeroclaw/workspace/skills/gmaps-places/SKILL.toml` | Tool definitions, agent prompt |
| `scripts/gmaps_client.py` | API client: auth, Text Search, Place Details, Photo resolution, daily budget counter |
| `scripts/gmaps_queries.py` | Pure functions: parse API responses, filter by min_rating, sort by rating/reviews, extract open_now |
| `scripts/gmaps_places.py` | CLI entrypoint: argparse subcommands `search` and `details` |
| `tests/conftest.py` | Mock API response fixtures |
| `tests/test_queries.py` | Pure function tests (filtering, sorting, parsing) |
| `tests/test_client.py` | Mocked API tests (request building, budget counter, pagination) |
| `tests/test_e2e.py` | Live E2E tests against real Google Places API (marked `@pytest.mark.e2e`) |
| `~/.zeroclaw/config.toml` | Add GOOGLE_API_KEY passthrough + allowed_tools |

---

## Chunk 1: Pure Functions + Tests (gmaps_queries.py)

### Task 1: Scaffold skill directory + conftest

**Files:**
- Create: `~/.zeroclaw/workspace/skills/gmaps-places/scripts/__init__.py` (empty)
- Create: `~/.zeroclaw/workspace/skills/gmaps-places/tests/__init__.py` (empty)
- Create: `~/.zeroclaw/workspace/skills/gmaps-places/tests/conftest.py`

- [ ] **Step 1: Create directories and conftest with mock API fixtures**

```python
# tests/conftest.py
"""Shared fixtures for gmaps-places tests."""
import sys, os
import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "scripts"))


@pytest.fixture
def raw_search_response():
    """Raw Google Places API Text Search response."""
    return {
        "places": [
            {
                "id": "ChIJ_abc123",
                "displayName": {"text": "Cafe Alpha", "languageCode": "en"},
                "formattedAddress": "123 Beach Rd, Samui",
                "location": {"latitude": 9.53, "longitude": 100.06},
                "rating": 4.5,
                "userRatingCount": 128,
                "priceLevel": "PRICE_LEVEL_MODERATE",
                "types": ["cafe", "food", "point_of_interest", "establishment"],
                "currentOpeningHours": {"openNow": True},
            },
            {
                "id": "ChIJ_def456",
                "displayName": {"text": "Restaurant Beta", "languageCode": "en"},
                "formattedAddress": "456 Main St, Samui",
                "location": {"latitude": 9.54, "longitude": 100.07},
                "rating": 3.8,
                "userRatingCount": 42,
                "priceLevel": "PRICE_LEVEL_INEXPENSIVE",
                "types": ["restaurant", "food"],
                "currentOpeningHours": {"openNow": False},
            },
            {
                "id": "ChIJ_ghi789",
                "displayName": {"text": "Bar Gamma", "languageCode": "en"},
                "formattedAddress": "789 Night St, Samui",
                "location": {"latitude": 9.55, "longitude": 100.08},
                "rating": 4.8,
                "userRatingCount": 250,
                "priceLevel": "PRICE_LEVEL_EXPENSIVE",
                "types": ["bar", "night_club"],
            },
        ],
        "nextPageToken": "token_abc123",
    }


@pytest.fixture
def raw_details_response():
    """Raw Google Places API Place Details response."""
    return {
        "id": "ChIJ_abc123",
        "displayName": {"text": "Cafe Alpha", "languageCode": "en"},
        "formattedAddress": "123 Beach Rd, Samui",
        "location": {"latitude": 9.53, "longitude": 100.06},
        "rating": 4.5,
        "userRatingCount": 128,
        "priceLevel": "PRICE_LEVEL_MODERATE",
        "types": ["cafe", "food"],
        "nationalPhoneNumber": "077-123456",
        "internationalPhoneNumber": "+66-77-123456",
        "websiteUri": "https://cafealpha.com",
        "googleMapsUri": "https://maps.google.com/?cid=12345",
        "currentOpeningHours": {
            "openNow": True,
            "weekdayDescriptions": [
                "Monday: 8:00\u2009AM\u2009–\u200910:00\u2009PM",
                "Tuesday: 8:00\u2009AM\u2009–\u200910:00\u2009PM",
            ],
        },
        "reviews": [
            {
                "authorAttribution": {"displayName": "John"},
                "rating": 5,
                "text": {"text": "Great coffee!", "languageCode": "en"},
                "relativePublishTimeDescription": "2 weeks ago",
                "publishTime": "2026-02-28T10:00:00Z",
            },
            {
                "authorAttribution": {"displayName": "Мария"},
                "rating": 4,
                "text": {"text": "Хорошая еда", "languageCode": "ru"},
                "publishTime": "2026-03-01T14:00:00Z",
            },
        ],
        "photos": [
            {"name": "places/ChIJ_abc123/photos/AUc7_photo1", "widthPx": 1200, "heightPx": 800},
            {"name": "places/ChIJ_abc123/photos/AUc7_photo2", "widthPx": 800, "heightPx": 600},
        ],
    }
```

- [ ] **Step 2: Commit scaffold**

```bash
git add ~/.zeroclaw/workspace/skills/gmaps-places/
git commit -m "feat(gmaps-places): scaffold skill directory + test fixtures"
```

### Task 2: Response parsing — parse_search_place, parse_details_place

**Files:**
- Create: `scripts/gmaps_queries.py`
- Create: `tests/test_queries.py`

- [ ] **Step 3: Write failing tests for parse_search_place**

```python
# tests/test_queries.py
"""Tests for gmaps_queries.py — pure functions."""
import sys, os
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "scripts"))

from gmaps_queries import parse_search_place, parse_details_place


class TestParseSearchPlace:
    def test_basic_fields(self, raw_search_response):
        raw = raw_search_response["places"][0]
        p = parse_search_place(raw)
        assert p["place_id"] == "ChIJ_abc123"
        assert p["name"] == "Cafe Alpha"
        assert p["address"] == "123 Beach Rd, Samui"
        assert p["location"] == {"lat": 9.53, "lng": 100.06}
        assert p["rating"] == 4.5
        assert p["reviews_count"] == 128
        assert p["price_level"] == 2
        assert "cafe" in p["types"]
        assert p["open_now"] is True

    def test_missing_opening_hours(self, raw_search_response):
        raw = raw_search_response["places"][2]  # Bar Gamma, no openNow
        p = parse_search_place(raw)
        assert p["open_now"] is None

    def test_price_level_mapping(self):
        raw = {"id": "x", "displayName": {"text": "X"}, "priceLevel": "PRICE_LEVEL_FREE"}
        p = parse_search_place(raw)
        assert p["price_level"] == 0

    def test_missing_price_level(self):
        raw = {"id": "x", "displayName": {"text": "X"}}
        p = parse_search_place(raw)
        assert p["price_level"] is None
```

- [ ] **Step 4: Run tests — verify they fail**

```bash
cd ~/.zeroclaw/workspace/skills/gmaps-places && python3 -m pytest tests/test_queries.py -v
```
Expected: ImportError — `gmaps_queries` not found.

- [ ] **Step 5: Implement parse_search_place and parse_details_place**

```python
# scripts/gmaps_queries.py
"""Pure functions for Google Places API response parsing, filtering, sorting."""

from typing import Any, Dict, List, Optional

_PRICE_LEVEL_MAP = {
    "PRICE_LEVEL_FREE": 0,
    "PRICE_LEVEL_INEXPENSIVE": 1,
    "PRICE_LEVEL_MODERATE": 2,
    "PRICE_LEVEL_EXPENSIVE": 3,
    "PRICE_LEVEL_VERY_EXPENSIVE": 4,
}


def parse_search_place(raw: Dict[str, Any]) -> Dict[str, Any]:
    """Parse a single place from Text Search API response into our schema."""
    hours = raw.get("currentOpeningHours") or {}
    return {
        "place_id": raw.get("id", ""),
        "name": (raw.get("displayName") or {}).get("text", ""),
        "address": raw.get("formattedAddress", ""),
        "location": {
            "lat": (raw.get("location") or {}).get("latitude"),
            "lng": (raw.get("location") or {}).get("longitude"),
        },
        "rating": raw.get("rating"),
        "reviews_count": raw.get("userRatingCount", 0),
        "price_level": _PRICE_LEVEL_MAP.get(raw.get("priceLevel")) if raw.get("priceLevel") else None,
        "types": [t for t in raw.get("types", []) if t not in ("point_of_interest", "establishment")],
        "open_now": hours.get("openNow") if hours else None,
    }


def parse_details_place(raw: Dict[str, Any], include_reviews: bool = True, include_photos: bool = False) -> Dict[str, Any]:
    """Parse Place Details API response into our schema."""
    base = parse_search_place(raw)
    hours = raw.get("currentOpeningHours") or {}

    base.update({
        "phone": raw.get("internationalPhoneNumber") or raw.get("nationalPhoneNumber") or "",
        "website": raw.get("websiteUri", ""),
        "google_maps_url": raw.get("googleMapsUri", ""),
        "hours": hours.get("weekdayDescriptions", []),
    })

    if include_reviews:
        base["reviews"] = [
            {
                "author": (r.get("authorAttribution") or {}).get("displayName", ""),
                "rating": r.get("rating"),
                "text": (r.get("text") or {}).get("text", ""),
                "time": (r.get("publishTime") or "")[:10],
                "language": (r.get("text") or {}).get("languageCode", ""),
            }
            for r in raw.get("reviews", [])
        ]

    if include_photos:
        base["photos"] = [
            {
                "photo_resource": p.get("name", ""),
                "width": p.get("widthPx"),
                "height": p.get("heightPx"),
            }
            for p in raw.get("photos", [])
        ]

    return base
```

- [ ] **Step 6: Run tests — verify they pass**

```bash
cd ~/.zeroclaw/workspace/skills/gmaps-places && python3 -m pytest tests/test_queries.py -v
```

- [ ] **Step 7: Commit**

```bash
git add ~/.zeroclaw/workspace/skills/gmaps-places/scripts/gmaps_queries.py tests/test_queries.py
git commit -m "feat(gmaps-places): parse_search_place + parse_details_place with tests"
```

### Task 3: Filtering and sorting

**Files:**
- Modify: `scripts/gmaps_queries.py`
- Modify: `tests/test_queries.py`

- [ ] **Step 8: Write failing tests for filter_places and sort_places**

```python
# Append to tests/test_queries.py
from gmaps_queries import filter_places, sort_places


class TestFilterPlaces:
    def test_min_rating(self, raw_search_response):
        places = [parse_search_place(p) for p in raw_search_response["places"]]
        filtered = filter_places(places, min_rating=4.0)
        assert len(filtered) == 2  # Alpha 4.5, Gamma 4.8
        assert all(p["rating"] >= 4.0 for p in filtered)

    def test_min_rating_zero_keeps_all(self, raw_search_response):
        places = [parse_search_place(p) for p in raw_search_response["places"]]
        filtered = filter_places(places, min_rating=0)
        assert len(filtered) == 3

    def test_min_rating_too_high(self, raw_search_response):
        places = [parse_search_place(p) for p in raw_search_response["places"]]
        filtered = filter_places(places, min_rating=5.0)
        assert len(filtered) == 0

    def test_none_rating_excluded(self):
        places = [{"rating": None, "name": "No Rating"}]
        filtered = filter_places(places, min_rating=1.0)
        assert len(filtered) == 0


class TestSortPlaces:
    def test_sort_by_rating(self, raw_search_response):
        places = [parse_search_place(p) for p in raw_search_response["places"]]
        sorted_p = sort_places(places, sort_by="rating")
        assert sorted_p[0]["name"] == "Bar Gamma"  # 4.8
        assert sorted_p[1]["name"] == "Cafe Alpha"  # 4.5

    def test_sort_by_reviews_count(self, raw_search_response):
        places = [parse_search_place(p) for p in raw_search_response["places"]]
        sorted_p = sort_places(places, sort_by="reviews_count")
        assert sorted_p[0]["name"] == "Bar Gamma"  # 250
        assert sorted_p[1]["name"] == "Cafe Alpha"  # 128

    def test_sort_relevance_preserves_order(self, raw_search_response):
        places = [parse_search_place(p) for p in raw_search_response["places"]]
        sorted_p = sort_places(places, sort_by="relevance")
        assert sorted_p[0]["name"] == "Cafe Alpha"  # original order
```

- [ ] **Step 9: Run tests — verify they fail**

- [ ] **Step 10: Implement filter_places and sort_places**

```python
# Append to scripts/gmaps_queries.py

def filter_places(places: List[Dict], min_rating: float = 0) -> List[Dict]:
    """Client-side post-filter by minimum rating."""
    if min_rating <= 0:
        return places
    return [p for p in places if (p.get("rating") or 0) >= min_rating]


def sort_places(places: List[Dict], sort_by: str = "relevance") -> List[Dict]:
    """Client-side sort. 'relevance' preserves API order. 'rating'/'reviews_count' sort desc."""
    if sort_by == "relevance":
        return places
    if sort_by == "rating":
        return sorted(places, key=lambda p: p.get("rating") or 0, reverse=True)
    if sort_by == "reviews_count":
        return sorted(places, key=lambda p: p.get("reviews_count") or 0, reverse=True)
    return places
```

- [ ] **Step 11: Run tests — verify they pass**

- [ ] **Step 12: Commit**

```bash
git commit -am "feat(gmaps-places): filter_places + sort_places with tests"
```

### Task 4: parse_details_place tests

- [ ] **Step 13: Write tests for parse_details_place**

```python
# Append to tests/test_queries.py

class TestParseDetailsPlace:
    def test_contact_fields(self, raw_details_response):
        p = parse_details_place(raw_details_response)
        assert p["phone"] == "+66-77-123456"
        assert p["website"] == "https://cafealpha.com"
        assert p["google_maps_url"] == "https://maps.google.com/?cid=12345"

    def test_hours(self, raw_details_response):
        p = parse_details_place(raw_details_response)
        assert len(p["hours"]) == 2

    def test_reviews_included(self, raw_details_response):
        p = parse_details_place(raw_details_response, include_reviews=True)
        assert len(p["reviews"]) == 2
        assert p["reviews"][0]["author"] == "John"
        assert p["reviews"][0]["rating"] == 5
        assert p["reviews"][1]["language"] == "ru"

    def test_reviews_excluded(self, raw_details_response):
        p = parse_details_place(raw_details_response, include_reviews=False)
        assert "reviews" not in p

    def test_photos_included(self, raw_details_response):
        p = parse_details_place(raw_details_response, include_photos=True)
        assert len(p["photos"]) == 2
        assert "photo_resource" in p["photos"][0]

    def test_photos_excluded_by_default(self, raw_details_response):
        p = parse_details_place(raw_details_response)
        assert "photos" not in p
```

- [ ] **Step 14: Run — should all pass (already implemented)**

- [ ] **Step 15: Commit**

```bash
git commit -am "test(gmaps-places): parse_details_place tests"
```

---

## Chunk 2: API Client (gmaps_client.py)

### Task 5: Daily budget counter

**Files:**
- Create: `scripts/gmaps_client.py`
- Create: `tests/test_client.py`

- [ ] **Step 16: Write failing tests for budget counter**

```python
# tests/test_client.py
"""Tests for gmaps_client.py — API client with mocked responses."""
import sys, os, json, tempfile
from unittest.mock import patch
import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "scripts"))

from gmaps_client import DailyBudget


class TestDailyBudget:
    def test_new_day_starts_at_zero(self, tmp_path):
        b = DailyBudget(counter_file=str(tmp_path / "budget.json"))
        assert b.get_count("search") == 0

    def test_increment(self, tmp_path):
        b = DailyBudget(counter_file=str(tmp_path / "budget.json"))
        b.increment("search")
        b.increment("search")
        assert b.get_count("search") == 2

    def test_check_within_limit(self, tmp_path):
        b = DailyBudget(counter_file=str(tmp_path / "budget.json"), limits={"search": 5})
        for _ in range(4):
            b.increment("search")
        assert b.check("search") is True

    def test_check_at_limit(self, tmp_path):
        b = DailyBudget(counter_file=str(tmp_path / "budget.json"), limits={"search": 3})
        for _ in range(3):
            b.increment("search")
        assert b.check("search") is False

    def test_resets_on_new_day(self, tmp_path):
        b = DailyBudget(counter_file=str(tmp_path / "budget.json"))
        b.increment("search")
        # Simulate date change
        data = json.loads((tmp_path / "budget.json").read_text())
        data["date"] = "2020-01-01"
        (tmp_path / "budget.json").write_text(json.dumps(data))
        b2 = DailyBudget(counter_file=str(tmp_path / "budget.json"))
        assert b2.get_count("search") == 0
```

- [ ] **Step 17: Run — verify fail**

- [ ] **Step 18: Implement DailyBudget**

```python
# scripts/gmaps_client.py
"""Google Places API (New) client with daily budget counter."""

import json
import os
from datetime import date
from typing import Any, Dict, List, Optional

import requests

BUDGET_FILE = "/tmp/gmaps_daily_calls.json"
DEFAULT_LIMITS = {"search": 100, "details": 50, "photos": 30}


class DailyBudget:
    """File-based daily API call counter."""

    def __init__(self, counter_file: str = BUDGET_FILE, limits: Dict[str, int] = None):
        self._file = counter_file
        self._limits = limits or DEFAULT_LIMITS
        self._data = self._load()

    def _load(self) -> Dict:
        today = str(date.today())
        try:
            with open(self._file) as f:
                data = json.load(f)
            if data.get("date") != today:
                return {"date": today, "counts": {}}
            return data
        except (FileNotFoundError, json.JSONDecodeError):
            return {"date": today, "counts": {}}

    def _save(self):
        with open(self._file, "w") as f:
            json.dump(self._data, f)

    def get_count(self, call_type: str) -> int:
        return self._data.get("counts", {}).get(call_type, 0)

    def check(self, call_type: str) -> bool:
        limit = self._limits.get(call_type, 999)
        return self.get_count(call_type) < limit

    def increment(self, call_type: str):
        self._data.setdefault("counts", {})[call_type] = self.get_count(call_type) + 1
        self._save()
```

- [ ] **Step 19: Run — verify pass**

- [ ] **Step 20: Commit**

```bash
git commit -am "feat(gmaps-places): DailyBudget counter with tests"
```

### Task 6: PlacesClient — Text Search

**Files:**
- Modify: `scripts/gmaps_client.py`
- Modify: `tests/test_client.py`

- [ ] **Step 21: Write failing tests for PlacesClient.text_search**

```python
# Append to tests/test_client.py
from unittest.mock import MagicMock
from gmaps_client import PlacesClient


class TestPlacesClientSearch:
    def test_text_search_basic(self, raw_search_response, tmp_path):
        client = PlacesClient(api_key="fake", budget_file=str(tmp_path / "b.json"))
        mock_resp = MagicMock()
        mock_resp.status_code = 200
        mock_resp.json.return_value = raw_search_response

        with patch.object(client._session, "post", return_value=mock_resp) as mock_post:
            result = client.text_search("cafes in Samui")

        assert len(result["places"]) == 3
        assert result["next_page_token"] == "token_abc123"
        call_args = mock_post.call_args
        body = call_args[1]["json"]
        assert body["textQuery"] == "cafes in Samui"

    def test_text_search_with_location_bias(self, raw_search_response, tmp_path):
        client = PlacesClient(api_key="fake", budget_file=str(tmp_path / "b.json"))
        mock_resp = MagicMock()
        mock_resp.status_code = 200
        mock_resp.json.return_value = raw_search_response

        with patch.object(client._session, "post", return_value=mock_resp) as mock_post:
            client.text_search("cafe", location="9.53,100.06", radius=2000)

        body = mock_post.call_args[1]["json"]
        assert "locationBias" in body
        assert body["locationBias"]["circle"]["center"]["latitude"] == 9.53
        assert body["locationBias"]["circle"]["radius"] == 2000.0

    def test_text_search_with_type(self, raw_search_response, tmp_path):
        client = PlacesClient(api_key="fake", budget_file=str(tmp_path / "b.json"))
        mock_resp = MagicMock()
        mock_resp.status_code = 200
        mock_resp.json.return_value = raw_search_response

        with patch.object(client._session, "post", return_value=mock_resp) as mock_post:
            client.text_search("food Samui", included_type="cafe")

        body = mock_post.call_args[1]["json"]
        assert body["includedType"] == "cafe"

    def test_budget_incremented(self, raw_search_response, tmp_path):
        client = PlacesClient(api_key="fake", budget_file=str(tmp_path / "b.json"))
        mock_resp = MagicMock()
        mock_resp.status_code = 200
        mock_resp.json.return_value = raw_search_response

        with patch.object(client._session, "post", return_value=mock_resp):
            client.text_search("test")

        assert client._budget.get_count("search") == 1

    def test_budget_exceeded_raises(self, tmp_path):
        client = PlacesClient(api_key="fake", budget_file=str(tmp_path / "b.json"),
                              budget_limits={"search": 0})
        with pytest.raises(RuntimeError, match="Daily API budget"):
            client.text_search("test")
```

- [ ] **Step 22: Run — verify fail**

- [ ] **Step 23: Implement PlacesClient.text_search**

```python
# Append to scripts/gmaps_client.py

BASE_URL = "https://places.googleapis.com/v1"

SEARCH_FIELD_MASK = (
    "places.id,places.displayName,places.formattedAddress,places.location,"
    "places.rating,places.userRatingCount,places.priceLevel,places.types,"
    "places.currentOpeningHours,nextPageToken"
)

DETAILS_FIELD_MASK = (
    "id,displayName,formattedAddress,location,rating,userRatingCount,"
    "priceLevel,types,nationalPhoneNumber,internationalPhoneNumber,"
    "websiteUri,googleMapsUri,currentOpeningHours,reviews,photos"
)

DETAILS_NO_REVIEWS_MASK = (
    "id,displayName,formattedAddress,location,rating,userRatingCount,"
    "priceLevel,types,nationalPhoneNumber,internationalPhoneNumber,"
    "websiteUri,googleMapsUri,currentOpeningHours"
)


class PlacesClient:
    """Google Places API (New) client."""

    def __init__(self, api_key: str = None, budget_file: str = BUDGET_FILE,
                 budget_limits: Dict[str, int] = None):
        self._api_key = api_key or os.environ.get("GOOGLE_API_KEY", "")
        self._session = requests.Session()
        self._session.headers.update({
            "X-Goog-Api-Key": self._api_key,
            "Content-Type": "application/json",
        })
        self._budget = DailyBudget(counter_file=budget_file, limits=budget_limits or DEFAULT_LIMITS)

    def text_search(
        self,
        query: str,
        included_type: str = None,
        location: str = None,
        radius: float = 5000,
        page_token: str = None,
        language: str = "en",
        page_size: int = 20,
    ) -> Dict[str, Any]:
        """POST /v1/places:searchText"""
        if not self._budget.check("search"):
            count = self._budget.get_count("search")
            limit = self._budget._limits.get("search", 100)
            raise RuntimeError(f"Daily API budget reached ({count}/{limit} search calls). Resets at midnight.")

        body: Dict[str, Any] = {
            "textQuery": query,
            "languageCode": language,
            "pageSize": min(page_size, 20),
        }
        if included_type:
            body["includedType"] = included_type
        if location:
            lat, lng = [float(x.strip()) for x in location.split(",")]
            body["locationBias"] = {
                "circle": {
                    "center": {"latitude": lat, "longitude": lng},
                    "radius": float(radius),
                }
            }
        if page_token:
            body["pageToken"] = page_token

        resp = self._session.post(
            f"{BASE_URL}/places:searchText",
            json=body,
            headers={"X-Goog-FieldMask": SEARCH_FIELD_MASK},
            timeout=15,
        )
        resp.raise_for_status()
        self._budget.increment("search")

        data = resp.json()
        return {
            "places": data.get("places", []),
            "next_page_token": data.get("nextPageToken"),
        }
```

- [ ] **Step 24: Run — verify pass**

- [ ] **Step 25: Commit**

```bash
git commit -am "feat(gmaps-places): PlacesClient.text_search with tests"
```

### Task 7: PlacesClient — Place Details + Photo resolution

- [ ] **Step 26: Write failing tests for get_details and resolve_photo_url**

```python
# Append to tests/test_client.py

class TestPlacesClientDetails:
    def test_get_details(self, raw_details_response, tmp_path):
        client = PlacesClient(api_key="fake", budget_file=str(tmp_path / "b.json"))
        mock_resp = MagicMock()
        mock_resp.status_code = 200
        mock_resp.json.return_value = raw_details_response

        with patch.object(client._session, "get", return_value=mock_resp):
            result = client.get_details("ChIJ_abc123")

        assert result["id"] == "ChIJ_abc123"
        assert result["rating"] == 4.5

    def test_get_details_budget(self, raw_details_response, tmp_path):
        client = PlacesClient(api_key="fake", budget_file=str(tmp_path / "b.json"))
        mock_resp = MagicMock()
        mock_resp.status_code = 200
        mock_resp.json.return_value = raw_details_response

        with patch.object(client._session, "get", return_value=mock_resp):
            client.get_details("ChIJ_abc123")

        assert client._budget.get_count("details") == 1

    def test_resolve_photo_url(self, tmp_path):
        client = PlacesClient(api_key="fake", budget_file=str(tmp_path / "b.json"))
        mock_resp = MagicMock()
        mock_resp.status_code = 200
        mock_resp.url = "https://lh3.googleusercontent.com/resolved_photo"

        with patch.object(client._session, "get", return_value=mock_resp):
            url = client.resolve_photo_url("places/X/photos/Y")

        assert url == "https://lh3.googleusercontent.com/resolved_photo"
```

- [ ] **Step 27: Implement get_details and resolve_photo_url**

```python
# Append to PlacesClient in scripts/gmaps_client.py

    def get_details(
        self,
        place_id: str,
        include_reviews: bool = True,
        language: str = "en",
    ) -> Dict[str, Any]:
        """GET /v1/places/{place_id}"""
        if not self._budget.check("details"):
            count = self._budget.get_count("details")
            limit = self._budget._limits.get("details", 50)
            raise RuntimeError(f"Daily API budget reached ({count}/{limit} detail calls). Resets at midnight.")

        mask = DETAILS_FIELD_MASK if include_reviews else DETAILS_NO_REVIEWS_MASK
        resp = self._session.get(
            f"{BASE_URL}/places/{place_id}",
            headers={"X-Goog-FieldMask": mask},
            params={"languageCode": language},
            timeout=15,
        )
        resp.raise_for_status()
        self._budget.increment("details")
        return resp.json()

    def resolve_photo_url(self, photo_resource: str, max_width: int = 800) -> str:
        """GET /v1/{photo_resource}/media → resolved redirect URL."""
        if not self._budget.check("photos"):
            count = self._budget.get_count("photos")
            limit = self._budget._limits.get("photos", 30)
            raise RuntimeError(f"Daily API budget reached ({count}/{limit} photo calls). Resets at midnight.")

        resp = self._session.get(
            f"{BASE_URL}/{photo_resource}/media",
            params={"maxWidthPx": max_width, "skipHttpRedirect": "true"},
            timeout=10,
        )
        resp.raise_for_status()
        self._budget.increment("photos")
        # When skipHttpRedirect=true, response JSON has photoUri
        data = resp.json()
        return data.get("photoUri", resp.url)
```

- [ ] **Step 28: Run — verify pass**

- [ ] **Step 29: Commit**

```bash
git commit -am "feat(gmaps-places): PlacesClient.get_details + resolve_photo_url"
```

---

## Chunk 3: CLI Entrypoint + SKILL.toml + Config

### Task 8: CLI entrypoint (gmaps_places.py)

**Files:**
- Create: `scripts/gmaps_places.py`

- [ ] **Step 30: Implement gmaps_places.py**

```python
#!/usr/bin/env python3
"""
Google Maps Places — CLI tool for business search and details.

Subcommands:
  search  — Text search for places with filtering and sorting
  details — Full place details with reviews and photos
"""

import sys
import os
import json
import argparse

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from gmaps_client import PlacesClient
from gmaps_queries import parse_search_place, parse_details_place, filter_places, sort_places


def cmd_search(args):
    """Text Search for places."""
    api_key = os.environ.get("GOOGLE_API_KEY", "")
    if not api_key:
        print(json.dumps({"success": False, "error": "GOOGLE_API_KEY not set"}, ensure_ascii=False), flush=True)
        return

    client = PlacesClient(api_key=api_key)
    limit = int(args.limit or 20)
    min_rating = float(args.min_rating or 0)

    try:
        # Fetch enough results to satisfy limit after filtering
        fetch_size = min(20, limit) if min_rating <= 0 else 20
        raw = client.text_search(
            query=args.query,
            included_type=args.type if args.type else None,
            location=args.location if args.location else None,
            radius=float(args.radius or 5000),
            page_token=args.page_token if args.page_token else None,
            language=args.language or "en",
            page_size=fetch_size,
        )

        places = [parse_search_place(p) for p in raw["places"]]

        # Auto-paginate if we need more results
        next_token = raw["next_page_token"]
        while len(places) < limit and next_token and min_rating > 0:
            raw2 = client.text_search(
                query=args.query,
                included_type=args.type if args.type else None,
                page_token=next_token,
                language=args.language or "en",
            )
            places.extend(parse_search_place(p) for p in raw2["places"])
            next_token = raw2["next_page_token"]

        # Client-side filter + sort
        places = filter_places(places, min_rating=min_rating)
        places = sort_places(places, sort_by=args.sort_by or "relevance")
        places = places[:limit]

        result = {
            "success": True,
            "count": len(places),
            "next_page_token": next_token if len(places) >= limit else None,
            "places": places,
        }
    except RuntimeError as e:
        result = {"success": False, "error": str(e)}
    except Exception as e:
        result = {"success": False, "error": f"API error: {e}"}

    print(json.dumps(result, ensure_ascii=False, default=str), flush=True)


def cmd_details(args):
    """Place Details lookup."""
    api_key = os.environ.get("GOOGLE_API_KEY", "")
    if not api_key:
        print(json.dumps({"success": False, "error": "GOOGLE_API_KEY not set"}, ensure_ascii=False), flush=True)
        return

    client = PlacesClient(api_key=api_key)
    include_reviews = (args.reviews or "true").lower() == "true"
    include_photos = (args.photos or "false").lower() == "true"
    max_photos = int(args.max_photos or 3)

    try:
        raw = client.get_details(
            place_id=args.place_id,
            include_reviews=include_reviews,
            language=args.language or "en",
        )

        place = parse_details_place(raw, include_reviews=include_reviews, include_photos=include_photos)

        # Resolve photo URLs if requested
        if include_photos and "photos" in place:
            resolved = []
            for photo in place["photos"][:max_photos]:
                try:
                    url = client.resolve_photo_url(photo["photo_resource"])
                    resolved.append({"url": url, "width": photo["width"], "height": photo["height"]})
                except Exception:
                    continue
            place["photos"] = resolved

        result = {"success": True, "place": place}
    except RuntimeError as e:
        result = {"success": False, "error": str(e)}
    except Exception as e:
        result = {"success": False, "error": f"API error: {e}"}

    print(json.dumps(result, ensure_ascii=False, default=str), flush=True)


def main():
    parser = argparse.ArgumentParser(description="Google Maps Places")
    sub = parser.add_subparsers(dest="command")

    p_search = sub.add_parser("search")
    p_search.add_argument("--query", required=True)
    p_search.add_argument("--type", default=None)
    p_search.add_argument("--location", default=None)
    p_search.add_argument("--radius", default="5000")
    p_search.add_argument("--min-rating", default="0")
    p_search.add_argument("--limit", default="20")
    p_search.add_argument("--sort-by", default="relevance")
    p_search.add_argument("--page-token", default=None)
    p_search.add_argument("--language", default="en")

    p_details = sub.add_parser("details")
    p_details.add_argument("--place-id", required=True)
    p_details.add_argument("--reviews", default="true")
    p_details.add_argument("--photos", default="false")
    p_details.add_argument("--max-photos", default="3")
    p_details.add_argument("--language", default="en")

    args = parser.parse_args()
    if not args.command:
        parser.print_help()
        sys.exit(1)

    try:
        {"search": cmd_search, "details": cmd_details}[args.command](args)
    except Exception as e:
        print(json.dumps({"success": False, "error": str(e)}, ensure_ascii=False), flush=True)
        sys.exit(0)


if __name__ == "__main__":
    main()
```

- [ ] **Step 31: Syntax check**

```bash
python3 -c "import py_compile; py_compile.compile(os.path.expanduser('~/.zeroclaw/workspace/skills/gmaps-places/scripts/gmaps_places.py'), doraise=True)"
```

- [ ] **Step 32: Commit**

```bash
git commit -am "feat(gmaps-places): CLI entrypoint gmaps_places.py"
```

### Task 9: SKILL.toml

**Files:**
- Create: `~/.zeroclaw/workspace/skills/gmaps-places/SKILL.toml`

- [ ] **Step 33: Write SKILL.toml**

Copy verbatim from spec (section "SKILL.toml draft"). The full content is in the spec at lines 143-211.

- [ ] **Step 34: Validate TOML syntax**

```bash
python3 -c "import tomllib; tomllib.load(open(os.path.expanduser('~/.zeroclaw/workspace/skills/gmaps-places/SKILL.toml'), 'rb'))"
```

- [ ] **Step 35: Commit**

```bash
git commit -am "feat(gmaps-places): SKILL.toml with tool definitions"
```

### Task 10: Config.toml — add GOOGLE_API_KEY passthrough + allowed_tools

**Files:**
- Modify: `~/.zeroclaw/config.toml`

- [ ] **Step 36: Add GOOGLE_API_KEY to shell_env_passthrough**

Find the `shell_env_passthrough` array and add `"GOOGLE_API_KEY"`.

- [ ] **Step 37: Add gmaps tools to erp_analyst allowed_tools**

In `[agents.erp_analyst]` section, add `"gmaps_search"` and `"gmaps_details"` to `allowed_tools`.

- [ ] **Step 38: Add GOOGLE_API_KEY to auto_approve**

Find the `auto_approve` list (scripts allowed without confirmation) and add `"gmaps_places.py"`.

- [ ] **Step 39: Commit**

```bash
git commit -am "config: add GOOGLE_API_KEY passthrough + gmaps tools to erp_analyst"
```

---

## Chunk 4: E2E Tests + Final Validation

### Task 11: E2E tests with real API

**Files:**
- Create: `tests/test_e2e.py`

- [ ] **Step 40: Write E2E tests (marked with @pytest.mark.e2e)**

```python
# tests/test_e2e.py
"""E2E tests against real Google Places API. Requires GOOGLE_API_KEY env var.
Run: source .env && python3 -m pytest tests/test_e2e.py -v -m e2e
"""
import sys, os, json
import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "scripts"))

pytestmark = pytest.mark.e2e


@pytest.fixture
def api_key():
    key = os.environ.get("GOOGLE_API_KEY", "")
    if not key:
        pytest.skip("GOOGLE_API_KEY not set")
    return key


class TestSearchE2E:
    def test_search_cafes_samui(self, api_key):
        """Search for cafes in Samui — should return results with ratings."""
        from gmaps_client import PlacesClient
        from gmaps_queries import parse_search_place

        client = PlacesClient(api_key=api_key)
        raw = client.text_search("cafes in Koh Samui", language="en")
        assert len(raw["places"]) > 0

        places = [parse_search_place(p) for p in raw["places"]]
        assert any(p["rating"] and p["rating"] > 0 for p in places)
        assert all(p["place_id"] for p in places)
        assert all(p["name"] for p in places)

    def test_search_with_type_filter(self, api_key):
        """Search with includedType=restaurant."""
        from gmaps_client import PlacesClient
        from gmaps_queries import parse_search_place

        client = PlacesClient(api_key=api_key)
        raw = client.text_search("food in Chaweng", included_type="restaurant", language="en")
        places = [parse_search_place(p) for p in raw["places"]]
        assert len(places) > 0
        # Most results should be restaurants
        assert any("restaurant" in p["types"] for p in places)

    def test_search_with_location_bias(self, api_key):
        """Search with lat/lng bias for Maenam area."""
        from gmaps_client import PlacesClient

        client = PlacesClient(api_key=api_key)
        raw = client.text_search("cafe", location="9.565,100.065", radius=2000, language="en")
        assert len(raw["places"]) > 0

    def test_search_min_rating_filter(self, api_key):
        """Client-side min_rating filter."""
        from gmaps_client import PlacesClient
        from gmaps_queries import parse_search_place, filter_places

        client = PlacesClient(api_key=api_key)
        raw = client.text_search("restaurant Samui", language="en")
        places = [parse_search_place(p) for p in raw["places"]]
        filtered = filter_places(places, min_rating=4.5)
        assert all(p["rating"] >= 4.5 for p in filtered)

    def test_search_spanish_language(self, api_key):
        """Search in Spain with Spanish language."""
        from gmaps_client import PlacesClient
        from gmaps_queries import parse_search_place

        client = PlacesClient(api_key=api_key)
        raw = client.text_search("restaurantes en Barcelona", language="es")
        places = [parse_search_place(p) for p in raw["places"]]
        assert len(places) > 0


class TestDetailsE2E:
    def test_details_with_reviews(self, api_key):
        """Get details for a known place — should have reviews."""
        from gmaps_client import PlacesClient
        from gmaps_queries import parse_details_place

        client = PlacesClient(api_key=api_key)
        # First search to get a place_id
        raw = client.text_search("Sweet & Salty Maenam Samui", language="en")
        assert len(raw["places"]) > 0
        place_id = raw["places"][0]["id"]

        # Get details
        details_raw = client.get_details(place_id, include_reviews=True, language="en")
        place = parse_details_place(details_raw, include_reviews=True)

        assert place["place_id"] == place_id
        assert place["name"]
        assert place["address"]
        assert isinstance(place["reviews"], list)

    def test_details_with_photos(self, api_key):
        """Get details with photo resolution."""
        from gmaps_client import PlacesClient
        from gmaps_queries import parse_details_place

        client = PlacesClient(api_key=api_key)
        raw = client.text_search("cafe in Samui", language="en")
        assert len(raw["places"]) > 0
        place_id = raw["places"][0]["id"]

        details_raw = client.get_details(place_id, language="en")
        place = parse_details_place(details_raw, include_photos=True)

        if place.get("photos"):
            url = client.resolve_photo_url(place["photos"][0]["photo_resource"])
            assert url.startswith("https://")


class TestCLIE2E:
    def test_cli_search(self, api_key):
        """Run CLI search command end-to-end."""
        import subprocess
        result = subprocess.run(
            ["python3", os.path.expanduser("~/.zeroclaw/workspace/skills/gmaps-places/scripts/gmaps_places.py"),
             "search", "--query", "cafe Samui", "--limit", "5"],
            capture_output=True, text=True, timeout=30,
            env={**os.environ, "GOOGLE_API_KEY": api_key},
        )
        assert result.returncode == 0
        data = json.loads(result.stdout)
        assert data["success"] is True
        assert data["count"] > 0

    def test_cli_details(self, api_key):
        """Run CLI details command end-to-end."""
        import subprocess
        # First get a place_id
        result = subprocess.run(
            ["python3", os.path.expanduser("~/.zeroclaw/workspace/skills/gmaps-places/scripts/gmaps_places.py"),
             "search", "--query", "restaurant Samui", "--limit", "1"],
            capture_output=True, text=True, timeout=30,
            env={**os.environ, "GOOGLE_API_KEY": api_key},
        )
        data = json.loads(result.stdout)
        place_id = data["places"][0]["place_id"]

        # Get details
        result2 = subprocess.run(
            ["python3", os.path.expanduser("~/.zeroclaw/workspace/skills/gmaps-places/scripts/gmaps_places.py"),
             "details", "--place-id", place_id, "--reviews", "true"],
            capture_output=True, text=True, timeout=30,
            env={**os.environ, "GOOGLE_API_KEY": api_key},
        )
        data2 = json.loads(result2.stdout)
        assert data2["success"] is True
        assert data2["place"]["name"]
```

- [ ] **Step 41: Run unit tests (should all still pass)**

```bash
cd ~/.zeroclaw/workspace/skills/gmaps-places && python3 -m pytest tests/test_queries.py tests/test_client.py -v
```

- [ ] **Step 42: Run E2E tests (requires GOOGLE_API_KEY)**

```bash
source ~/.env && cd ~/.zeroclaw/workspace/skills/gmaps-places && python3 -m pytest tests/test_e2e.py -v -m e2e --timeout=60
```

- [ ] **Step 43: Commit**

```bash
git commit -am "test(gmaps-places): E2E tests against real Google Places API"
```

### Task 12: Final validation + push

- [ ] **Step 44: Run full test suite**

```bash
cd ~/.zeroclaw/workspace/skills/gmaps-places && python3 -m pytest tests/test_queries.py tests/test_client.py -v
```
Expected: 15+ unit tests pass.

- [ ] **Step 45: Run E2E smoke tests**

```bash
source ~/.env && python3 -m pytest tests/test_e2e.py -v -m e2e
```
Expected: All E2E tests pass (8 tests).

- [ ] **Step 46: Push config repo**

```bash
cd ~/.zeroclaw && git add -A && git commit -m "feat(gmaps-places): Google Maps Places skill with E2E tests" && git push
```
