#!/usr/bin/env python3
"""Validate Akasha cookbook recipe JSON files against recipe.schema.json."""

from __future__ import annotations

import json
import sys
from pathlib import Path

try:
    import jsonschema
except ImportError:
    print("jsonschema required: pip install jsonschema", file=sys.stderr)
    sys.exit(1)

ROOT = Path(__file__).resolve().parents[1]
COOKBOOK = ROOT / "spec" / "cookbook"
SCHEMA_PATH = COOKBOOK / "schema" / "recipe.schema.json"
INDEX_PATH = COOKBOOK / "index.json"


def main() -> int:
    schema = json.loads(SCHEMA_PATH.read_text(encoding="utf-8"))
    index = json.loads(INDEX_PATH.read_text(encoding="utf-8"))
    validator = jsonschema.Draft7Validator(schema)
    errors: list[str] = []
    seen_ids: set[str] = set()

    for entry in index.get("recipes", []):
        rid = entry.get("id")
        rel = entry.get("path")
        if not rid or not rel:
            errors.append(f"index entry missing id or path: {entry}")
            continue
        if rid in seen_ids:
            errors.append(f"duplicate recipe id in index: {rid}")
        seen_ids.add(rid)

        path = COOKBOOK / rel.replace("/", "\\") if "\\" in rel else COOKBOOK / rel
        if not path.is_file():
            errors.append(f"missing recipe file: {path}")
            continue

        data = json.loads(path.read_text(encoding="utf-8"))
        if data.get("id") != rid:
            errors.append(f"{path}: id mismatch index={rid} file={data.get('id')}")
        if entry.get("category") and data.get("category") != entry.get("category"):
            errors.append(f"{path}: category mismatch index vs file")

        for err in validator.iter_errors(data):
            loc = ".".join(str(p) for p in err.path) or "(root)"
            errors.append(f"{path}: {loc}: {err.message}")

    if errors:
        print("Cookbook validation FAILED:", file=sys.stderr)
        for e in errors:
            print(f"  - {e}", file=sys.stderr)
        return 1

    print(f"OK: {len(seen_ids)} recipes validated")
    return 0


if __name__ == "__main__":
    sys.exit(main())
