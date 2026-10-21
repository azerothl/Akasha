"""Write build/cache measurements to JSON and the GitHub job summary."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import time


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("phase", choices=["start", "finish"])
    parser.add_argument("--label", default=os.environ.get("GITHUB_JOB", "local"))
    parser.add_argument("--cache-hit", default="")
    parser.add_argument("--target-dir", default="target")
    parser.add_argument("--features", default="")
    args = parser.parse_args()
    path = Path("ci-build-report.json")
    if args.phase == "start":
        path.write_text(json.dumps({"started_at_unix": time.time(), "label": args.label}), encoding="utf-8")
        return
    report = json.loads(path.read_text(encoding="utf-8"))
    report.update(
        elapsed_seconds=round(time.time() - report["started_at_unix"], 3),
        commit=os.environ.get("GITHUB_SHA", ""),
        cache_hit=args.cache_hit,
        runner_os=os.environ.get("RUNNER_OS", os.name),
        runner_arch=os.environ.get("RUNNER_ARCH", ""),
        image_version=os.environ.get("ImageVersion", ""),
        features=args.features,
        job_status=os.environ.get("BUILD_JOB_STATUS", "unknown"),
        target_bytes=sum(p.stat().st_size for p in Path(args.target_dir).rglob("*") if p.is_file()),
    )
    try:
        result = subprocess.run(["rustc", "-Vv"], capture_output=True, text=True)
        report["rustc"] = result.stdout.strip()
    except OSError as error:
        report["rustc"] = f"unavailable: {error}"
    path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    if summary := os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(summary, "a", encoding="utf-8") as out:
            out.write(f"\n### Build measurement: {args.label}\n\n")
            out.write(f"- Exact dependency cache hit: {args.cache_hit or 'unavailable'}\n")
            out.write(f"- Elapsed (setup, restore and build): {report['elapsed_seconds']} s\n")
            out.write(f"- Target before cache pruning: {report['target_bytes']} bytes\n")
            out.write(f"- Status: {report['job_status']}\n")


if __name__ == "__main__":
    main()
