"""Fetch public catalogue GGUFs at immutable Hub revisions and verify LFS hashes."""
import argparse,concurrent.futures,hashlib,json,urllib.request
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
def fetch(entry,output):
    from urllib.parse import urlparse
    url=urlparse(entry['url']);assert url.netloc=='huggingface.co';repo,tail=url.path.strip('/').split('/resolve/');revision,relative=tail.split('/',1);api=f'https://huggingface.co/api/models/{repo}/revision/{revision}?blobs=true'
    with urllib.request.urlopen(api,timeout=30) as response:metadata=json.load(response)
    source=next(file for file in metadata['siblings'] if file['rfilename']==relative);expected=source.get('lfs',{}).get('sha256');assert expected and len(expected)==64
    directory=output/entry['id'];directory.mkdir(parents=True,exist_ok=True);target=directory/entry['filename'];immutable=f"https://huggingface.co/{repo}/resolve/{metadata['sha']}/{relative}"
    if not target.is_file() or hashlib.file_digest(target.open('rb'),'sha256').hexdigest()!=expected:
        partial=target.with_suffix('.partial')
        with urllib.request.urlopen(immutable,timeout=120) as response,partial.open('wb') as file:
            while block:=response.read(1024*1024):file.write(block)
        assert hashlib.file_digest(partial.open('rb'),'sha256').hexdigest()==expected,'GGUF checksum mismatch';partial.replace(target)
    return {'model':entry['id'],'filename':entry['filename'],'hub_revision':metadata['sha'],'sha256':expected,'bytes':target.stat().st_size,'url':immutable,'license':metadata.get('cardData',{}).get('license'),'downloaded':True}
if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);a=p.parse_args();a.output.mkdir(parents=True,exist_ok=True);catalog=json.loads((ROOT/'spec/embedded_models.json').read_text(encoding='utf-8'));results=[]
    with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
        futures={pool.submit(fetch,entry,a.output):entry for entry in catalog['models']}
        for future in concurrent.futures.as_completed(futures):
            try:row=future.result()
            except Exception as error:row={'model':futures[future]['id'],'downloaded':False,'error':type(error).__name__+': '+str(error)}
            results.append(row);print(json.dumps(row),flush=True)
    (a.output/'catalog-lock.json').write_text(json.dumps({'schema_version':1,'models':results},indent=2)+'\n',encoding='utf-8')
