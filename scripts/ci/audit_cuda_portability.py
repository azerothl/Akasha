"""Audit a Windows CUDA staging folder as a possible CPU-only distribution.

A direct nvcuda.dll import is a load-time requirement, before Rust chooses any
backend or opens a GGUF. An HTTP start alone never qualifies as inference proof.
"""
import argparse
import ctypes
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time

from pe_imports import imports


def audit(directory, try_start=False):
    daemon = directory / "akasha-daemon.exe"
    dlls = imports(daemon)
    driver = Path(os.environ.get("WINDIR", "C:/Windows")) / "System32/nvcuda.dll"
    direct_driver = "nvcuda.dll" in dlls
    report = dict(
        daemon_sha256=hashlib.sha256(daemon.read_bytes()).hexdigest(),
        load_time_dlls=dlls, direct_driver_import=direct_driver,
        nvidia_driver_dll_present=driver.is_file(),
        directory_bytes=sum(p.stat().st_size for p in directory.rglob("*") if p.is_file()),
        verdict="NO_GO" if direct_driver else "INCONCLUSIVE",
        reason=("Windows requires nvcuda.dll before backend/GGUF selection"
                if direct_driver else "Loader/inference compatibility still needs validation"),
        inference_validated=False,
        gguf_present_inference="not reached/validated",
        gguf_absent_inference="not reached/validated",
        startup=None,
        startup_skipped_reason=("host has NVIDIA driver" if driver.is_file()
                                else "startup not requested on native Windows"),
    )
    if try_start and os.name == "nt" and not driver.is_file():
        ctypes.windll.kernel32.SetErrorMode(0x0001 | 0x0002 | 0x8000)
        with tempfile.TemporaryDirectory(prefix="akasha-cuda-portability-") as data:
            env = dict(os.environ, AKASHA_DATA_DIR=data, AKASHA_PORT="0",
                       AKASHA_EMBEDDED_BACKEND="candle",
                       OLLAMA_HOST="http://127.0.0.1:11434")
            try:
                process = subprocess.Popen(
                    [str(daemon.resolve())], cwd=directory.resolve(), env=env,
                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                    creationflags=subprocess.CREATE_NO_WINDOW)
            except OSError as error:
                report["startup"] = dict(loader_error=str(error), winerror=error.winerror)
                report["startup_skipped_reason"] = None
                return report
            report["startup_skipped_reason"] = None
            started = time.monotonic()
            try:
                code = process.wait(timeout=10)
                report["startup"] = dict(exit_code=code, hex_exit=f"0x{code & 0xFFFFFFFF:08X}",
                                         elapsed_seconds=round(time.monotonic() - started, 3))
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
                report["startup"] = dict(result="survived 10 seconds; inference unvalidated")
    return report


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("directory", type=Path)
    parser.add_argument("--try-start", action="store_true")
    parser.add_argument("--output", type=Path, default=Path("cpu-cuda-portability.json"))
    args = parser.parse_args()
    report = audit(args.directory, args.try_start)
    args.output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(report, indent=2))
    if summary := os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(summary, "a", encoding="utf-8") as out:
            out.write(f"\n### CPU/CUDA universal artifact: {report['verdict']}\n\n")
            out.write(report["reason"] + ". Inference validated: false.\n")


if __name__ == "__main__":
    main()
