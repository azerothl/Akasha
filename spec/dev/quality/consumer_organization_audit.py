"""Synthetic HTTP organization/backup/restore audit, in an owned profile and port."""
import argparse
import hashlib
import json
import os
import urllib.request,urllib.error
from pathlib import Path
import shutil
import sqlite3
import tempfile
import threading
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
from datetime import datetime,timedelta,timezone
from real_agentic_eval import ROOT,Daemon,clean_env,choose_port,http_json,write_router
from document_workflow_audit import wait

def rejected(base,path,body):
    request=urllib.request.Request(base+path,data=json.dumps(body).encode(),headers={"Content-Type":"application/json"},method="POST")
    try:
        with urllib.request.urlopen(request,timeout=20) as response:return json.load(response)
    except urllib.error.HTTPError as error:
        assert error.code==400
        return json.load(error)

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary",type=Path,required=True)
    parser.add_argument("--output",type=Path,required=True)
    parser.add_argument("--controlled",action="store_true")
    args=parser.parse_args();cases=[];fixture=None
    if args.controlled:
        class Provider(BaseHTTPRequestHandler):
            def log_message(self,*args):pass
            def do_POST(self):
                self.rfile.read(int(self.headers["Content-Length"]))
                text=json.dumps({"actions":[{"title":"Payer la facture synthétique","date_hint":"lundi","explanation":"Confirmer la date et l’heure"}]})
                body=json.dumps({"choices":[{"message":{"content":text},"finish_reason":"stop"}]}).encode()
                self.send_response(200);self.send_header("Content-Type","application/json");self.send_header("Content-Length",str(len(body)));self.end_headers();self.wfile.write(body)
        fixture=ThreadingHTTPServer(("127.0.0.1",0),Provider);threading.Thread(target=fixture.serve_forever,daemon=True).start()
    with tempfile.TemporaryDirectory(prefix="akasha-organization-audit-") as temporary:
        root=Path(temporary);data=root/"profile";data.mkdir();binary=root/args.binary.name;shutil.copy2(args.binary,binary)
        write_router(data,"qwen3.5:9b","http://127.0.0.1:11434","auto");consent=[]
        if fixture:
            endpoint=f"http://127.0.0.1:{fixture.server_port}/v1";route={"primary":{"provider":"openai","model":"organization-fixture"},"fallback":[]}
            (data/"llm_router.yaml").write_text(json.dumps({"providers":{"openai":{"base_url":endpoint}},"global":{"enable_fallback":False,"default_max_retries":0},"task_types":{kind:route for kind in ["conversation","system","orchestrator"]}}),encoding="utf-8")
            consent=[{"recipient":f"model:openai:{endpoint}","data_classes":["message","history","memory","documents","tool_results"]}]
        env={key:value for key,value in clean_env().items() if not key.upper().startswith("AKASHA_")};port=choose_port();env.update(AKASHA_DATA_DIR=str(data),AKASHA_PORT=str(port),AKASHA_SPEC_DIR=str(ROOT/"spec"))
        if fixture:env["OPENAI_API_KEY"]="synthetic-key"
        daemon=Daemon(binary,env,root/"daemon.log",f"http://127.0.0.1:{port}")
        def record(name,passed,**evidence):
            cases.append({"id":name,"passed":bool(passed),**evidence});print(json.dumps({"id":name,"passed":bool(passed)}),flush=True)
        try:
            daemon.start();base=daemon.base
            note=http_json(base,"POST","/api/notes",{"title":"Note synthétique","content":"Payer la facture synthétique lundi. L’heure reste à confirmer."})
            ack=http_json(base,"POST","/api/message",{"message":"Propose les actions de cette note, sans rien enregistrer.","organization_note":{"note_id":note["id"]},"remote_consents":consent,"composer_mode":"ask","incognito":True});task=wait(base,ack["task_id"])
            actions=http_json(base,"GET","/api/consumer/actions");wakeups=http_json(base,"GET","/api/wakeups");children=http_json(base,"GET",f"/api/tasks/{ack['task_id']}/children")
            preview=task.get("outcome",{}).get("preview",{});proposals=preview.get("proposals",[])
            record("preview_without_effects",task["status"]=="completed" and proposals and not actions.get("actions") and not wakeups.get("wakeups") and not children.get("children"),task=task,actions=actions,wakeups=wakeups,children=children)
            assert proposals,"model failed to produce proposals"
            action={"proposal_id":proposals[0]["id"],"title":"Payer la facture synthétique","remind_local":"2026-10-25T02:30","timezone":"Europe/Paris"}
            ambiguous=rejected(base,"/api/consumer/actions/confirm",{"preview_id":preview["id"],"confirmed":True,"actions":[action]})
            missing=rejected(base,"/api/consumer/actions/confirm",{"preview_id":preview["id"],"confirmed":False,"actions":[action]})
            record("ambiguous_date_and_missing_confirmation_rejected",bool(ambiguous.get("error")) and bool(missing.get("error")) and not http_json(base,"GET","/api/consumer/actions").get("actions"),ambiguous=ambiguous,missing_confirmation=missing)
            due=(datetime.now(timezone.utc)+timedelta(hours=1)).replace(second=0,microsecond=0);action.update(remind_local=due.strftime("%Y-%m-%dT%H:%M"),timezone="UTC")
            payload={"preview_id":preview["id"],"confirmed":True,"actions":[action]};confirmed=http_json(base,"POST","/api/consumer/actions/confirm",payload);again=http_json(base,"POST","/api/consumer/actions/confirm",payload)
            actions=http_json(base,"GET","/api/consumer/actions");persisted=actions.get("actions",[])
            record("confirmation_persists_once",confirmed.get("status")=="completed" and again.get("status")=="completed" and len(persisted)==1 and persisted[0]["reminder_status"]=="pending" and confirmed["outcome"]["actions"]==again["outcome"]["actions"],first=confirmed,repeat=again,actions=actions)
            phrase="phrase synthétique de sauvegarde sûre";ack=http_json(base,"POST","/api/backup/create",{"passphrase":phrase});backup=wait(base,ack["task_id"]);archive=Path(backup.get("outcome",{}).get("backup_path",""))
            record("backup_encrypted_and_no_password_in_task",backup["status"]=="completed" and archive.is_file() and archive.read_bytes()[:8]==b"AKBK\0\0\0\x01" and phrase not in json.dumps(backup),task=backup,archive_sha256=hashlib.sha256(archive.read_bytes()).hexdigest() if archive.is_file() else None)
            for label,password,file in [("wrong_password","phrase complètement différente",archive),("tampered_archive",phrase,root/"tampered.akbk")]:
                if label=="tampered_archive":raw=bytearray(archive.read_bytes());raw[-1]^=1;file.write_bytes(raw)
                ack=http_json(base,"POST","/api/backup/restore",{"passphrase":password,"archive":str(file),"confirmed":True});failed=wait(base,ack["task_id"])
                record(label,failed["status"]=="failed" and not failed.get("outcome",{}).get("restored_profile") and data.is_dir(),task=failed)
            ack=http_json(base,"POST","/api/backup/restore",{"passphrase":phrase,"archive":str(archive),"confirmed":True});restored=wait(base,ack["task_id"]);target=Path(restored.get("outcome",{}).get("restored_profile",""))
            record("restore_isolated",restored["status"]=="completed" and target!=data and target.is_dir() and not (target/"remote_consents.json").exists() and not (target/"connectors.env").exists() and (data/"notes").exists(),task=restored)
            activated=http_json(base,"POST","/api/backup/activate",{"restore_task_id":ack["task_id"],"confirmed":True});record("activation_waits_for_restart",activated.get("restart_required") and http_json(base,"GET","/api/consumer/actions")["actions"][0]["reminder_status"]=="pending",receipt=activated)
            daemon.stop();daemon=Daemon(binary,env,root/"restarted.log",base);daemon.start();after=http_json(base,"GET","/api/consumer/actions");notes=http_json(base,"GET","/api/notes")
            record("restart_uses_restored_profile_without_replaying_reminders",len(after["actions"])==1 and after["actions"][0]["id"]==persisted[0]["id"] and after["actions"][0]["reminder_status"]=="disabled_restored" and notes["notes"][0]["id"]==note["id"],actions=after,notes=notes)
        finally:
            daemon.stop();args.output.parent.mkdir(parents=True,exist_ok=True);args.output.write_text(json.dumps({"binary_sha256":hashlib.sha256(args.binary.read_bytes()).hexdigest(),"mode":"deterministic" if fixture else "live","model":"organization-fixture" if fixture else "qwen3.5:9b","cases":cases,"passed":len(cases)==9 and all(case["passed"] for case in cases)},ensure_ascii=False,indent=2)+"\n",encoding="utf-8")
    if fixture:fixture.shutdown();fixture.server_close()
    return 0 if len(cases)==9 and all(case["passed"] for case in cases) else 1
if __name__=="__main__":raise SystemExit(main())
