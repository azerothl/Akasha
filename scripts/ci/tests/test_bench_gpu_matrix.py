import hashlib
import io
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / 'spec/dev/quality'))

from bench_gpu_matrix import model_file


class MatrixModelTests(unittest.TestCase):
    def test_models_are_downloaded_and_checked_inside_the_selected_ci_directory(self):
        payload = b'synthetic catalog weights'
        entry = {'filename': 'catalog.gguf', 'sha256': hashlib.sha256(payload).hexdigest(), 'url': 'https://example.invalid/catalog'}
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder) / 'ci/models/embedded'
            with patch('bench_gpu_matrix.urllib.request.urlopen', return_value=io.BytesIO(payload)) as download:
                path = model_file(entry, root)
                self.assertEqual(path, root / entry['filename'])
                self.assertEqual(path.read_bytes(), payload)
            with patch('bench_gpu_matrix.urllib.request.urlopen') as download:
                self.assertEqual(model_file(entry, root), path)
                download.assert_not_called()

    def test_tampered_download_never_replaces_an_existing_model(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / 'catalog.gguf').write_bytes(b'previous fixture')
            entry = {'filename': 'catalog.gguf', 'sha256': '0' * 64, 'url': 'https://example.invalid/catalog'}
            with patch('bench_gpu_matrix.urllib.request.urlopen', return_value=io.BytesIO(b'altered')):
                with self.assertRaisesRegex(ValueError, 'checksum mismatch'):
                    model_file(entry, root)
            self.assertEqual((root / 'catalog.gguf').read_bytes(), b'previous fixture')
            self.assertFalse((root / 'catalog.partial').exists())

    def test_catalog_filenames_cannot_escape_the_model_directory(self):
        with tempfile.TemporaryDirectory() as folder:
            with self.assertRaises(ValueError):
                model_file({'filename': '../other-profile.gguf'}, folder)


if __name__ == '__main__':
    unittest.main()
