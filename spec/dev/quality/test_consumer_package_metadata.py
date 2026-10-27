import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

path=Path(__file__).resolve().parents[3]/"scripts/ci/package_consumer.py"
spec=importlib.util.spec_from_file_location("consumer_package",path)
package=importlib.util.module_from_spec(spec)
spec.loader.exec_module(package)

class BuildMetadataTests(unittest.TestCase):
    def record(self, cuda=True):
        return b"AKASHA_CONSUMER_INFO_V1:"+json.dumps({"consumer_beta":True,"cuda":cuda,"source_revision":"synthetic"}).encode()+b":END_AKASHA_CONSUMER_INFO_V1"

    def test_cuda_metadata_is_read_without_executing_the_target(self):
        with tempfile.TemporaryDirectory() as directory:
            path=Path(directory)/"not-executable.exe"
            path.write_bytes(b"synthetic-image\0"+self.record()+b"\0")
            with patch.object(package.subprocess,"check_output",side_effect=AssertionError("Target must never execute")):
                self.assertTrue(package.binary_build_info(path)["cuda"])

    def test_identical_release_records_have_one_identity(self):
        with tempfile.TemporaryDirectory() as directory:
            path=Path(directory)/"optimized-image.exe"
            record=self.record()
            path.write_bytes(b"section1\0"+record+b"\0section2\0"+record+b"\0section3\0"+record)
            with patch.object(package.subprocess,"check_output",side_effect=AssertionError("Target must never execute")):
                self.assertEqual(package.binary_build_info(path),json.loads(record.split(b':',1)[1].removesuffix(b':END_AKASHA_CONSUMER_INFO_V1')))

    def test_missing_or_conflicting_records_cannot_identify_a_candidate(self):
        with tempfile.TemporaryDirectory() as directory:
            path=Path(directory)/"image"
            for data in [b"older image",self.record()+b"\0"+self.record(False)]:
                path.write_bytes(data)
                with self.assertRaises(ValueError):package.binary_build_info(path)

    def test_truncated_metadata_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            path=Path(directory)/"image";path.write_bytes(self.record()[:-4])
            with self.assertRaises(ValueError):package.binary_build_info(path)

    def test_same_variant_with_different_source_identity_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            path=Path(directory)/"image"
            path.write_bytes(self.record()+b"\0"+self.record().replace(b'synthetic',b'another-source'))
            with self.assertRaises(ValueError):package.binary_build_info(path)

if __name__=="__main__":unittest.main()
