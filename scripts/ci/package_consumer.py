"""Package the isolated Windows beta; no installation or public publication."""
import argparse,hashlib,json,mmap,os,re,shutil,subprocess
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
def digest(path):return hashlib.file_digest(path.open('rb'),'sha256').hexdigest()

def binary_build_info(path):
    # Do not execute the target: a CUDA executable may require a driver absent
    # from its build host. The same embedded record powers the diagnostic flag.
    with path.open('rb') as file:
        with mmap.mmap(file.fileno(),0,access=mmap.ACCESS_READ) as data:
            records=set(re.findall(rb'AKASHA_CONSUMER_INFO_V1:(\{[^\x00]{1,2048}?\}):END_AKASHA_CONSUMER_INFO_V1',data))
    # Optimized code can retain identical copies in different codegen units.
    # Only byte-identical records describe one identity; differing records
    # remain ambiguous and must never be chosen by position in the executable.
    if len(records)!=1:raise ValueError('Missing or ambiguous consumer build metadata')
    return json.loads(next(iter(records)))
def package(daemon,ui,runtime,output,cuda=False,embeddings=None):
    packaging_revision=subprocess.check_output(['git','rev-parse','HEAD'],cwd=ROOT,text=True).strip()
    packaging_dirty=bool(subprocess.check_output(['git','status','--porcelain'],cwd=ROOT,text=True).strip())
    build=binary_build_info(daemon)
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
    for name in ['consumer-beta-document.webm','consumer-beta-document-receipt.json']:
        if (ROOT/'docs/demos'/name).is_file():shutil.copy2(ROOT/'docs/demos'/name,output/'demo'/name)
    if embeddings is None:raise ValueError('Pinned offline embedding components are required')
    shutil.copytree(embeddings,output/'embedding_model')
    (output/'qualification').mkdir();shutil.copy2(ROOT/'spec/dev/quality/CONSUMER_BETA_DELIVERY.md',output/'qualification/DELIVERY.md')
    for document in (ROOT/'spec/dev/quality').glob('consumer_beta_*.md'):shutil.copy2(document,output/'qualification'/document.name)
    (output/'README.md').write_text((ROOT/'spec/consumer-beta-windows.md').read_text(encoding='utf-8').replace('dev/quality/CONSUMER_BETA_DELIVERY.md','qualification/DELIVERY.md').replace('../docs/demos/consumer-beta-document.webm','demo/consumer-beta-document.webm'),encoding='utf-8')
    shutil.copy2(ROOT/'LICENSE',output/'LICENSE')
    if cuda:
        cuda_bin=Path(os.environ['CUDA_PATH'])/'bin'
        for pattern in ['cudart64_*.dll','cublas64_*.dll','cublasLt64_*.dll']:
            matches=list(cuda_bin.glob(pattern))
            if not matches:raise ValueError('CUDA runtime component missing: '+pattern)
            for file in matches:shutil.copy2(file,output/file.name)
    files=[{'path':path.relative_to(output).as_posix(),'sha256':digest(path),'bytes':path.stat().st_size} for path in sorted(output.rglob('*')) if path.is_file()]
    metadata={'schema_version':1,'edition':'consumer-beta','version':build['version'],'candidate_only':True,'personal_data_pilot_authorized':False,'source_revision':build['source_revision'],'source_dirty':build['source_dirty'],'packaging_source_revision':packaging_revision,'packaging_source_dirty':packaging_dirty,'variant':'nvidia' if cuda else 'cpu','daemon_port':3877,'runtime_lock_sha256':digest(runtime/'runtime-lock.json'),'qualification_required':['full_50_corpus','three_real_runs','windows_cpu','windows_nvidia','second_account_restore','distributed_package_install_update_restore'],'files':files}
    (output/'consumer-package.json').write_text(json.dumps(metadata,indent=2)+'\n',encoding='utf-8')
    return metadata
if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__)
    for name in ['daemon','ui','runtime','output']:p.add_argument('--'+name,type=Path,required=True)
    p.add_argument('--cuda',action='store_true');p.add_argument('--embeddings',type=Path,required=True);a=p.parse_args();result=package(a.daemon,a.ui,a.runtime,a.output,a.cuda,a.embeddings);print(json.dumps({'edition':result['edition'],'files':len(result['files']),'candidate_only':True}))
