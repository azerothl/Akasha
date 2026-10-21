"""Record exact shared client bytes, load-time DLLs and Cargo features for review."""
import hashlib
import json
from pathlib import Path
import subprocess

from pe_imports import imports


def main():
    output = Path("target/x86_64-pc-windows-msvc/release")
    clients = []
    for name in ("akasha.exe", "akasha-tui.exe"):
        path = output / name
        clients.append(dict(name=name, sha256=hashlib.sha256(path.read_bytes()).hexdigest(),
                            bytes=path.stat().st_size, load_time_dlls=imports(path)))
    tree = subprocess.check_output(
        ["cargo", "tree", "--locked", "--target", "x86_64-pc-windows-msvc",
         "-p", "akasha-cli", "-p", "akasha-tui", "--format", "{p} {f}"], text=True)
    Path("windows-clients-dependencies.txt").write_text(tree, encoding="utf-8")
    (output / "windows-clients.json").write_text(
        json.dumps(dict(clients=clients, features="package defaults; no daemon features"), indent=2) + "\n",
        encoding="utf-8")


if __name__ == "__main__":
    main()
