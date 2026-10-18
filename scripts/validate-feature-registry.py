#!/usr/bin/env python3
"""Lightweight checks on spec/feature_evolution_tracking.md."""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
REGISTRY = ROOT / "spec" / "feature_evolution_tracking.md"


def main() -> int:
    if not REGISTRY.is_file():
        print(f"Missing registry: {REGISTRY}", file=sys.stderr)
        return 1

    text = REGISTRY.read_text(encoding="utf-8")
    errors: list[str] = []

    # Table rows: | ... | Partially documented | ... |
    partial_rows: list[str] = []
    for line in text.splitlines():
        if line.startswith("|") and "Partially documented" in line:
            partial_rows.append(line)

    for row in partial_rows:
        if not re.search(r"(suivi|issue|#\d+|\[#\d+\]|GitHub)", row, re.IGNORECASE):
            errors.append(
                "Partially documented row must mention suivi/issue/#NNN in the row or list below:\n"
                f"  {row[:120]}..."
            )

    # Companion hardware must not be marked Implemented
    for line in text.splitlines():
        if not line.startswith("|"):
            continue
        lower = line.lower()
        if ("fnk0104" in lower or "esp32 matériel" in lower or "esp32 materiel" in lower):
            if "Implemented" in line and "Planned" not in line.split("Implemented")[0]:
                # implementation column should be Planned only
                cols = [c.strip() for c in line.split("|")]
                if len(cols) > 3 and cols[3] == "Implemented":
                    errors.append(f"Companion ESP32 hardware must not be Implemented:\n  {line}")

    if "Preuves de release" not in text:
        errors.append("Registry must document release proof locations (§ Preuves de release).")

    if errors:
        for e in errors:
            print(f"ERROR: {e}", file=sys.stderr)
        return 1

    print(f"OK: feature registry validated ({len(partial_rows)} partially documented row(s)).")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
