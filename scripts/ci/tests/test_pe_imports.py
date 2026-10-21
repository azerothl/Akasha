import importlib.util
from pathlib import Path
import struct
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location(
    "pe_imports", Path(__file__).resolve().parents[1] / "pe_imports.py")
pe = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(pe)


class PEImportsTests(unittest.TestCase):
    def fixture(self, magic):
        data = bytearray(1024)
        data[:2] = b"MZ"
        struct.pack_into("<I", data, 0x3C, 0x80)
        data[0x80:0x84] = b"PE\0\0"
        struct.pack_into("<H", data, 0x86, 1)
        optional_size = 240 if magic == 0x20B else 224
        struct.pack_into("<H", data, 0x94, optional_size)
        optional = 0x98
        struct.pack_into("<H", data, optional, magic)
        dirs = optional + (112 if magic == 0x20B else 96)
        struct.pack_into("<I", data, dirs - 4, 16)
        struct.pack_into("<II", data, dirs + 8, 0x1000, 60)
        struct.pack_into("<IIII", data, optional + optional_size + 8, 512, 0x1000, 512, 512)
        struct.pack_into("<I", data, 512 + 12, 0x1080)
        struct.pack_into("<I", data, 532 + 12, 0x1090)
        data[640:651] = b"nvcuda.dll\0"
        data[656:669] = b"KERNEL32.dll\0"
        return data

    def test_pe32_and_pe32_plus_imports(self):
        for magic in [0x10B, 0x20B]:
            with self.subTest(magic=magic), tempfile.TemporaryDirectory() as directory:
                path = Path(directory) / "fixture.exe"
                path.write_bytes(self.fixture(magic))
                self.assertEqual(pe.imports(path), ["kernel32.dll", "nvcuda.dll"])

    def test_rejects_non_pe(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "invalid"
            path.write_bytes(b"not an executable")
            with self.assertRaises(ValueError):
                pe.imports(path)


if __name__ == "__main__":
    unittest.main()
