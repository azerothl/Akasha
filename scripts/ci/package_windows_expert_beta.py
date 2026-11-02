"""Assemble a portable Expert beta, retaining the account/API tool runtime."""
import argparse
import json
import os
import shutil
import subprocess
from pathlib import Path

from package_consumer import ROOT, binary_build_info, digest, validate_source_provenance


def package(daemon, ui, clients, runtime, embeddings, output, cuda):
    revision = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip()
    dirty = bool(subprocess.check_output(['git', 'status', '--porcelain'], cwd=ROOT, text=True).strip())
    build = binary_build_info(daemon)
    validate_source_provenance(build, revision, dirty)
    if build['consumer_beta'] or build['cuda'] != cuda:
        raise ValueError('Wrong Expert edition or CUDA variant')
    if build['ocr_lock_sha256'] != digest(runtime / 'runtime-lock.json'):
        raise ValueError('OCR runtime differs from compiled pin')
    output.mkdir(parents=True, exist_ok=False)
    for source, name in [(daemon, 'akasha-daemon.exe'), (ui, 'akasha-ui.exe'),
                         (clients / 'akasha.exe', 'akasha.exe'), (clients / 'akasha-tui.exe', 'akasha-tui.exe')]:
        shutil.copy2(source, output / name)
    for source, name in [(runtime, 'ocr-runtime'), (embeddings, 'embedding_model'),
                         (ROOT / 'spec', 'spec'), (ROOT / 'docs', 'docs')]:
        shutil.copytree(source, output / name)
    shutil.copytree(ROOT / 'scripts' / 'playwright-runner', output / 'playwright-runner',
                    ignore=shutil.ignore_patterns('node_modules', '__pycache__', '*.pyc'))
    (output / 'scripts').mkdir()
    for name in ['install.ps1', 'setup.ps1', 'update.ps1']:
        shutil.copy2(ROOT / 'scripts' / name, output / 'scripts' / name)
    shutil.copy2(ROOT / 'LICENSE', output / 'LICENSE')
    if cuda:
        root = Path(os.environ['CUDA_PATH'])
        for pattern in ['cudart64_*.dll', 'cublas64_*.dll', 'cublasLt64_*.dll']:
            matches = sorted({*root.joinpath('bin').glob(pattern), *root.joinpath('bin/x64').glob(pattern)})
            if not matches:
                raise ValueError('Missing CUDA runtime: ' + pattern)
            for source in matches:
                shutil.copy2(source, output / source.name)
    (output / 'START-AKASHA.cmd').write_text('@echo off\r\ncd /d "%~dp0"\r\nstart "" "%~dp0akasha-ui.exe"\r\n', encoding='ascii')
    (output / 'README-BETA.txt').write_text(
        'Akasha ' + build['version'] + ' — édition Expert Windows\n'
        'Fermez Akasha avant de remplacer ses exécutables. Extrayez ce paquet dans un dossier de binaires, '
        'puis lancez START-AKASHA.cmd. Le profil existant ~/akasha est conservé.\n'
        'Cette édition conserve les outils avancés autorisés par votre politique. '
        'Une clé de service ne remplace pas une autorisation de transmission.\n'
        'La mémoire héritée doit être sauvegardée puis migrée via les réglages de protection. '
        'Elle reste verrouillée jusqu’à cette migration ; aucune base vide ne la remplace.\n'
        'Les conversations, notes et documents ne sont pas chiffrés par cette protection de la mémoire.\n'
        'Cette préversion ne modifie pas le canal de mise à jour stable.\n', encoding='utf-8')
    files = [{'path': p.relative_to(output).as_posix(), 'sha256': digest(p), 'bytes': p.stat().st_size}
             for p in sorted(output.rglob('*')) if p.is_file()]
    result = dict(schema_version=1, edition='expert-beta', version=build['version'],
                  source_revision=revision, source_dirty=False, packaging_source_revision=revision,
                  variant='cuda' if cuda else 'cpu', ocr_lock_sha256=build['ocr_lock_sha256'], files=files)
    (output / 'release-beta-package.json').write_text(json.dumps(result, indent=2) + '\n', encoding='utf-8')
    return result


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    for name in ['daemon', 'ui', 'clients', 'runtime', 'embeddings', 'output']:
        p.add_argument('--' + name, type=Path, required=True)
    p.add_argument('--cuda', action='store_true')
    a = p.parse_args()
    result = package(a.daemon, a.ui, a.clients, a.runtime, a.embeddings, a.output, a.cuda)
    print(json.dumps({k: v for k, v in result.items() if k != 'files'}))
