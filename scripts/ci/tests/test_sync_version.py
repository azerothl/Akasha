"""Version synchronization must not invalidate locked release builds."""
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[3]


class SyncVersionTests(unittest.TestCase):
    def test_product_versions_and_locks_change_without_dependency_upgrades(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            workspace = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]
            files = [
                "Cargo.toml", "Cargo.lock", "apps/akasha-ui/package.json",
                "apps/akasha-ui/package-lock.json", "apps/akasha-ui/src-tauri/Cargo.toml",
                "apps/akasha-ui/src-tauri/Cargo.lock", "apps/akasha-ui/src-tauri/tauri.conf.json",
                *[member + "/Cargo.toml" for member in workspace["members"]],
            ]
            for name in files:
                destination = root / name
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(ROOT / name, destination)
            before = tomllib.loads((root / "Cargo.lock").read_text())["package"]
            subprocess.run([sys.executable, str(ROOT / "scripts/sync-release-version.py"),
                            "0.11.1", str(root)], check=True, capture_output=True)
            after = tomllib.loads((root / "Cargo.lock").read_text())["package"]
            names = {tomllib.loads((root / member / "Cargo.toml").read_text())["package"]["name"]
                     for member in workspace["members"]}
            self.assertEqual([p for p in before if p["name"] not in names],
                             [p for p in after if p["name"] not in names])
            self.assertEqual({p["version"] for p in after if p["name"] in names}, {"0.11.1"})
            ui = tomllib.loads((root / "apps/akasha-ui/src-tauri/Cargo.lock").read_text())
            self.assertEqual(next(p["version"] for p in ui["package"] if p["name"] == "akasha-ui"), "0.11.1")
            self.assertEqual(json.loads((root / "apps/akasha-ui/package.json").read_text())["version"], "0.11.1")


if __name__ == "__main__":
    unittest.main()
