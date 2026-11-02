"""A competing Clang on Windows must not supply bindgen's builtin headers."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import unittest

ROOT = Path(__file__).resolve().parents[3]


@unittest.skipUnless(os.name == 'nt' and shutil.which('pwsh'), 'Windows LLVM environment')
class LlvmWindowsTests(unittest.TestCase):
    def test_selected_compiler_library_and_intrinsic_headers_match(self):
        llvm = Path(os.environ.get('ProgramFiles', 'C:/Program Files')) / 'LLVM/bin'
        if not (llvm / 'libclang.dll').is_file():
            self.skipTest('LLVM is not installed')
        script = str(ROOT / 'scripts/ci/prepare-llvm-windows.ps1').replace("'", "''")
        command = f"""
        $env:CLANG_PATH='C:/unrelated-visual-studio/clang.exe'
        $env:LIBCLANG_PATH='C:/unrelated-visual-studio'
        Remove-Item Env:GITHUB_ENV -ErrorAction SilentlyContinue
        $pathBefore=$env:PATH
        $selected=& '{script}'
        if ($env:PATH -ne $pathBefore) {{ throw 'MSVC linker search order changed' }}
        '#include <mmintrin.h>' | & $env:CLANG_PATH -x c++ -fsyntax-only -
        if ($LASTEXITCODE) {{ throw 'Selected intrinsic headers cannot be parsed' }}
        @{{selected=$selected;clang=$env:CLANG_PATH;library=$env:LIBCLANG_PATH;args=$env:BINDGEN_EXTRA_CLANG_ARGS}} | ConvertTo-Json -Depth 3
        """
        result = subprocess.run(['pwsh', '-NoProfile', '-Command', command], capture_output=True, text=True, timeout=60)
        self.assertEqual(result.returncode, 0, result.stderr)
        receipt = json.loads(result.stdout)
        self.assertEqual(Path(receipt['clang']), llvm / 'clang.exe')
        self.assertEqual(Path(receipt['library']), llvm)
        self.assertEqual(Path(receipt['selected']['clang']), Path(receipt['clang']))
        self.assertIn('-resource-dir=', receipt['args'])


if __name__ == '__main__':
    unittest.main()
