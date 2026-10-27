"""Optional catalog matrix using isolated daemons and actual completion metrics."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import urllib.request

import bench_ar_protocol as bench
from real_agentic_eval import ROOT, Daemon, choose_port, clean_env


def model_file(entry, directory):
    directory = Path(directory)
    directory.mkdir(parents=True, exist_ok=True)
    filename = entry['filename']
    if Path(filename).name != filename or filename in {'.', '..'}:
        raise ValueError('Catalog model filename must remain inside the CI model directory')
    destination = directory / filename
    def digest(path):
        with path.open('rb') as stream:
            return hashlib.file_digest(stream, 'sha256').hexdigest()
    if destination.is_file() and digest(destination) == entry['sha256']:
        return destination
    partial = destination.with_suffix('.partial')
    try:
        with urllib.request.urlopen(entry['url'], timeout=120) as response, partial.open('wb') as output:
            shutil.copyfileobj(response, output)
        if digest(partial) != entry['sha256']:
            raise ValueError('Downloaded catalog model checksum mismatch')
        partial.replace(destination)
    finally:
        partial.unlink(missing_ok=True)
    return destination


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--daemon', type=Path, required=True)
    parser.add_argument('--data-dir', type=Path, required=True)
    args = parser.parse_args()
    root = args.data_dir.resolve()
    root.mkdir(parents=True, exist_ok=True)
    catalog = json.loads((ROOT / 'spec/embedded_models.json').read_text(encoding='utf-8'))
    reports = []
    passed = True
    for entry in catalog['models']:
        try:
            model = model_file(entry, root / 'models/embedded')
            for layers in (0, 99):
                profile = root / f"matrix-{entry['id']}-{layers}"
                installed = profile / 'models/embedded' / entry['filename']
                installed.parent.mkdir(parents=True, exist_ok=True)
                if not installed.exists():
                    os.link(model, installed)
                shutil.copy2(ROOT / 'spec/llm_router.example.yaml', profile / 'llm_router.yaml')
                port = choose_port()
                env = {key: value for key, value in clean_env().items() if not key.startswith('AKASHA_')}
                env.update(AKASHA_DATA_DIR=str(profile), AKASHA_SPEC_DIR=str(ROOT / 'spec'),
                           AKASHA_PORT=str(port), AKASHA_EMBEDDED_BACKEND='llama_cpp',
                           AKASHA_EMBEDDED_GGUF_PATH=str(installed), AKASHA_EMBEDDED_N_GPU_LAYERS=str(layers))
                daemon = Daemon(args.daemon.resolve(), env, profile / 'daemon.log', f'http://127.0.0.1:{port}')
                try:
                    daemon.start()
                    protocol = bench.load_protocol(bench.DEFAULT_PROTOCOL)
                    protocol['candidates'] = [{'id': entry['id'], 'arch': 'catalog', 'tier': 'measured'}]
                    os.environ['AKASHA_EMBEDDED_N_GPU_LAYERS'] = str(layers)
                    result = bench.live_run(protocol, daemon.base, entry['id'], 128, entry['id'], None)
                    passed = passed and result['totals']['ok'] == result['totals']['expected_rows']
                    reports.append(result)
                finally:
                    daemon.stop()
        except Exception as error:
            passed = False
            reports.append({'model': entry['id'], 'error': str(error), 'passed': False})
        (root / 'embedded-matrix.json').write_text(json.dumps({'passed': passed, 'reports': reports}, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
    return 0 if passed and len(reports) == 2 * len(catalog['models']) else 1


if __name__ == '__main__':
    raise SystemExit(main())
