import base64
import os
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[3]
PWSH = shutil.which('pwsh')


def quote(value):
    return "'" + str(value).replace("'", "''") + "'"


def powershell(source, env=None):
    command = base64.b64encode(source.encode('utf-16-le')).decode('ascii')
    return subprocess.run([PWSH, '-NoProfile', '-NonInteractive', '-EncodedCommand', command],
                          env=env, capture_output=True, text=True, timeout=40)


@unittest.skipUnless(PWSH, 'PowerShell is needed for runner isolation checks')
class NvidiaRunnerTests(unittest.TestCase):
    def test_only_current_main_push_or_dispatch_is_authorized(self):
        policy = ROOT / 'scripts/ci/nvidia-main-policy.ps1'
        revision = 'a' * 40
        source = f"$ErrorActionPreference='Stop'; . {quote(policy)}; "
        for event in ('push', 'workflow_dispatch'):
            result = powershell(source + f"Assert-NvidiaMainJob 'azerothl/Akasha' 'refs/heads/main' '{event}' '{revision}' '{revision}'")
            self.assertEqual(result.returncode, 0, result.stderr)
        denied = [('other/repo', 'refs/heads/main', 'push', revision, revision),
                  ('azerothl/Akasha', 'refs/pull/1/merge', 'pull_request', revision, revision),
                  ('azerothl/Akasha', 'refs/heads/feature', 'workflow_dispatch', revision, revision),
                  ('azerothl/Akasha', 'refs/heads/main', 'pull_request_target', revision, revision),
                  ('azerothl/Akasha', 'refs/heads/main', 'push', revision, 'b' * 40),
                  ('azerothl/Akasha', 'refs/heads/main', 'push', '', '')]
        for arguments in denied:
            with self.subTest(arguments=arguments):
                result = powershell(source + 'Assert-NvidiaMainJob ' + ' '.join(map(quote, arguments)))
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('accepts only', result.stderr)

    def test_actual_dot_sourced_hook_rejects_a_pr_without_network_or_test_bypass(self):
        hook = ROOT / 'scripts/ci/assert-nvidia-main-job.ps1'
        env = {**os.environ, 'GITHUB_REPOSITORY': 'azerothl/Akasha', 'GITHUB_REF': 'refs/pull/999/merge',
               'GITHUB_EVENT_NAME': 'pull_request', 'GITHUB_SHA': 'a' * 40, 'AKASHA_GPU_HOOK_TEST': '1'}
        result = powershell(f"$ErrorActionPreference='Stop'; . {quote(hook)}", env)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('accepts only', result.stderr)

    def test_preparation_never_reuses_an_occupied_port_or_an_existing_profile(self):
        with tempfile.TemporaryDirectory() as folder, socket.socket() as occupied:
            occupied.bind(('127.0.0.1', 0))
            occupied.listen()
            root = Path(folder)
            cuda = root / 'cuda'
            (cuda / 'bin').mkdir(parents=True)
            (cuda / 'bin/nvcc.exe').touch()
            output = root / 'environment'
            paths = root / 'path'
            env = {**os.environ, 'CUDA_PATH': str(cuda), 'GITHUB_PATH': str(paths)}
            source = ("$ErrorActionPreference='Stop'; function global:nvidia-smi { '8.9'; $global:LASTEXITCODE=0 }; "
                      f"& {quote(ROOT / 'scripts/ci/prepare-gpu-ci.ps1')} -OutputFile {quote(output)} -TemporaryRoot {quote(root)}")
            for _ in range(2):
                result = powershell(source, env)
                self.assertEqual(result.returncode, 0, result.stderr)
            values = {}
            for line in output.read_text(encoding='utf-8-sig').splitlines():
                name, value = line.split('=', 1)
                values.setdefault(name, []).append(value)
            for value in values['AKASHA_PORT']:
                self.assertNotEqual(int(value), occupied.getsockname()[1])
                with socket.socket() as probe:
                    probe.bind(('127.0.0.1', int(value)))
            self.assertNotEqual(*values['AKASHA_DATA_DIR'])
            for value in values['AKASHA_DATA_DIR']:
                self.assertTrue(Path(value).is_relative_to(root))
                self.assertTrue(Path(value).is_dir())
            self.assertEqual(values['CMAKE_CUDA_ARCHITECTURES'], ['89', '89'])

    def test_powershell_sources_parse_without_errors(self):
        for name in ('prepare-gpu-ci.ps1', 'nvidia-main-policy.ps1', 'assert-nvidia-main-job.ps1', 'start-nvidia-runner.ps1', 'reset-nvidia-runner-state.ps1'):
            with self.subTest(name=name):
                result = powershell(f"$tokens=$null; $errors=$null; [System.Management.Automation.Language.Parser]::ParseFile({quote(ROOT / 'scripts/ci' / name)}, [ref]$tokens, [ref]$errors) | Out-Null; if ($errors.Count) {{ $errors | Out-String | Write-Error; exit 1 }}")
                self.assertEqual(result.returncode, 0, result.stderr)

    def test_jit_replacement_clears_only_its_hidden_registration_files(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            for name in ('.runner', '.credentials', '.credentials_rsaparams', 'work.txt'):
                (root / name).write_text('synthetic fixture')
            source = f"$ErrorActionPreference='Stop'; . {quote(ROOT / 'scripts/ci/reset-nvidia-runner-state.ps1')}; "
            if os.name == 'nt':
                source += f"(Get-Item -Force -LiteralPath {quote(root / '.runner')}).Attributes = [IO.FileAttributes]::Hidden; "
            result = powershell(source + f"Reset-NvidiaRunnerState -Application {quote(root)}")
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(sorted(item.name for item in root.iterdir()), ['work.txt'])
            self.assertEqual((root / 'work.txt').read_text(), 'synthetic fixture')


if __name__ == '__main__':
    unittest.main()
