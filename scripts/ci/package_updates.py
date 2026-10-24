"""Publish Windows update components independently of the installation ZIPs."""
import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import re
import zipfile

PUBLIC_REPO = "https://github.com/azerothl/Akasha_app"
VARIANTS = ("windows-x86_64", "windows-x86_64-cuda")


def version(value):
    if value == "workspace":
        cargo = (Path(__file__).resolve().parents[2] / "Cargo.toml").read_text(encoding="utf-8")
        value = re.search(r'\[workspace\.package\]\s*\nversion = "([^"]+)"', cargo).group(1)
    value = value.removeprefix("v")
    if not re.fullmatch(r"\d+\.\d+\.\d+", value):
        raise ValueError("Updates require a stable X.Y.Z version")
    return value


def marker(directory, target, release_version):
    data = {
        "schema_version": 1, "target": target, "version": version(release_version),
        "manifest_url": f"{PUBLIC_REPO}/releases/download/updates-stable/akasha-update-{target}.json",
    }
    (Path(directory) / "akasha-install.json").write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")


def build(directory, release_version, asset_base=None):
    directory = Path(directory)
    release_version = version(release_version)
    asset_base = asset_base or f"{PUBLIC_REPO}/releases/download/updates-v{release_version}"
    for target in VARIANTS:
        source = directory / f"akasha-{target}.zip"
        with zipfile.ZipFile(source) as package:
            try:
                metadata = json.loads(package.read("akasha-install.json"))
            except KeyError as error:
                raise ValueError(f"Incomplete update package: {target}") from error
            if metadata.get("version") != release_version or metadata.get("target") != target or metadata.get("schema_version") != 1:
                raise ValueError(f"Update metadata does not match release {release_version}: {target}")
            groups = {}
            seen = set()
            for info in sorted(package.infolist(), key=lambda entry: entry.filename):
                if info.is_dir():
                    continue
                name = info.filename
                path = PurePosixPath(name)
                if path.is_absolute() or ".." in path.parts or "\\" in name or ":" in name:
                    raise ValueError(f"Unsafe update path: {name}")
                if name.lower() in seen or (info.external_attr >> 16) & 0o170000 == 0o120000:
                    raise ValueError(f"Duplicate or symlink update path: {name}")
                seen.add(name.lower())
                if name in ("akasha.exe", "akasha-daemon.exe", "akasha-tui.exe"):
                    group = path.stem
                elif len(path.parts) == 1 and name.lower().endswith(".dll"):
                    group = path.stem
                elif name == "akasha-install.json" or path.parts[0] in ("docs", "spec", "playwright-runner", "scripts"):
                    group = "resources"
                else:
                    continue
                groups.setdefault(group, []).append((name, package.read(info)))
            required = {"akasha", "akasha-daemon", "akasha-tui", "resources"}
            if not required.issubset(groups) or "scripts/update.ps1" not in seen or "akasha-install.json" not in seen:
                raise ValueError(f"Incomplete update package: {target}")
            components = []
            for group, files in sorted(groups.items()):
                # Content names allow reuse across versions and prevent asset-name collisions.
                inventory = [{"path": name, "size": len(payload), "sha256": hashlib.sha256(payload).hexdigest()}
                             for name, payload in files]
                identity = hashlib.sha256(json.dumps(inventory, sort_keys=True).encode()).hexdigest()
                asset_name = f"akasha-component-{target}-{group}-{identity[:16]}.zip"
                asset = directory / asset_name
                with zipfile.ZipFile(asset, "w", zipfile.ZIP_DEFLATED, compresslevel=6) as archive:
                    for name, payload in files:
                        entry = zipfile.ZipInfo(name, date_time=(2020, 1, 1, 0, 0, 0))
                        entry.compress_type = zipfile.ZIP_DEFLATED
                        archive.writestr(entry, payload)
                components.append({"name": group, "url": f"{asset_base}/{asset_name}",
                                   "sha256": hashlib.sha256(asset.read_bytes()).hexdigest(),
                                   "size": asset.stat().st_size, "files": inventory})
            manifest = {"schema_version": 1, "target": target, "version": release_version, "components": components}
            (directory / f"akasha-update-{target}.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    commands = parser.add_subparsers(dest="command", required=True)
    m = commands.add_parser("marker")
    m.add_argument("directory")
    m.add_argument("target", choices=VARIANTS)
    m.add_argument("version")
    b = commands.add_parser("build")
    b.add_argument("directory")
    b.add_argument("version")
    args = parser.parse_args()
    if args.command == "marker":
        marker(args.directory, args.target, args.version)
    else:
        build(args.directory, args.version)
