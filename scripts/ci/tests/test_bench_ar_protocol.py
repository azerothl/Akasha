from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "spec" / "dev" / "quality"))

import bench_ar_protocol as bench  # noqa: E402


PROTOCOL = {
    "id": "ar_test_v1",
    "provider": "akasha_embedded",
    "decision_gate": {},
    "prompts": [
        {"id": "one", "text": "Réponds en une phrase."},
        {"id": "two", "text": "Donne un exemple."},
    ],
    "candidates": [
        {"id": "qwen-test", "arch": "qwen", "tier": "tier1"},
        {"id": "other", "arch": "llama", "tier": "tier2"},
    ],
}


def response(text="réponse valide", model="default", tokens=12, provider="akasha_embedded"):
    return {
        "text": text,
        "model_used": model,
        "provider_used": provider,
        "eval_count": tokens,
        "usage": {"prompt_tokens": 5, "completion_tokens": tokens} if tokens is not None else None,
        "latency_ms": 400,
        "ttft_ns": 50_000_000,
        "load_duration_ns": 100_000_000,
        "generation_duration_ns": 400_000_000,
    }


class BenchArProtocolTests(unittest.TestCase):
    def setUp(self):
        self.http = patch.object(bench, "http_json")
        self.http_json = self.http.start()
        self.addCleanup(self.http.stop)
        self.http_json.side_effect = [
            {"schema_version": 1, "runtime": {"gguf": True}},
            {
                "embedded_available": True,
                "backend": "llama_cpp",
                "device": "cuda",
                "model_path": None,
                "active_model_id": "qwen-test",
            },
            response(),
            {"embedded_available": True, "backend": "llama_cpp", "device": "cuda", "active_model_id": "qwen-test"},
            response(),
            {"embedded_available": True, "backend": "llama_cpp", "device": "cuda", "active_model_id": "qwen-test"},
            {"embedded_available": True, "backend": "llama_cpp", "device": "cuda", "active_model_id": "qwen-test"},
        ]
        self.gpu = patch.object(bench, "_gpu_devices", return_value=["NVIDIA Test GPU, 24 GiB"])
        self.gpu.start()
        self.addCleanup(self.gpu.stop)

    def test_live_uses_completion_endpoint_and_real_usage_counts(self):
        result = bench.live_run(PROTOCOL, "http://127.0.0.1:3876", "qwen-test", 32, "qwen-test")

        self.assertEqual(result["totals"], {"rows": 2, "ok": 2, "errors": 0, "expected_rows": 2})
        self.assertEqual(result["environment"]["backend"], "llama_cpp")
        self.assertEqual(result["environment"]["device"], "cuda")
        self.assertIsInstance(result["source_tree_dirty"], bool)
        self.assertIsNone(result["environment"]["gpu_memory_samples"])
        for row in result["results"]:
            self.assertEqual(row["completion_tokens"], 12)
            self.assertEqual(row["tok_per_s"], 30.0)
            self.assertEqual(row["provider_used"], "akasha_embedded")
            self.assertEqual(row["model_used"], "default")
            self.assertEqual(row["runtime_model"], "qwen-test")
            self.assertEqual(row["ttft_s"], 0.05)
            self.assertEqual(row["load_s"], 0.1)
            self.assertEqual(row["token_count_source"], "eval_count")
            self.assertEqual(row["actions"], [])
            self.assertEqual(row["tools"], [])
        posts = [call.args for call in self.http_json.call_args_list if call.args[0] == "POST"]
        self.assertEqual(len(posts), 2)
        self.assertTrue(all(call[1].endswith("/api/complete") for call in posts))
        self.assertTrue(all(call[2]["max_tokens"] == 32 for call in posts))

    def test_gpu_sample_summary_reports_observed_vram_and_utilization(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "gpu-samples.csv"
            path.write_text(
                "2026/10/20 13:24:19.781, 2061 MiB, 13 %\n"
                "2026/10/20 13:24:24.498, 3661 MiB, 53 %\n",
                encoding="utf-8",
            )

            self.assertEqual(
                bench._gpu_sample_summary(path),
                {
                    "source": "gpu-samples.csv",
                    "sample_count": 2,
                    "memory_used_min_mib": 2061,
                    "memory_used_max_mib": 3661,
                    "utilization_max_pct": 53,
                },
            )

    def test_live_requires_one_candidate_and_an_expected_model(self):
        with self.assertRaisesRegex(SystemExit, "requires --candidate"):
            bench.live_run(PROTOCOL, "http://localhost", None, 32, "qwen-test")
        with self.assertRaisesRegex(SystemExit, "requires --expected-model"):
            bench.live_run(PROTOCOL, "http://localhost", "qwen-test", 32, None)
        with self.assertRaisesRegex(SystemExit, "unknown candidate"):
            bench.live_run(PROTOCOL, "http://localhost", "missing", 32, "qwen-test")

    def test_wrong_routed_model_is_a_failed_row(self):
        self.http_json.side_effect = [
            {"schema_version": 1, "runtime": {}},
            {"embedded_available": True, "model_path": None},
            response(model="ollama:tinyllama"),
            {"embedded_available": True, "active_model_id": "qwen-test"},
            response(model="ollama:tinyllama"),
            {"embedded_available": True, "active_model_id": "qwen-test"},
            {"embedded_available": True, "active_model_id": "qwen-test"},
        ]

        result = bench.live_run(PROTOCOL, "http://127.0.0.1:3876", "qwen-test", 32, "qwen-test")

        self.assertEqual(result["totals"]["ok"], 0)
        self.assertTrue(all("does not match expected model" in row["error"] for row in result["results"]))

    def test_wrong_routed_provider_is_a_failed_row(self):
        self.http_json.side_effect = [
            {"schema_version": 1, "runtime": {}},
            {"embedded_available": True, "model_path": None},
            response(provider="ollama"),
            {"embedded_available": True, "active_model_id": "qwen-test"},
            response(provider="ollama"),
            {"embedded_available": True, "active_model_id": "qwen-test"},
            {"embedded_available": True, "active_model_id": "qwen-test"},
        ]

        result = bench.live_run(PROTOCOL, "http://127.0.0.1:3876", "qwen-test", 32, "qwen-test")

        self.assertEqual(result["totals"]["ok"], 0)
        self.assertTrue(all("does not match expected provider" in row["error"] for row in result["results"]))

    def test_empty_response_and_missing_usage_are_failures(self):
        self.http_json.side_effect = [
            {"schema_version": 1, "runtime": {}},
            {"embedded_available": True, "model_path": None},
            response(text=""),
            {"embedded_available": True, "active_model_id": "qwen-test"},
            response(tokens=None),
            {"embedded_available": True, "active_model_id": "qwen-test"},
            {"embedded_available": True, "active_model_id": "qwen-test"},
        ]

        result = bench.live_run(PROTOCOL, "http://127.0.0.1:3876", "qwen-test", 32, "qwen-test")

        self.assertEqual(result["totals"]["ok"], 0)
        self.assertIn("text is empty", result["results"][0]["error"])
        self.assertIn("token count is unavailable", result["results"][1]["error"])
        self.assertIsNone(result["results"][1]["tok_per_s"])


if __name__ == "__main__":
    unittest.main()
