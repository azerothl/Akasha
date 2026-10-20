"""Verify that CPU and CUDA release zips contain the shared Windows clients."""
import hashlib
import json
from pathlib import Path
import sys
import zipfile


ARCHIVES = (
    "akasha-windows-x86_64.zip",
    "akasha-windows-x86_64-cuda.zip",
)
MANIFEST = "windows-clients.json"


def verify(directory):
    directory = Path(directory)
    archives = [directory / name for name in ARCHIVES]
    missing = [path.name for path in archives if not path.is_file()]
    if missing:
        raise ValueError(f"missing required Windows archives: {', '.join(missing)}")

    reference = None
    for archive_path in archives:
        with zipfile.ZipFile(archive_path) as archive:
            try:
                manifest = json.loads(archive.read(MANIFEST))
                clients = manifest["clients"]
            except (KeyError, json.JSONDecodeError, zipfile.BadZipFile) as exc:
                raise ValueError(f"{archive_path}: invalid or missing {MANIFEST}: {exc}") from exc
            if not isinstance(clients, list) or not clients:
                raise ValueError(f"{archive_path}: manifest has no clients")

            actual = {}
            for client in clients:
                name = client.get("name")
                expected_hash = client.get("sha256")
                if not isinstance(name, str) or Path(name).name != name:
                    raise ValueError(f"{archive_path}: invalid client name {name!r}")
                if not isinstance(expected_hash, str) or len(expected_hash) != 64:
                    raise ValueError(f"{archive_path}: invalid SHA-256 for {name}")
                try:
                    payload = archive.read(name)
                except KeyError as exc:
                    raise ValueError(f"{archive_path}: missing {name}") from exc
                digest = hashlib.sha256(payload).hexdigest()
                if digest != expected_hash.lower():
                    raise ValueError(f"{archive_path}: SHA-256 mismatch for {name}")
                actual[name] = digest

            if reference is None:
                reference = actual
            elif actual != reference:
                raise ValueError(f"{archive_path}: Windows clients differ from the CPU archive")
            print(f"{archive_path.name}: verified {len(actual)} shared clients")


def main():
    if len(sys.argv) != 2:
        print(f"Usage: {Path(sys.argv[0]).name} RELEASE_DIRECTORY", file=sys.stderr)
        return 2
    try:
        verify(sys.argv[1])
    except (OSError, ValueError) as exc:
        print(f"Release archive verification failed: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
