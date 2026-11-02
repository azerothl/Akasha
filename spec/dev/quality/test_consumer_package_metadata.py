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
    def test_source_provenance_requires_clean_matching_revisions(self):
        clean={"source_revision":"main-revision","source_dirty":False}
        package.validate_source_provenance(clean,"main-revision",False)
        for build,revision,dirty in [
            ({**clean,"source_dirty":True},"main-revision",False),
            (clean,"main-revision",True),
            (clean,"different-revision",False),
            ({"source_revision":"main-revision"},"main-revision",False),
        ]:
            with self.subTest(build=build,revision=revision,dirty=dirty):
                with self.assertRaises(ValueError):
                    package.validate_source_provenance(build,revision,dirty)

    def test_cuda_installer_is_ignored_but_source_changes_are_visible(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            package.subprocess.run(["git","init",str(root)],check=True,capture_output=True)
            (root/".gitignore").write_bytes((path.parents[2]/".gitignore").read_bytes())
            (root/"cuda_download").mkdir()
            installer=root/"cuda_download/vendor-installer.exe"
            installer.write_bytes(b"synthetic-vendor-installer")
            result=package.subprocess.run(["git","check-ignore",str(installer)],cwd=root,capture_output=True)
            self.assertEqual(result.returncode,0)
            source=root/"source.rs";source.write_text("changed source")
            result=package.subprocess.run(["git","check-ignore",str(source)],cwd=root,capture_output=True)
            self.assertEqual(result.returncode,1)

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
