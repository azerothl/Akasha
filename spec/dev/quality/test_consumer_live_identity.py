import copy
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock, patch

import consumer_beta_run as driver
from consumer_live_identity import LiveIdentityGuard, local_identity


class LiveIdentityTests(unittest.TestCase):
    def setUp(self):
        self.folder = tempfile.TemporaryDirectory()
        self.addCleanup(self.folder.cleanup)
        self.root = Path(self.folder.name)
        self.binary = self.root / "daemon.exe"
        self.binary.write_bytes(b"synthetic binary")
        self.identity = {"provider": "ollama", "model": "synthetic:1",
                         "model_digest": "a" * 64, "model_bytes": 42,
                         "engine": "ollama:synthetic", "binary_sha256":
                         hashlib.sha256(self.binary.read_bytes()).hexdigest()}
        self.probe = {"identity": {**self.identity, "configuration_sha256": "config"},
                      "identity_changed": False, "probe_suite_passed": True}
        self.probe["identity_after"] = copy.deepcopy(self.probe["identity"])

    def guard(self, snapshots=None):
        return LiveIdentityGuard(self.binary, "synthetic:1", self.root / "identities.json",
                                 snapshots or (lambda: copy.deepcopy(self.identity)))

    def test_local_snapshot_reads_digest_and_engine_without_running_inference(self):
        tags = {"models": [{"name": "synthetic:1", "digest": "a" * 64, "size": 42}]}
        with patch("consumer_live_identity.http_json", side_effect=[tags, {"version": "synthetic"}]) as api:
            self.assertEqual(local_identity(self.binary, "synthetic:1"), self.identity)
        self.assertEqual([call.args[2] for call in api.call_args_list], ["/api/tags", "/api/version"])

    def test_absent_digest_missing_engine_and_remote_alias_are_rejected(self):
        entry = {"name": "synthetic:1", "digest": "a" * 64, "size": 42}
        for tag in (None, {**entry, "digest": ""}, {**entry, "remote_host": "remote.example"},
                    {**entry, "remote_model": "remote-model"}):
            with self.subTest(tag=tag), patch("consumer_live_identity.http_json", return_value={"models": [tag] if tag else []}):
                with self.assertRaises(ValueError):
                    local_identity(self.binary, "synthetic:1")
        with patch("consumer_live_identity.http_json", side_effect=[{"models": [entry]}, {}]):
            with self.assertRaisesRegex(ValueError, "engine version"):
                local_identity(self.binary, "synthetic:1")

    def test_each_live_workflow_and_final_probe_keep_before_after_receipts(self):
        guard = self.guard()
        execute = Mock(return_value=("", {"exit_code": 0}))
        for phase in ("chat-live", "documents", "organization", "model-probe"):
            guard.workflow(phase, ["synthetic-command"], execute)
        self.assertEqual(guard.verify_probe(self.probe, {"exit_code": 0}), self.probe["identity"])
        receipt = json.loads(guard.output.read_text(encoding="utf-8"))
        self.assertTrue(receipt["verified"])
        self.assertEqual([row["phase"] for row in receipt["observations"]],
                         [phase + ":" + boundary for phase in
                          ("chat-live", "documents", "organization", "model-probe")
                          for boundary in ("before", "after")])
        self.assertEqual(execute.call_count, 4)

    def test_digest_engine_and_binary_drift_during_a_workflow_are_rejected(self):
        for field in ("model_digest", "engine", "binary_sha256"):
            with self.subTest(field=field):
                changed = {**self.identity, field: "changed"}
                guard = self.guard(Mock(side_effect=[self.identity, changed]))
                with self.assertRaisesRegex(ValueError, "changed at documents:after"):
                    guard.workflow("documents", [], Mock(return_value=("", {"exit_code": 0})))
                receipt = json.loads(guard.output.read_text(encoding="utf-8"))
                self.assertFalse(receipt["verified"])
                self.assertEqual(receipt["observations"][-1]["identity"], changed)

    def test_drift_between_workflows_prevents_the_next_workflow(self):
        guard = self.guard(Mock(side_effect=[self.identity, self.identity,
                                            {**self.identity, "model_digest": "changed"}]))
        execute = Mock(return_value=("", {"exit_code": 0}))
        guard.workflow("chat-live", [], execute)
        with self.assertRaisesRegex(ValueError, "changed at documents:before"):
            guard.workflow("documents", [], execute)
        self.assertEqual(execute.call_count, 1)

    def test_failed_workflow_still_records_the_after_identity(self):
        guard = self.guard()
        with self.assertRaisesRegex(TimeoutError, "synthetic timeout"):
            guard.workflow("documents", [], Mock(side_effect=TimeoutError("synthetic timeout")))
        self.assertEqual([row["phase"] for row in guard.observations],
                         ["documents:before", "documents:after"])
        self.assertFalse(json.loads(guard.output.read_text())["verified"])

    def test_unavailable_identity_fails_closed_and_retains_the_error(self):
        guard = self.guard(Mock(side_effect=RuntimeError("engine unavailable")))
        execute = Mock()
        with self.assertRaisesRegex(ValueError, "Cannot verify live model identity"):
            guard.workflow("documents", [], execute)
        execute.assert_not_called()
        self.assertIn("engine unavailable", json.loads(guard.output.read_text())["error"])

    def test_probe_drift_flags_missing_fields_failed_process_and_configuration_drift_reject(self):
        invalid = [({**self.probe, "identity_changed": True}, 0),
                   ({key: value for key, value in self.probe.items() if key != "identity_changed"}, 0),
                   ({**self.probe, "identity_after": None}, 0),
                   ({**self.probe, "identity_after": {**self.probe["identity"], "configuration_sha256": "changed"}}, 0),
                   ({**self.probe, "probe_suite_passed": False}, 0), (self.probe, 1)]
        for probe, code in invalid:
            with self.subTest(probe=probe, code=code):
                guard = self.guard()
                guard.capture("chat-live:before")
                with self.assertRaisesRegex(ValueError, "probe failed or reported identity drift"):
                    guard.verify_probe(probe, {"exit_code": code})
                self.assertFalse(json.loads(guard.output.read_text())["verified"])

    def test_probe_cannot_relabel_earlier_workflows_with_a_later_identity(self):
        guard = self.guard()
        guard.capture("chat-live:before")
        for field in ("model_digest", "engine", "binary_sha256"):
            changed = {**self.probe["identity"], field: "changed"}
            with self.subTest(field=field), self.assertRaisesRegex(ValueError, "differs from the live workflows"):
                guard.verify_probe({**self.probe, "identity": changed, "identity_after": changed}, {"exit_code": 0})

    def test_failed_repeated_run_cannot_reuse_old_reports_or_qualification(self):
        (self.root / "runtime-lock.json").write_text("{}")
        (self.root / "qualification.json").write_text('{"qualified":true}')
        run = self.root / "run-1"
        run.mkdir()
        (run / "report.json").write_text('{"run_id":"stale"}')
        argv = ["driver", "--binary", str(self.binary), "--runtime", str(self.root),
                "--output", str(self.root), "--repeat", "3", "--live"]
        with patch("sys.argv", argv), patch.dict(driver.os.environ), patch.object(driver.subprocess, "run", return_value=Mock(returncode=1)) as process:
            self.assertEqual(driver.main(), 1)
        self.assertEqual(process.call_count, 1)
        self.assertFalse((self.root / "qualification.json").exists())

    def test_driver_rejects_drift_before_assembling_a_report_and_removes_stale_results(self):
        (self.root / "runtime-lock.json").write_text("{}")
        for name in ("report.json", "evidence.json"):
            (self.root / name).write_text('{"passed_gate":true}')
        invalid_probe = {**self.probe, "identity_changed": True}

        def process(command, **kwargs):
            if "--output" in command:
                output = Path(command[command.index("--output") + 1])
                payload = invalid_probe if output.name == "model-probe.json" else {"results": [], "cases": []}
                output.write_text(json.dumps(payload), encoding="utf-8")
            return Mock(returncode=0, stdout=b"")

        argv = ["driver", "--binary", str(self.binary), "--runtime", str(self.root),
                "--output", str(self.root), "--live", "--model", "synthetic:1"]
        with patch("sys.argv", argv), patch.dict(driver.os.environ), \
                patch.object(driver.subprocess, "check_output", return_value="synthetic-revision"), \
                patch.object(driver.subprocess, "run", side_effect=process), \
                patch("consumer_live_identity.local_identity", return_value=self.identity), \
                patch.object(driver.gate, "report") as assemble:
            with self.assertRaisesRegex(ValueError, "probe failed or reported identity drift"):
                driver.main()
        assemble.assert_not_called()
        self.assertFalse((self.root / "report.json").exists())
        self.assertFalse((self.root / "evidence.json").exists())
        receipt = json.loads((self.root / "identity-checks.json").read_text())
        self.assertEqual(len(receipt["observations"]), 8)
        self.assertFalse(receipt["verified"])


if __name__ == "__main__":
    unittest.main()
