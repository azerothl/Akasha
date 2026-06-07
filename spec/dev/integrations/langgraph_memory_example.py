"""
LangGraph memory integration example (G1).

This script demonstrates how to call Akasha external memory APIs:
- GET /api/memory/advanced-settings
- GET /api/memory/search?q=...&top_k=...
- GET /api/memory/recall-metrics
"""

from __future__ import annotations

import json
import os
from dataclasses import dataclass
from typing import Any, Dict
from urllib.parse import quote
from urllib.request import Request, urlopen

BASE_URL = os.environ.get("AKASHA_BASE_URL", "http://127.0.0.1:3876")


def get_json(path: str) -> Dict[str, Any]:
    req = Request(f"{BASE_URL}{path}", method="GET")
    with urlopen(req, timeout=20) as resp:
        body = resp.read().decode("utf-8", errors="replace")
    return json.loads(body)


@dataclass
class MemoryNodeState:
    query: str
    top_k: int = 5
    advanced_settings: Dict[str, Any] | None = None
    recall_metrics: Dict[str, Any] | None = None
    search_results: Dict[str, Any] | None = None


def fetch_settings(state: MemoryNodeState) -> MemoryNodeState:
    state.advanced_settings = get_json("/api/memory/advanced-settings")
    return state


def fetch_metrics(state: MemoryNodeState) -> MemoryNodeState:
    state.recall_metrics = get_json("/api/memory/recall-metrics")
    return state


def run_search(state: MemoryNodeState) -> MemoryNodeState:
    q = quote(state.query)
    state.search_results = get_json(f"/api/memory/search?q={q}&top_k={state.top_k}")
    return state


def run_graph(query: str) -> MemoryNodeState:
    # Minimal linear flow equivalent to a LangGraph chain:
    # settings -> metrics -> search
    state = MemoryNodeState(query=query)
    state = fetch_settings(state)
    state = fetch_metrics(state)
    state = run_search(state)
    return state


if __name__ == "__main__":
    out = run_graph("What did we decide about memory hygiene?")
    print(json.dumps(
        {
            "query": out.query,
            "advanced_settings": out.advanced_settings,
            "recall_metrics": out.recall_metrics,
            "search_results": out.search_results,
        },
        ensure_ascii=False,
        indent=2,
    ))
