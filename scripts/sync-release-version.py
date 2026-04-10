#!/usr/bin/env python3
"""
Sync Akasha product version across workspace Cargo.toml and akasha-ui (Tauri) files.
Usage: python scripts/sync-release-version.py X.Y.Z [REPO_ROOT]
"""
from __future__ import annotations

import re
import sys
from pathlib import Path

SEMVER_RE = re.compile(r"^\d+\.\d+\.\d+$")


def die(msg: str, code: int = 1) -> None:
    print(msg, file=sys.stderr)
    sys.exit(code)


def set_workspace_package_version(cargo_toml: Path, version: str) -> None:
    text = cargo_toml.read_text(encoding="utf-8")
    new_text, n = re.subn(
        r'(\[workspace\.package\]\s*\n)version = "[^"]*"',
        rf'\1version = "{version}"',
        text,
        count=1,
    )
    if n != 1:
        die(f"Could not find [workspace.package] / version in {cargo_toml}")
    cargo_toml.write_text(new_text, encoding="utf-8")


def set_akasha_ui_cargo_version(path: Path, version: str) -> None:
    text = path.read_text(encoding="utf-8")
    new_text, n = re.subn(
        r'^version = "[^"]*"$',
        f'version = "{version}"',
        text,
        count=1,
        flags=re.MULTILINE,
    )
    if n != 1:
        die(f"Could not find single package version line in {path}")
    path.write_text(new_text, encoding="utf-8")


def set_package_json_version(path: Path, version: str) -> None:
    text = path.read_text(encoding="utf-8")
    new_text, n = re.subn(
        r'^  "version": "[^"]*",?\s*$',
        f'  "version": "{version}",',
        text,
        count=1,
        flags=re.MULTILINE,
    )
    if n != 1:
        die(f"Could not update root version in {path}")
    path.write_text(new_text, encoding="utf-8")


def set_package_lock_versions(path: Path, version: str) -> None:
    text = path.read_text(encoding="utf-8")
    new_text, n1 = re.subn(
        r'^  "version": "[^"]*",?\s*$',
        f'  "version": "{version}",',
        text,
        count=1,
        flags=re.MULTILINE,
    )
    new_text, n2 = re.subn(
        r'(\n      "name": "akasha-ui",\n      "version": ")[^"]+(",?\s*)',
        rf"\g<1>{version}\2",
        new_text,
        count=1,
    )
    if n1 != 1 or n2 != 1:
        die(f"Could not update both akasha-ui versions in {path} (got {n1}, {n2})")
    path.write_text(new_text, encoding="utf-8")


def set_tauri_conf_version(path: Path, version: str) -> None:
    text = path.read_text(encoding="utf-8")
    new_text, n = re.subn(
        r'^  "version": "[^"]*",?\s*$',
        f'  "version": "{version}",',
        text,
        count=1,
        flags=re.MULTILINE,
    )
    if n != 1:
        die(f"Could not update version in {path}")
    path.write_text(new_text, encoding="utf-8")


def main() -> None:
    if len(sys.argv) < 2:
        die("Usage: sync-release-version.py X.Y.Z [REPO_ROOT]")
    version = sys.argv[1].strip()
    if not SEMVER_RE.match(version):
        die(f"Invalid version (expected MAJOR.MINOR.PATCH): {version!r}")

    root = Path(sys.argv[2]).resolve() if len(sys.argv) > 2 else Path.cwd()
    if not (root / "Cargo.toml").is_file():
        die(f"Not an Akasha repo root (no Cargo.toml): {root}")

    cargo_toml = root / "Cargo.toml"
    pkg_json = root / "apps" / "akasha-ui" / "package.json"
    lock_json = root / "apps" / "akasha-ui" / "package-lock.json"
    ui_cargo = root / "apps" / "akasha-ui" / "src-tauri" / "Cargo.toml"
    tauri_conf = root / "apps" / "akasha-ui" / "src-tauri" / "tauri.conf.json"

    for p in (cargo_toml, pkg_json, lock_json, ui_cargo, tauri_conf):
        if not p.is_file():
            die(f"Missing file: {p}")

    set_workspace_package_version(cargo_toml, version)
    set_package_json_version(pkg_json, version)
    set_package_lock_versions(lock_json, version)
    set_akasha_ui_cargo_version(ui_cargo, version)
    set_tauri_conf_version(tauri_conf, version)

    print(f"Synced release version to {version}")
    print(f"  {cargo_toml.relative_to(root)}")
    print(f"  {pkg_json.relative_to(root)}")
    print(f"  {lock_json.relative_to(root)}")
    print(f"  {ui_cargo.relative_to(root)}")
    print(f"  {tauri_conf.relative_to(root)}")


if __name__ == "__main__":
    main()
