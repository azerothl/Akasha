"""Read-only publication gate: both packages, real runs and operator evidence.

This command never publishes, modifies a package or starts a pilot. Physical
checks are signed off by an operator and linked to hashed evidence files; a CI
build or a model's statement cannot replace those checks.
"""
import argparse
import hashlib
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "spec/dev/quality"))
import consumer_beta_eval as gate


def require(condition, message):
    if not condition:
        raise ValueError(message)


def digest(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def contained(root, relative):
    path = (root / relative).resolve()
    if not path.is_relative_to(root.resolve()):
        raise ValueError("Evidence or component path escapes its directory")
    return path


def evaluate(packages, evidence_root):
    errors = []
    try:
        bundle = json.loads((evidence_root / "acceptance.json").read_text(encoding="utf-8"))
        require(bundle["schema_version"] == 1, "Incomplete acceptance evidence")
    except (OSError, ValueError, KeyError, AssertionError) as error:
        return {"ready_for_pilot": False, "errors": ["Operator evidence bundle unavailable: " + str(error)]}
    variants = set()
    revisions = set()
    for package in packages:
        try:
            manifest_path = package / "consumer-package.json"
            manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
            require(manifest["schema_version"] == 1 and manifest["edition"] == "consumer-beta", "Incomplete acceptance evidence")
            require(manifest["source_dirty"] is False, "A clean committed build is required")
            variant = manifest["variant"]
            require(variant in {"cpu", "nvidia"} and variant not in variants, "Duplicate or invalid package variant")
            variants.add(variant)
            revisions.add(manifest["source_revision"])
            for component in manifest["files"]:
                require(digest(contained(package, component["path"])) == component["sha256"], "Altered package component")
            binary_sha = next(row["sha256"] for row in manifest["files"] if row["path"] == "akasha-daemon.exe")
            proof = next(row for row in bundle["packages"] if row["variant"] == variant)
            require(proof["manifest_sha256"] == digest(manifest_path), "Evidence belongs to another package")
            deterministic = json.loads(contained(evidence_root, proof["deterministic_report"]).read_text(encoding="utf-8"))
            require(deterministic["mode"] == "deterministic" and deterministic["passed_gate"] is True and deterministic["passed"] == 50, "Incomplete acceptance evidence")
            reports = [json.loads(contained(evidence_root, name).read_text(encoding="utf-8")) for name in proof["live_reports"]]
            # Recompute every report from its independent observations. Claimed
            # summary booleans alone cannot authorize publication.
            corpus = gate.load_corpus()
            verified_reports = []
            for report in [deterministic, *reports]:
                require(report["identity"]["binary_sha256"] == binary_sha, "Incomplete acceptance evidence")
                require(report["identity"]["source_revision"] == manifest["source_revision"], "Incomplete acceptance evidence")
                observed = {row["id"]: row.get("evidence") for row in report["cases"]}
                recomputed = gate.report(corpus, {**report, "cases": observed})
                require(recomputed["passed_gate"] is True and recomputed["corpus_file_sha256"] == report["corpus_file_sha256"], "Incomplete acceptance evidence")
                verified_reports.append(recomputed)
            reports = verified_reports[1:]
            require(gate.qualify(reports), "Three consecutive real runs of the same configuration are required")
            for category in ["conversation", "documents", "organization"]:
                ids = {case["id"] for case in corpus["cases"] if case["category"] == category}
                require(all(sum(row["passed"] for row in report["cases"] if row["id"] in ids) / len(ids) >= .95 for report in reports), "A usage misses its own 95% threshold")
            physical = proof["physical_checks"]
            for kind, checks in {
                "windows_hardware": ["fresh_install", "first_document_result", "offline_start", "update_preserves_data", "cancellation", "consent_revocation"],
                "second_windows_account": ["restore_validated", "identifiers_preserved", "services_disconnected", "wrong_password_rejected", "tampered_archive_rejected"],
            }.items():
                record = physical[kind]
                require(record["operator"] and record["performed_at"] and record["windows_build"], "Incomplete acceptance evidence")
                require(record["manifest_sha256"] == digest(manifest_path), "Incomplete acceptance evidence")
                require(all(record["checks"].get(check) is True for check in checks), "Incomplete physical acceptance")
                receipts = record["receipts"]
                require(receipts, "Physical checks need retained evidence")
                for receipt in receipts:
                    path = contained(evidence_root, receipt["path"])
                    require(path.stat().st_size > 0 and digest(path) == receipt["sha256"], "Incomplete acceptance evidence")
            hardware = physical["windows_hardware"]["hardware"]
            require(hardware["cpu"] and hardware["ram_gb"] > 0, "Incomplete acceptance evidence")
            require((variant == "cpu" and hardware["gpu"] is None) or (variant == "nvidia" and "nvidia" in hardware["gpu"].lower()), "Use the requested physical hardware")
            video = contained(evidence_root, proof["document_video"]["path"])
            require(video.stat().st_size > 0 and digest(video) == proof["document_video"]["sha256"], "Incomplete acceptance evidence")
        except (OSError, ValueError, KeyError, AssertionError, StopIteration, TypeError) as error:
            errors.append(package.name + ": " + (str(error) or "Incomplete acceptance evidence"))
    if variants != {"cpu", "nvidia"}:
        errors.append("Both CPU-only Windows and NVIDIA Windows candidates are required")
    if len(revisions) != 1:
        errors.append("Both packages must use the same committed source revision")
    return {"schema_version": 1, "ready_for_pilot": not errors, "errors": errors,
            "operator_attestation_required": True, "publication_performed": False}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package", type=Path, action="append", required=True)
    parser.add_argument("--evidence-root", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    result = evaluate(args.package, args.evidence_root)
    args.output.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(result))
    raise SystemExit(0 if result["ready_for_pilot"] else 1)
