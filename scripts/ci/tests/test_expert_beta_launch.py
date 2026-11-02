"""A beta launch must not take over an unrelated local service or data path."""
import hashlib
import http.server
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import threading
import unittest

ROOT = Path(__file__).resolve().parents[3]


@unittest.skipUnless(os.name == 'nt', 'Windows PowerShell launcher')
class ExpertLaunchSafetyTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / 'scripts').mkdir()
        shutil.copy2(ROOT / 'scripts/expert-beta-launch.ps1', self.root / 'scripts/expert-beta-launch.ps1')
        (self.root / 'unused.bin').write_bytes(b'not an executable')
        self.manifest = dict(schema_version=1, edition='expert-beta', files=[
            dict(path='unused.bin', sha256=hashlib.sha256(b'not an executable').hexdigest())])

    def invoke(self, profile, port):
        (self.root / 'release-beta-package.json').write_text(json.dumps(self.manifest))
        return subprocess.run(['powershell.exe', '-NoProfile', '-ExecutionPolicy', 'Bypass',
            '-File', str(self.root / 'scripts/expert-beta-launch.ps1'), '-ProfileDir', str(profile),
            '-Port', str(port)], capture_output=True, text=True, timeout=20)

    def test_busy_port_preserves_the_foreign_service_and_profile(self):
        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), http.server.BaseHTTPRequestHandler)
        self.addCleanup(server.server_close)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(server.shutdown)
        profile = self.root / 'new-profile'
        result = self.invoke(profile, server.server_port)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('already in use', result.stderr)
        self.assertTrue(thread.is_alive())
        self.assertFalse(profile.exists())

    def test_relative_profile_is_rejected_before_starting_any_binary(self):
        result = self.invoke('relative-profile', 3876)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('must be absolute', result.stderr)
        self.assertFalse((self.root / 'relative-profile').exists())

    def test_tampered_component_is_rejected_before_profile_creation(self):
        (self.root / 'unused.bin').write_bytes(b'changed')
        profile = self.root / 'new-profile'
        result = self.invoke(profile, 3876)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('Changed package component', result.stderr)
        self.assertFalse(profile.exists())
