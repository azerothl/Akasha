import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
import zipfile

ROOT = Path(__file__).resolve().parents[3]
SPEC = importlib.util.spec_from_file_location("package_updates", ROOT / "scripts/ci/package_updates.py")
updates = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(updates)


def package(directory, target):
    updates.marker(directory, target, "1.1.0")
    payloads = {
        "akasha.exe": b"unchanged CLI", "akasha-tui.exe": b"unchanged TUI",
        "akasha-daemon.exe": b"new daemon", "cudart64_12.dll": b"unchanged CUDA",
        "scripts/update.ps1": (ROOT / "scripts/update.ps1").read_bytes(),
        "spec/09_event_model.yaml": b"updated spec", "docs/user_guide.md": b"updated docs",
        "akasha-install.json": (directory / "akasha-install.json").read_bytes(),
    }
    with zipfile.ZipFile(directory / f"akasha-{target}.zip", "w") as archive:
        for name, content in payloads.items():
            archive.writestr(name, content)
    return payloads


class PackageUpdateTests(unittest.TestCase):
    def test_components_are_deterministic_and_all_bytes_are_verified(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            for target in updates.VARIANTS:
                package(directory, target)
            updates.build(directory, "v1.1.0")
            before = {p.name: p.read_bytes() for p in directory.glob("akasha-component-*.zip")}
            for target in updates.VARIANTS:
                manifest = json.loads((directory / f"akasha-update-{target}.json").read_text())
                self.assertEqual(manifest["target"], target)
                self.assertEqual(len(manifest["components"]), 5)
                for component in manifest["components"]:
                    asset = directory / component["url"].rsplit("/", 1)[1]
                    self.assertEqual(hashlib.sha256(asset.read_bytes()).hexdigest(), component["sha256"])
                    self.assertEqual(asset.stat().st_size, component["size"])
                    with zipfile.ZipFile(asset) as archive:
                        for file in component["files"]:
                            payload = archive.read(file["path"])
                            self.assertEqual(len(payload), file["size"])
                            self.assertEqual(hashlib.sha256(payload).hexdigest(), file["sha256"])
            updates.build(directory, "1.1.0")
            self.assertEqual(before, {p.name: p.read_bytes() for p in directory.glob("akasha-component-*.zip")})

    def test_rejects_unsafe_archive_paths(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            package(directory, updates.VARIANTS[0])
            with zipfile.ZipFile(directory / f"akasha-{updates.VARIANTS[0]}.zip", "a") as archive:
                archive.writestr("../outside.exe", b"invalid")
            with self.assertRaisesRegex(ValueError, "Unsafe"):
                updates.build(directory, "1.1.0")

    def test_rejects_version_mismatch(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            package(directory, updates.VARIANTS[0])
            with self.assertRaisesRegex(ValueError, "does not match release"):
                updates.build(directory, "1.2.0")

    def test_rejects_case_collisions_and_incomplete_packages(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            package(directory, updates.VARIANTS[0])
            archive_path = directory / f"akasha-{updates.VARIANTS[0]}.zip"
            with zipfile.ZipFile(archive_path, "a") as archive:
                archive.writestr("AKASHA.EXE", b"collision")
            with self.assertRaisesRegex(ValueError, "Duplicate"):
                updates.build(directory, "1.1.0")
            with zipfile.ZipFile(archive_path, "w") as archive:
                archive.writestr("akasha.exe", b"incomplete")
            with self.assertRaisesRegex(ValueError, "Incomplete"):
                updates.build(directory, "1.1.0")

    @unittest.skipUnless(os.name == "nt", "Requires Windows PowerShell 5.1")
    def test_windows_prepare_confirmation_integrity_apply_and_rollback(self):
        with tempfile.TemporaryDirectory(prefix="akasha update ") as temporary:
            directory = Path(temporary)
            payloads = {}
            for target in updates.VARIANTS:
                payloads = package(directory, target)
            updates.build(directory, "1.1.0")
            install = directory / "installed"
            with zipfile.ZipFile(directory / "evil.zip", "w") as archive:
                archive.writestr("../outside.exe", b"escape")
            install.mkdir()
            for name, payload in payloads.items():
                destination = install / name
                destination.parent.mkdir(parents=True, exist_ok=True)
                destination.write_bytes(payload)
            (install / "akasha-daemon.exe").write_bytes(b"old daemon")
            updates.marker(install, "windows-x86_64-cuda", "1.0.0")
            # Reproduce hosted Windows TEMP paths such as RUNNER~1. Preserve
            # the directory identity even when .NET expands its 8.3 spelling.
            import ctypes
            buffer = ctypes.create_unicode_buffer(32768)
            short_path = ctypes.windll.kernel32.GetShortPathNameW
            short_path.argtypes = [ctypes.c_wchar_p, ctypes.c_wchar_p, ctypes.c_uint32]
            short_path.restype = ctypes.c_uint32
            fixture = buffer.value if short_path(str(directory), buffer, len(buffer)) else str(directory)
            result = subprocess.run([
                "powershell.exe", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File",
                str(ROOT / "scripts/ci/test_update.ps1"), "-Fixture", fixture,
            ], capture_output=True, text=True, timeout=120)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn("Windows updater regression checks passed", result.stdout)


if __name__ == "__main__":
    unittest.main()
