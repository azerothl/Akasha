import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

path = Path(__file__).resolve().parents[3] / "scripts/ci/qualify_consumer_candidate.py"
spec = importlib.util.spec_from_file_location("candidate_gate", path)
candidate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(candidate)


class CandidateGateTests(unittest.TestCase):
    def test_build_without_physical_evidence_never_opens_pilot(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            result = candidate.evaluate([root], root)
            self.assertFalse(result["ready_for_pilot"])
            self.assertTrue(result["errors"])

    def test_dirty_or_tampered_package_cannot_reuse_evidence(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "acceptance.json").write_text('{"schema_version":1,"packages":[]}', encoding="utf-8")
            manifest = {"schema_version":1,"edition":"consumer-beta","source_dirty":True}
            (root / "consumer-package.json").write_text(json.dumps(manifest), encoding="utf-8")
            self.assertFalse(candidate.evaluate([root], root)["ready_for_pilot"])
            manifest.update(source_dirty=False,packaging_source_dirty=False,variant="cpu",source_revision="synthetic",files=[{"path":"component.exe","sha256":"0"*64}])
            (root / "component.exe").write_bytes(b"altered synthetic fixture")
            (root / "consumer-package.json").write_text(json.dumps(manifest), encoding="utf-8")
            result = candidate.evaluate([root], root)
            self.assertFalse(result["ready_for_pilot"])
            self.assertTrue(any("Altered package component" in error for error in result["errors"]))

    def test_evidence_paths_cannot_escape_the_bundle(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaises(ValueError):
                candidate.contained(Path(directory), "../other-account.json")


if __name__ == "__main__":
    unittest.main()
