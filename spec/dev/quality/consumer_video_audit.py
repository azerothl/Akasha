"""Record the real synthetic document flow without touching an installed profile."""
import argparse,hashlib,json,os,shutil,subprocess,tempfile
from pathlib import Path
from real_agentic_eval import ROOT,Daemon,clean_env,choose_port,write_router,http_json
if __name__=='__main__':
 p=argparse.ArgumentParser(description=__doc__);p.add_argument('--binary',type=Path,required=True);p.add_argument('--runtime',type=Path,required=True);p.add_argument('--output',type=Path,required=True);a=p.parse_args();a.output=a.output.resolve();a.output.mkdir(parents=True,exist_ok=True)
 with tempfile.TemporaryDirectory(prefix='akasha-consumer-video-') as temporary:
  root=Path(temporary);data=root/'profile';data.mkdir();binary=root/a.binary.name;shutil.copy2(a.binary,binary);write_router(data,'qwen3.5:9b','http://127.0.0.1:11434','auto');port=choose_port();env={key:value for key,value in clean_env().items() if not key.startswith('AKASHA_')};env.update(AKASHA_DATA_DIR=str(data),AKASHA_SPEC_DIR=str(ROOT/'spec'),AKASHA_PORT=str(port),AKASHA_OCR_RUNTIME_DIR=str(a.runtime.resolve()));daemon=Daemon(binary,env,root/'daemon.log',f'http://127.0.0.1:{port}')
  try:
   daemon.start();result=subprocess.run(['npx.cmd' if os.name=='nt' else 'npx','playwright','test','--config','playwright.consumer-live.config.ts','--output',str(a.output/'browser')],cwd=ROOT/'apps/akasha-ui',env={**os.environ,'CONSUMER_LIVE_DAEMON':daemon.base,'CONSUMER_LIVE_SCREENSHOT':str(a.output/'source.png'),'CONSUMER_LIVE_MARKDOWN':str(a.output/'report.md')},stdout=subprocess.PIPE,stderr=subprocess.STDOUT,timeout=240);(a.output/'browser.log').write_bytes(result.stdout);print(result.stdout.decode(errors='replace'));tasks=http_json(daemon.base,'GET','/api/tasks');(a.output/'receipt.json').write_text(json.dumps({'binary_sha256':hashlib.sha256(a.binary.read_bytes()).hexdigest(),'mode':'live','model':'qwen3.5:9b','playwright_exit_code':result.returncode,'tasks':tasks},ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
  finally:daemon.stop()
 raise SystemExit(result.returncode)
