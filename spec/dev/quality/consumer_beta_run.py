"""Execute the 50 consumer cases using HTTP workflows, native OCR and storage guards.

Fault injection remains controlled even in live runs. Semantic conversation,
document and proposal cases use the selected real local model with --live.
All profiles/ports are owned by each driver; the installed daemon is untouched.
Missing checks remain failures, never inferred from a model's success statement.
"""
import argparse,hashlib,json,os,re,subprocess,sys,time,uuid
from pathlib import Path
import consumer_beta_eval as gate
ROOT=gate.ROOT
def digest(path):return hashlib.file_digest(Path(path).open('rb'),'sha256').hexdigest()
def main():
 p=argparse.ArgumentParser(description=__doc__);p.add_argument('--binary',type=Path,required=True);p.add_argument('--runtime',type=Path,required=True);p.add_argument('--output',type=Path,required=True);p.add_argument('--live',action='store_true');p.add_argument('--model',default='qwen3.5:9b');p.add_argument('--repeat',type=int,choices=[1,3],default=1);p.add_argument('--series');p.add_argument('--position',type=int,default=1);p.add_argument('--previous-id');p.add_argument('--skip-native',action='store_true',help='Reuse this output directory’s immutable native test receipt, verified against current sources');a=p.parse_args();a.binary=a.binary.resolve();a.runtime=a.runtime.resolve();a.output=a.output.resolve();a.output.mkdir(parents=True,exist_ok=True)
 os.environ['AKASHA_OCR_LOCK_SHA256']=digest(a.runtime/'runtime-lock.json')
 if a.repeat==3:
  import shutil
  series=str(uuid.uuid4());previous=None;reports=[]
  for position in range(1,4):
   folder=a.output/f'run-{position}';folder.mkdir(exist_ok=True)
   command=[sys.executable,__file__,'--binary',str(a.binary),'--runtime',str(a.runtime),'--output',str(folder),'--model',a.model,'--series',series,'--position',str(position),*(['--live'] if a.live else [])]
   if previous:command+=['--previous-id',previous]
   if position>1:
    shutil.copy2(a.output/'run-1/native.json',folder/'native.json')
    for log in ['akasha-daemon.log','akasha-store.log','akasha-llm.log']:shutil.copy2(a.output/'run-1'/log,folder/log)
    command+=['--skip-native']
   subprocess.run(command,cwd=ROOT,check=False);report=json.loads((folder/'report.json').read_text(encoding='utf-8'));reports.append(report);previous=report['run_id']
  qualified=gate.qualify(reports);(a.output/'qualification.json').write_text(json.dumps({'qualified':qualified,'reports':[str(Path(f'run-{index}')/'report.json') for index in range(1,4)]},indent=2)+'\n',encoding='utf-8');return 0 if qualified else 1
 corpus=gate.load_corpus();cases={};native={};source=subprocess.check_output(['git','rev-parse','HEAD'],cwd=ROOT,text=True).strip()
 def execute(name,command,timeout=1800):
  print(json.dumps({'phase':name,'state':'running'}),flush=True);started=time.monotonic();process=subprocess.run(command,cwd=ROOT,env={**os.environ,'CARGO_INCREMENTAL':'0','CARGO_TERM_COLOR':'never'},stdout=subprocess.PIPE,stderr=subprocess.STDOUT,timeout=timeout);output=process.stdout.decode('utf-8',errors='replace');log=a.output/(name+'.log');log.write_text(output,encoding='utf-8');record={'exit_code':process.returncode,'duration_ms':round((time.monotonic()-started)*1000),'log':log.name,'log_sha256':digest(log)};print(json.dumps({'phase':name,**record}),flush=True)
  if process.returncode:print(output[-3500:],flush=True)
  return output,record
 native_path=a.output/'native.json'
 # Bind cached native controls to every source file they compile. This is not a
 # source-only acceptance test: the cached receipt comes from real test processes.
 paths=[ROOT/'Cargo.toml',ROOT/'Cargo.lock']+sorted(path for folder in ['crates','spec'] for path in (ROOT/folder).rglob('*') if path.is_file() and path.suffix in {'.rs','.toml','.yaml','.json'} and '/evidence/' not in path.as_posix())
 source_digest=hashlib.sha256(os.environ['AKASHA_OCR_LOCK_SHA256'].encode()+b''.join(str(path.relative_to(ROOT)).encode()+b'\0'+path.read_bytes() for path in paths)).hexdigest()
 if a.skip_native:
  native=json.loads(native_path.read_text(encoding='utf-8'));assert native['source_sha256']==source_digest,'Native sources changed; rerun native tests';assert all(digest(a.output/value['log'])==value['log_sha256'] for value in native['suites'].values())
 else:
  suites={}
  for package,features in [('akasha-daemon',['--no-default-features','--features','consumer-beta']),('akasha-store',['--features','memory-sqlcipher']),('akasha-llm',[])]:
   output,record=execute(package,['cargo','test','--locked','-p',package,*features,'--lib']);record['tests']=re.findall(r'^test ([\w:]+) \.\.\. ok\s*$',output,re.M);suites[package]=record
  native={'source_sha256':source_digest,'suites':suites};native_path.write_text(json.dumps(native,indent=2)+'\n',encoding='utf-8')
 def native_case(case_id,package,suffix):
  suite=native['suites'][package];names=[name for name in suite['tests'] if name.endswith('::'+suffix)];passed=suite['exit_code']==0 and len(names)==1
  put(case_id,passed,{'kind':'native_assertions','suite':package,'test':names[0] if names else suffix,'return_code':suite['exit_code'],'log_sha256':suite['log_sha256'],'source_sha256':source_digest})
 def put(case_id,passed,receipt,model=False,duration=0):
  if not receipt:return
  definition=next(case for case in corpus['cases'] if case['id']==case_id);checks={check:bool(passed) for check in definition['checks']};checks['no_false_success']=bool(passed) or receipt.get('task',{}).get('status') in {'failed','cancelled','interrupted'} or receipt.get('return_code',0)!=0;cases[case_id]={'observed':definition['expect'] if passed else 'failure','checks':checks,'model_executed':model,'duration_ms':duration,'receipt':receipt}
 # Actual malformed responses, cancellation and dependency failures are injected
 # into the ordinary chat path, with parent/child statuses and receipts checked.
 chat=a.output/'chat.json';execute('chat',[sys.executable,str(Path(__file__).with_name('chat_agent_audit.py')),'--daemon',str(a.binary),'--cases','qa,followup,installed_version,ask_denied,lfm_repair,json_stuck,cancel,delegation_failure,example','--output',str(chat)])
 controlled=json.loads(chat.read_text(encoding='utf-8'));conversations={row['case']:row for row in controlled['results']}
 if a.live:
  live_chat=a.output/'chat-live.json';execute('chat-live',[sys.executable,str(Path(__file__).with_name('chat_agent_audit.py')),'--daemon',str(a.binary),'--model',a.model,'--cases','qa,followup,installed_version,ask_denied,example','--output',str(live_chat)])
  conversations.update({row['case']:row for row in json.loads(live_chat.read_text(encoding='utf-8'))['results']})
 for case_id,name in {'conversation.fact':'qa','conversation.followup':'followup','conversation.version':'installed_version','conversation.ask_readonly':'ask_denied','conversation.malformed_repair':'lfm_repair','conversation.malformed_exhausted':'json_stuck','conversation.cancel':'cancel','conversation.child_failed':'delegation_failure','conversation.dependency_failed':'delegation_failure','conversation.syntax_example':'example'}.items():
  row=conversations.get(name,{});put(case_id,row.get('passed',False),row,a.live and row.get('model_executed',False),row.get('duration_ms',0))
 worker=a.output/'worker.json';execute('worker',[sys.executable,str(Path(__file__).with_name('document_worker_audit.py')),'--binary',str(a.binary),'--runtime',str(a.runtime),'--output',str(worker)])
 workers=json.loads(worker.read_text(encoding='utf-8'))
 documents=a.output/'documents.json';execute('documents',[sys.executable,str(Path(__file__).with_name('document_workflow_audit.py')),'--binary',str(a.binary),'--runtime',str(a.runtime),'--fixtures',str(a.output/'document-fixtures-éété'),'--model',a.model,'--output',str(documents),*([] if a.live else ['--controlled'])])
 docs=json.loads(documents.read_text(encoding='utf-8'));docrows=docs['cases'];imports={row['name']:row for row in docrows if row['kind']=='import'}
 for case_id,name in {'documents.text':'unicode-été.txt','documents.unicode':'unicode-été.txt','documents.pdf_text':'text.pdf','documents.pdf_scan':'scan.pdf','documents.pdf_mixed':'mixed.pdf','documents.photo_jpg':'photo.jpg','documents.photo_png':'photo.png'}.items():
  row=imports.get(name,{});passed=row.get('passed',False)
  if case_id=='documents.pdf_mixed':
   same_page=next((item for item in workers['cases'] if item['file']=='mixed-content.pdf' and item['operation']=='extract'),{})
   passed=passed and same_page.get('passed',False);row={'workflow':row,'same_page_text_and_scan':same_page}
  put(case_id,passed,row)
 for case_id,kind in [('documents.missing_fact','missing_fact'),('documents.unreadable','unreadable'),('documents.injection','answer_export_injection'),('documents.restart','restart')]:
  row=next((row for row in docrows if row['kind']==kind),{});put(case_id,row.get('passed',False),row,a.live and kind in {'missing_fact','answer_export_injection'},row.get('duration_ms',0))
 for case_id,name in [('documents.rotation','rotation.png'),('documents.page_limit','too-many-pages.pdf')]:
  row=next((row for row in workers['cases'] if row['file']==name),{});put(case_id,row.get('passed',False),row)
 native_case('documents.size_limit','akasha-daemon','oversized_source_is_rejected_before_persistence');native_case('documents.batch_limit','akasha-daemon','batch_limit_is_enforced_on_the_server_before_import')
 organization=a.output/'organization.json';execute('organization',[sys.executable,str(Path(__file__).with_name('consumer_organization_audit.py')),'--binary',str(a.binary),'--model',a.model,'--output',str(organization),*([] if a.live else ['--controlled'])]);org=json.loads(organization.read_text(encoding='utf-8'));orgrows={row['id']:row for row in org['cases']}
 for case_id,name in {'organization.note':'restart_uses_restored_profile_without_replaying_reminders','organization.preview':'preview_without_effects','organization.ambiguous':'ambiguous_date_and_missing_confirmation_rejected','organization.confirm':'confirmation_persists_once','organization.timezone':'confirmation_persists_once','organization.duplicate':'confirmation_persists_once','organization.restart':'restart_uses_restored_profile_without_replaying_reminders','protection.wrong_password':'wrong_password','protection.tampered_archive':'tampered_archive','protection.restore':'restore_isolated'}.items():
  row=orgrows.get(name,{});passed=row.get('passed',False)
  if case_id=='organization.timezone':passed=passed and row['actions']['actions'][0]['timezone']=='UTC'
  put(case_id,passed,row,a.live and case_id=='organization.preview',row.get('duration_ms',0))
 store_test='preview_has_no_reminders_and_confirmation_is_persistent_and_idempotent';native_case('organization.cancel','akasha-store',store_test);native_case('organization.missed','akasha-store',store_test);native_case('organization.memory_edit','akasha-store','corrected_or_deleted_sources_do_not_leave_stale_derived_memory')
 for case_id in ['watch.sources','watch.novelty','watch.revoke']:native_case(case_id,'akasha-daemon','watch_needs_its_own_grant_tracks_seen_items_and_stops_after_revocation')
 native_case('watch.unavailable','akasha-daemon','unavailable_sources_produce_a_failed_task_and_a_retained_error_report');native_case('watch.missed','akasha-daemon','personal_watch_resumes_once_after_many_missed_deadlines')
 for case_id in ['protection.local_default','protection.grant','protection.changed_recipient']:native_case(case_id,'akasha-llm','transport_is_never_called_without_scoped_consent')
 native_case('protection.inherit','akasha-daemon','a_child_inherits_only_the_roots_scope_and_revocation')
 for case_id,name in [('protection.encrypted_memory','actual_windows_device_key_protects_all_memory_tables'),('protection.missing_key','missing_key_cannot_reinitialize_or_truncate_database'),('protection.migration','legacy_migration_preserves_schema_ids_and_embedding_bytes')]:native_case(case_id,'akasha-store',name)
 identity={'binary_sha256':digest(a.binary),'source_revision':source,'model':'controlled-fixture','model_digest':digest(Path(__file__).with_name('chat_agent_audit.py')),'engine':'controlled-http-and-native-assertions','configuration_sha256':source_digest}
 if a.live:
  probe=a.output/'model-probe.json';execute('model-probe',[sys.executable,str(Path(__file__).with_name('consumer_model_probe.py')),'--binary',str(a.binary),'--model',a.model,'--output',str(probe)]);identity=json.loads(probe.read_text(encoding='utf-8')).get('identity',{})
 evidence={'run_id':str(uuid.uuid4()),'series_id':a.series or str(uuid.uuid4()),'sequence_number':a.position,'previous_run_id':a.previous_id,'mode':'live' if a.live else 'deterministic','identity':identity,'cases':cases};(a.output/'evidence.json').write_text(json.dumps(evidence,ensure_ascii=False,indent=2)+'\n',encoding='utf-8');report=gate.report(corpus,evidence);(a.output/'report.json').write_text(json.dumps(report,ensure_ascii=False,indent=2)+'\n',encoding='utf-8');print(json.dumps({'passed':report['passed'],'total':report['total'],'counts':report['counts'],'passed_gate':report['passed_gate']}),flush=True);return 0 if report['passed_gate'] else 1
if __name__=='__main__':raise SystemExit(main())
