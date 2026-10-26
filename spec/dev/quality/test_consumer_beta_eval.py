import copy
import unittest

import consumer_beta_eval as gate


class AcceptanceTests(unittest.TestCase):
    def test_corpus_distribution_and_unique_ids(self):
        self.assertEqual(len(gate.load_corpus()["cases"]), 50)

    def test_missing_evidence_cannot_pass(self):
        result = gate.report(gate.load_corpus(), {})
        self.assertEqual(result["counts"], {"not_run": 50})
        self.assertFalse(result["passed_gate"])

    def test_prose_and_completed_status_do_not_prove_an_action(self):
        case = {"id": "write", "expect": "result", "checks": ["no_false_success", "actual_write"]}
        row = gate.assess(case, {"observed": "result", "response": "Fichier créé", "status": "completed", "checks": {"no_false_success": False}})
        self.assertEqual(row["classification"], "false_success")
        self.assertFalse(row["passed"])

    def test_expected_refusal_is_distinct_from_a_completed_action(self):
        case = {"id": "denied", "expect": "refusal", "checks": ["no_false_success", "privacy"]}
        evidence = {"observed": "refusal", "checks": {"no_false_success": True, "privacy": True}}
        self.assertEqual(gate.assess(case, evidence)["classification"], "expected_refusal")
        case["expect"] = "result"
        self.assertFalse(gate.assess(case, evidence)["passed"])

    def test_three_same_real_runs_are_required(self):
        corpus = gate.load_corpus()
        evidence = {"mode":"live", "identity":dict.fromkeys(("binary_sha256", "source_revision", "model", "model_digest", "engine", "configuration_sha256"), "synthetic"), "cases":{case["id"]:{"observed":case["expect"],"checks":dict.fromkeys(case["checks"],True)} for case in corpus["cases"]}}
        record = gate.report(corpus, evidence)
        records = [{**copy.deepcopy(record), "run_id": str(i),"series_id":"synthetic-series","sequence_number":i+1,"previous_run_id":str(i-1) if i else None} for i in range(3)]
        self.assertTrue(gate.qualify(records))
        self.assertFalse(gate.qualify([record] * 3))
        self.assertFalse(gate.qualify([record] * 2))
        changed = copy.deepcopy(record)
        changed["identity"]["model_digest"] = "changed"
        self.assertFalse(gate.qualify([record, record, changed]))

    def test_partial_observation_is_not_an_executed_failure(self):
        corpus = gate.load_corpus()
        cases = {case["id"]:{"observed":case["expect"],"checks":dict.fromkeys(case["checks"],True)} for case in corpus["cases"]}
        cases[corpus["cases"][0]["id"]] = {"checks":{"no_false_success":True}}
        result = gate.report(corpus, {"mode":"live","identity":dict.fromkeys(("binary_sha256", "source_revision", "model", "model_digest", "engine", "configuration_sha256"),"synthetic"),"cases":cases})
        self.assertFalse(result["complete"])
        self.assertFalse(result["passed_gate"])
        self.assertEqual(result["counts"]["not_run"], 1)

    def test_cached_flags_and_stale_contract_never_qualify(self):
        records = [{"mode":"live","run_id":str(i),"series_id":"fake","sequence_number":i+1,"previous_run_id":str(i-1) if i else None,"qualifies_this_run":True} for i in range(3)]
        self.assertFalse(gate.qualify(records))
        corpus = gate.load_corpus()
        evidence = {"mode":"live","identity":dict.fromkeys(("binary_sha256", "source_revision", "model", "model_digest", "engine", "configuration_sha256"),"synthetic"),"cases":{case["id"]:{"observed":case["expect"],"checks":dict.fromkeys(case["checks"],True)} for case in corpus["cases"]}}
        complete = [{**gate.report(corpus,evidence),**record} for record in records]
        self.assertTrue(gate.qualify(complete))
        for row in complete: row["corpus_sha256"] = "obsolete"
        self.assertFalse(gate.qualify(complete))


if __name__ == "__main__":
    unittest.main()
