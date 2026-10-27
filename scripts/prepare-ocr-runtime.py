"""Build a pinned minimal Windows OCR runtime without installing global tools.

Requires MSVC 2022 and CMake. Only the upstream source archives and PDFium binary
listed in spec/ocr_runtime.json are downloaded. Rust decodes PNG/JPEG, so the
Leptonica build deliberately has no image codecs, CURL or archive dependencies.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
PINS = json.loads((ROOT / "spec/ocr_runtime.json").read_text(encoding="utf-8"))


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def fetch(url, target, expected):
    target.parent.mkdir(parents=True, exist_ok=True)
    if not target.exists() or digest(target) != expected:
        temp = target.with_suffix(".download")
        urllib.request.urlretrieve(url, temp)
        if digest(temp) != expected:
            raise ValueError(f"Upstream hash mismatch: {target.name}")
        temp.replace(target)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cache", type=Path, default=Path(os.environ.get("LOCALAPPDATA", ".")) / "AkashaDevTools/ocr-5.5.3-pdfium-8111")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if os.name != "nt":
        raise SystemExit("This distribution runtime targets Windows x64.")
    cache = args.cache.resolve()
    output = args.output.resolve()
    cmake = shutil.which("cmake") or "C:/Program Files/CMake/bin/cmake.exe"
    for name in ("pdfium", "tesseract", "leptonica"):
        pin = PINS[name]
        archive = cache / f"{name}-pinned.tar.gz"
        fetch(pin["url"], archive, pin["sha256"])
        destination = cache / ("pdfium" if name == "pdfium" else "source")
        destination.mkdir(parents=True, exist_ok=True)
        with tarfile.open(archive) as bundle:
            # Reject escapes, devices and links; these archives need ordinary files only.
            for member in bundle.getmembers():
                resolved = (destination / member.name).resolve()
                if not resolved.is_relative_to(destination.resolve()) or not (member.isfile() or member.isdir()):
                    raise ValueError(f"Unsafe archive member: {member.name}")
            bundle.extractall(destination, filter="data")
    prefix = cache / "minimal"
    bridge = ROOT / "native/ocr/akasha_utf8.cpp"
    if digest(bridge) != PINS["utf8_bridge_sha256"]:
        raise ValueError("OCR UTF-8 bridge source hash mismatch")
    tesseract_source = cache / "source/tesseract-5.5.3"
    shutil.copy2(bridge, tesseract_source / "akasha_utf8.cpp")
    with (tesseract_source / "CMakeLists.txt").open("a", encoding="utf-8") as cmake_source:
        cmake_source.write('\ntarget_sources(libtesseract PRIVATE "${CMAKE_CURRENT_SOURCE_DIR}/akasha_utf8.cpp")\n')

    def build(name, flags):
        source = cache / "source" / f"{name}-{PINS[name]['version']}"
        build_dir = cache / f"build-{name}"
        subprocess.run([cmake, "-S", str(source), "-B", str(build_dir), "-G", "Visual Studio 17 2022", "-A", "x64", f"-DCMAKE_INSTALL_PREFIX={prefix}", *flags], check=True)
        subprocess.run([cmake, "--build", str(build_dir), "--config", "Release", "--parallel", "4"], check=True)
        subprocess.run([cmake, "--install", str(build_dir), "--config", "Release"], check=True)

    build("leptonica", ["-DCMAKE_POLICY_DEFAULT_CMP0091=NEW", "-DCMAKE_MSVC_RUNTIME_LIBRARY=MultiThreaded", "-DBUILD_SHARED_LIBS=OFF", "-DSW_BUILD=OFF", "-DBUILD_PROG=OFF", *[f"-DENABLE_{codec}=OFF" for codec in ("ZLIB", "PNG", "GIF", "JPEG", "TIFF", "WEBP", "OPENJPEG")]])
    build("tesseract", ["-DBUILD_SHARED_LIBS=ON", "-DWIN32_MT_BUILD=ON", "-DSW_BUILD=OFF", "-DOPENMP_BUILD=OFF", "-DGRAPHICS_DISABLED=ON", "-DBUILD_TRAINING_TOOLS=OFF", "-DBUILD_TESTS=OFF", "-DDISABLE_TIFF=ON", "-DDISABLE_ARCHIVE=ON", "-DDISABLE_CURL=ON", "-DENABLE_NATIVE=OFF", f"-DCMAKE_PREFIX_PATH={prefix}"])
    output.mkdir(parents=True, exist_ok=True)
    shutil.copy2(prefix / "bin/tesseract55.dll", output / "tesseract55.dll")
    shutil.copy2(cache / "pdfium/bin/pdfium.dll", output / "pdfium.dll")
    for language, expected in PINS["languages"].items():
        fetch(f"https://raw.githubusercontent.com/tesseract-ocr/tessdata_fast/{PINS['tessdata_revision']}/{language}.traineddata", output / f"tessdata/{language}.traineddata", expected)
    licenses = output / "licenses"
    shutil.copytree(cache / "pdfium/licenses", licenses / "pdfium", dirs_exist_ok=True)
    licenses.mkdir(exist_ok=True)
    shutil.copy2(cache / "source/tesseract-5.5.3/LICENSE", licenses / "tesseract.txt")
    shutil.copy2(cache / "source/leptonica-1.87.0/leptonica-license.txt", licenses / "leptonica.txt")
    # tessdata_fast is Apache-2.0, with the same upstream license text.
    shutil.copy2(licenses / "tesseract.txt", licenses / "tessdata-fast.txt")
    files = [{"path": str(path.relative_to(output)).replace("\\", "/"), "sha256": digest(path)} for path in sorted(output.rglob("*")) if path.is_file() and path.name != "runtime-lock.json"]
    lock = output / "runtime-lock.json"
    lock.write_text(json.dumps({"schema_version": 1, "pins": PINS, "files": files}, sort_keys=True, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"runtime": str(output), "AKASHA_OCR_LOCK_SHA256": digest(lock)}))


if __name__ == "__main__":
    main()
