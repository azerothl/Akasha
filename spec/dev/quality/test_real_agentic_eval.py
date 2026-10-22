import importlib.util
import json
import os
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch


MODULE_PATH = Path(__file__).with_name("real_agentic_eval.py")
SPEC = importlib.util.spec_from_file_location("real_agentic_eval", MODULE_PATH)
assert SPEC and SPEC.loader
eval_runner = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(eval_runner)


class RealAgenticEvalTests(unittest.TestCase):
    def test_llm_failure_diagnostics_are_categorical(self):
        self.assertEqual(
            eval_runner.classify_llm_failure("Provider rejected malformed tool-call JSON before execution"),
            "tool_call_json_parse",
        )
        self.assertEqual(eval_runner.classify_llm_failure("task timed out"), "timeout")

    def test_task_completion_status_aliases_are_supported(self):
        self.assertEqual(eval_runner.TASK_SUCCEEDED, {"done", "completed"})
        self.assertTrue(eval_runner.TASK_SUCCEEDED <= eval_runner.TERMINAL)

    def test_clean_env_removes_secrets_but_keeps_runtime_path(self):
        with patch.dict(os.environ, {"AKASHA_EVAL_TEST_API_KEY": "secret", "AKASHA_EVAL_TEST_TOKEN": "secret", "AKASHA_EVAL_TEST_VISIBLE": "yes"}):
            cleaned = eval_runner.clean_env()
        self.assertNotIn("AKASHA_EVAL_TEST_API_KEY", cleaned)
        self.assertNotIn("AKASHA_EVAL_TEST_TOKEN", cleaned)
        self.assertEqual(cleaned["AKASHA_EVAL_TEST_VISIBLE"], "yes")
        self.assertIn("PATH", cleaned)

    def test_tool_policy_scopes_file_access_and_disables_shell(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / "data").mkdir()
            eval_runner.write_eval_tools_policy(root / "data", root)
            policy = json.loads((root / "data" / "tools_policy.yaml").read_text(encoding="utf-8"))
        self.assertEqual(policy["allowed_read_paths"], [str(root)])
        self.assertEqual(policy["allowed_write_paths"], [str(root)])
        self.assertEqual(policy["allowed_commands"], [])
        tools = policy["tool_profiles"]["real_agentic_eval"]
        self.assertIn("memory_store", tools)
        self.assertIn("schedule_task", tools)
        self.assertNotIn("write_file", tools)
        self.assertNotIn("run_command", tools)
        self.assertNotIn("github_create_pr", tools)

    def test_policy_refusal_profile_allows_only_a_write_tool_and_path(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            data_dir = root / "data"
            allowed_dir = root / "allowed"
            data_dir.mkdir()
            allowed_dir.mkdir()
            eval_runner.write_eval_tools_policy(
                data_dir,
                root,
                allowed_tools=["write_file"],
                allowed_write_paths=[allowed_dir],
            )
            policy = json.loads((data_dir / "tools_policy.yaml").read_text(encoding="utf-8"))
        self.assertEqual(policy["tool_profiles"]["real_agentic_eval"], ["write_file"])
        self.assertEqual(policy["allowed_write_paths"], [str(allowed_dir)])
        self.assertEqual(policy["allowed_commands"], [])

    def test_scenario_report_keeps_tool_outcomes_without_result_previews(self):
        call = {
            "tools": ["write_file"],
            "tool_events": [
                {"tool": "write_file", "success": False, "result_preview": "path not allowed by policy"}
            ],
            "provider_model_deltas": [],
            "status": "done",
            "response_chars": 12,
            "latency_ms": 100,
            "model_used": None,
            "tokens_out": 0,
            "fallback_triggered": False,
            "fallback_success": False,
            "model_match": True,
            "provider_used": "ollama",
            "device_match": True,
            "model_vram_bytes": 1024,
            "llm_success_models": ["qwen2.5-coder:14b"],
            "llm_failure_kinds": ["context_window"],
        }
        result = eval_runner.scenario_result(
            "policy_refusal",
            [call],
            True,
            "denied",
            {"policy_denial_observed": True, "protected_file_unchanged": True},
        )
        self.assertEqual(result["tool_results"], [{"tool": "write_file", "success": False}])
        self.assertEqual(result["task_statuses"], ["done"])
        self.assertEqual(result["response_chars"], 12)
        self.assertEqual(result["llm_call_count"], 2)
        self.assertEqual(result["llm_success_count"], 1)
        self.assertEqual(result["llm_failure_kinds"], ["context_window"])
        self.assertEqual(result["llm_success_models"], ["qwen2.5-coder:14b"])
        self.assertEqual(
            result["checks"],
            {"policy_denial_observed": True, "protected_file_unchanged": True},
        )
        self.assertNotIn("result_preview", json.dumps(result))

    def test_fallback_uses_known_provider_and_unavailable_model(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            eval_runner.write_router(root, "qwen2.5-coder:14b", "http://127.0.0.1:11434", "cpu", unavailable_primary=True)
            config = json.loads((root / "llm_router.yaml").read_text(encoding="utf-8"))
        route = config["task_types"]["conversation"]
        self.assertEqual(route["primary"]["provider"], "ollama")
        self.assertEqual(route["primary"]["model"], "qwen2.5-coder:14b-akasha-eval-unavailable")
        self.assertEqual(route["fallback"][0]["model"], "qwen2.5-coder:14b")
        self.assertEqual(route["fallback"][0]["config"]["num_gpu"], 0)

    def test_router_allows_real_models_a_reasonable_tool_response_budget(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            eval_runner.write_router(root, "qwen3.5:9b", "http://127.0.0.1:11434", "cuda")
            config = json.loads((root / "llm_router.yaml").read_text(encoding="utf-8"))
        route = config["task_types"]["conversation"]["primary"]["config"]
        self.assertEqual(route["max_tokens"], 2048)
        self.assertEqual(route["num_gpu"], 99)

    def test_device_verification_uses_ollama_vram_evidence(self):
        with patch.object(eval_runner, "ollama_processes", return_value=[{"name": "qwen2.5-coder:14b", "size_vram_bytes": 0}]):
            self.assertEqual(eval_runner.model_device_match("http://ollama", "qwen2.5-coder:14b", "cpu"), (True, 0))
            self.assertEqual(eval_runner.model_device_match("http://ollama", "qwen2.5-coder:14b", "cuda"), (False, 0))
            self.assertEqual(eval_runner.model_device_match("http://ollama", "qwen2.5-coder:14b", "auto"), (None, 0))

    def test_comparison_rejects_unmeasured_baseline(self):
        baseline = {"maximum_scenario_latency_ms": 360000, "measured_baselines": {}}
        report = {
            "environment": {"model_requested": "qwen2.5-coder:14b", "execution_device_requested": "cpu"},
            "no_go": [],
            "passed": 5,
            "total": 5,
            "scenarios": [],
        }
        args = type("Args", (), {"model": "qwen2.5-coder:14b", "device": "cpu", "record_baseline": False})()
        eval_runner.compare_or_record_baseline(report, baseline, args)
        self.assertIn("measured_baseline_missing", report["no_go"])
        self.assertEqual(report["baseline_comparison"]["status"], "missing")

    def test_comparison_accepts_same_hardware_within_latency_budget(self):
        environment = {
            "model_requested": "qwen2.5-coder:14b",
            "execution_device_requested": "cpu",
            "os": "Windows test",
            "architecture": "AMD64",
            "cpu": "Test CPU",
            "cpu_logical_cores": 16,
            "gpu_devices": ["GPU test, 16 GB"],
        }
        scenario = {"name": "tool_choice_execution", "passed": True, "latency_ms": 18000}
        baseline = {
            "maximum_scenario_latency_ms": 360000,
            "latency_regression_factor": 2.0,
            "latency_regression_slack_ms": 5000,
            "measured_baselines": {
                "ollama::qwen2.5-coder:14b@cpu": {
                    "source_commit": "abc123",
                    "hardware": eval_runner.hardware_fingerprint(environment, "cpu"),
                    "scenarios": {"tool_choice_execution": {"passed": True, "latency_ms": 12000}},
                }
            },
        }
        report = {"environment": environment, "no_go": [], "passed": 1, "total": 1, "scenarios": [scenario]}
        args = type("Args", (), {"model": "qwen2.5-coder:14b", "device": "cpu", "record_baseline": False})()
        eval_runner.compare_or_record_baseline(report, baseline, args)
        self.assertEqual(report["baseline_comparison"]["status"], "matched")
        self.assertFalse(report["no_go"])


if __name__ == "__main__":
    unittest.main()
