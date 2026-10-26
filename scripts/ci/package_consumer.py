"""Package the isolated Windows beta; no installation or public publication."""
import argparse,hashlib,json,os,shutil,socket,subprocess,tempfile
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
def digest(path):return hashlib.file_digest(path.open('rb'),'sha256').hexdigest()
def package(daemon,ui,runtime,output,cuda=False,embeddings=None):
    # Even an incompatible older executable must not open the user's profile.
    with tempfile.TemporaryDirectory(prefix='akasha-package-metadata-') as temporary:
        environment={key:value for key,value in os.environ.items() if not key.upper().startswith('AKASHA_') and not any(word in key.upper() for word in ['API_KEY','TOKEN','SECRET','PASSWORD','CREDENTIAL'])}
        with socket.socket() as listener:
            listener.bind(('127.0.0.1',0));port=listener.getsockname()[1]
        environment.update(AKASHA_DATA_DIR=temporary,AKASHA_PORT=str(port),AKASHA_BIND='127.0.0.1')
        build=json.loads(subprocess.check_output([str(daemon.resolve()),'--consumer-build-info'],text=True,env=environment,cwd=temporary,timeout=10))
    if build.get('consumer_beta') is not True or build.get('cuda')!=cuda:raise ValueError('Wrong daemon edition or CUDA variant')
    if build.get('ocr_lock_sha256')!=digest(runtime/'runtime-lock.json'):raise ValueError('OCR components do not match the compiled daemon pin')
    if output.exists():raise ValueError('Use a new empty candidate directory')
    output.mkdir(parents=True)
    for source,name in [(daemon,'akasha-daemon.exe'),(ui,'AkashaBeta.exe')]:shutil.copy2(source,output/name)
    lock=json.loads((runtime/'runtime-lock.json').read_text(encoding='utf-8'))
    # Runtime validation is performed by the daemon against its compiled hash.
    shutil.copytree(runtime,output/'ocr-runtime')
    (output/'scripts').mkdir()
    for name in ['START-AKASHA.cmd','RECOVER-PROFILE.cmd']:shutil.copy2(ROOT/'scripts/consumer-beta'/name,output/name)
    # Windows PowerShell 5.1 needs a BOM for French UTF-8 text.
    (output/'scripts/consumer-launch.ps1').write_text((ROOT/'scripts/consumer-beta/consumer-launch.ps1').read_text(encoding='utf-8-sig'),encoding='utf-8-sig')
    (output/'spec').mkdir()
    for name in ['09_event_model.yaml','10_data_model.yaml','embedded_models.json','tools_policy.example.yaml']:shutil.copy2(ROOT/'spec'/name,output/'spec'/name)
    (output/'spec/dev/quality').mkdir(parents=True)
    shutil.copy2(ROOT/'spec/dev/quality/embedded_profiles.json',output/'spec/dev/quality/embedded_profiles.json')
    shutil.copytree(ROOT/'spec/consumer_demo',output/'demo')
    if embeddings is None:raise ValueError('Pinned offline embedding components are required')
    shutil.copytree(embeddings,output/'embedding_model')
    (output/'qualification').mkdir();shutil.copy2(ROOT/'spec/dev/quality/CONSUMER_BETA_DELIVERY.md',output/'qualification/DELIVERY.md')
    for document in (ROOT/'spec/dev/quality').glob('consumer_beta_*.md'):shutil.copy2(document,output/'qualification'/document.name)
    (output/'README.md').write_text((ROOT/'spec/consumer-beta-windows.md').read_text(encoding='utf-8').replace('dev/quality/CONSUMER_BETA_DELIVERY.md','qualification/DELIVERY.md'),encoding='utf-8')
    shutil.copy2(ROOT/'LICENSE',output/'LICENSE')
    if cuda:
        import os
        cuda_bin=Path(os.environ['CUDA_PATH'])/'bin'
        for pattern in ['cudart64_*.dll','cublas64_*.dll','cublasLt64_*.dll']:
            matches=list(cuda_bin.glob(pattern))
            if not matches:raise ValueError('CUDA runtime component missing: '+pattern)
            for file in matches:shutil.copy2(file,output/file.name)
    files=[{'path':path.relative_to(output).as_posix(),'sha256':digest(path),'bytes':path.stat().st_size} for path in sorted(output.rglob('*')) if path.is_file()]
    metadata={'schema_version':1,'edition':'consumer-beta','version':build['version'],'candidate_only':True,'personal_data_pilot_authorized':False,'source_revision':build['source_revision'],'source_dirty':build['source_dirty'],'variant':'nvidia' if cuda else 'cpu','daemon_port':3877,'runtime_lock_sha256':digest(runtime/'runtime-lock.json'),'qualification_required':['full_50_corpus','three_real_runs','windows_cpu','windows_nvidia','second_account_restore','distributed_package_install_update_restore'],'files':files}
    (output/'consumer-package.json').write_text(json.dumps(metadata,indent=2)+'\n',encoding='utf-8')
    return metadata
if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__)
    for name in ['daemon','ui','runtime','output']:p.add_argument('--'+name,type=Path,required=True)
    p.add_argument('--cuda',action='store_true');p.add_argument('--embeddings',type=Path,required=True);a=p.parse_args();result=package(a.daemon,a.ui,a.runtime,a.output,a.cuda,a.embeddings);print(json.dumps({'edition':result['edition'],'files':len(result['files']),'candidate_only':True}))
