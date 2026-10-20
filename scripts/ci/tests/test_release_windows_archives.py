import importlib.util
from pathlib import Path
import tempfile
import zipfile
import hashlib
import json
import unittest

SPEC = importlib.util.spec_from_file_location(
    "verify_release_windows_archives", Path(__file__).resolve().parents[1] / "verify_release_windows_archives.py")
verify_archives = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(verify_archives)


class ReleaseWindowsArchiveTests(unittest.TestCase):
    def write_archive(self, directory, filename, clients):
        with zipfile.ZipFile(directory / filename, "w") as archive:
            archive.writestr("windows-clients.json", json.dumps({
                "clients": [
                    {"name": name, "sha256": hashlib.sha256(payload).hexdigest()}
                    for name, payload in clients.items()
                ]
            }))
            for name, payload in clients.items():
                archive.writestr(name, payload)

    def test_accepts_identical_clients_in_cpu_and_cuda_archives(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            clients = {"akasha.exe": b"cli", "akasha-tui.exe": b"tui"}
            for archive in verify_archives.ARCHIVES:
                self.write_archive(directory, archive, clients)
            verify_archives.verify(directory)

    def test_rejects_different_clients_between_archives(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            clients = {"akasha.exe": b"cli", "akasha-tui.exe": b"tui"}
            for archive in verify_archives.ARCHIVES[:-1]:
                self.write_archive(directory, archive, clients)
            self.write_archive(directory, verify_archives.ARCHIVES[-1], {
                "akasha.exe": b"different", "akasha-tui.exe": b"tui"
            })
            with self.assertRaisesRegex(ValueError, "clients differ"):
                verify_archives.verify(directory)

    def test_rejects_client_hash_mismatch(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            clients = {"akasha.exe": b"cli", "akasha-tui.exe": b"tui"}
            for archive_name in verify_archives.ARCHIVES[:-1]:
                self.write_archive(directory, archive_name, clients)
            with zipfile.ZipFile(directory / verify_archives.ARCHIVES[-1], "w") as archive:
                archive.writestr("windows-clients.json", json.dumps({"clients": [
                    {"name": "akasha.exe", "sha256": "0" * 64},
                    {"name": "akasha-tui.exe", "sha256": hashlib.sha256(b"tui").hexdigest()}
                ]}))
                archive.writestr("akasha.exe", b"cli")
                archive.writestr("akasha-tui.exe", b"tui")
            with self.assertRaisesRegex(ValueError, "SHA-256 mismatch"):
                verify_archives.verify(directory)


if __name__ == "__main__":
    unittest.main()
