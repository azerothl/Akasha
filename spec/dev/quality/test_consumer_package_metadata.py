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

    def test_missing_or_duplicate_records_cannot_identify_a_candidate(self):
        with tempfile.TemporaryDirectory() as directory:
            path=Path(directory)/"image"
            for data in [b"older image",self.record()+b"\0"+self.record(False)]:
                path.write_bytes(data)
                with self.assertRaises(ValueError):package.binary_build_info(path)

    def test_truncated_metadata_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            path=Path(directory)/"image";path.write_bytes(self.record()[:-4])
            with self.assertRaises(ValueError):package.binary_build_info(path)

if __name__=="__main__":unittest.main()
