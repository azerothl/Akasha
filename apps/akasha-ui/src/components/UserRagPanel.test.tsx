// @vitest-environment jsdom
import {cleanup,fireEvent,render,screen,waitFor} from "@testing-library/react";
import {afterEach,expect,it,vi} from "vitest";
import {UserRagPanel} from "./UserRagPanel";
afterEach(()=>{cleanup();localStorage.clear();});
const document={id:"source-id",name:"facture.txt",mime_type:"text/plain",added_at:"2026-10-26",index_status:"ready",index_version:2};
function setup(){
 const fetchEndpoint=vi.fn(async(path:string,init?:RequestInit)=>{
  let value:unknown={};
  if(path==="/api/privacy")value={memory:{state:"encrypted"},services:[],search_services:[]};
  if(path==="/api/user-rag/documents")value={documents:[document]};
  if(path==="/api/message"){
   const body=JSON.parse(init!.body as string);expect(body.document_ids).toEqual([document.id]);expect(body.incognito).toBe(true);expect(body.remote_consents).toEqual([]);value={task_id:"answer-id"};
  }
  if(path==="/api/tasks/answer-id")value={status:"completed",outcome:{response:"Montant : 42 euros [1]",references:[{citation:1,excerpt:"Montant : 42 euros",reference:{document_id:document.id,document_name:document.name,page:1,source_url:"/source",ocr_confidence:null,method:"utf8_text"}}]}};
  if(path==="/source")value={content_base64:btoa("Montant : 42 euros")};
  return {ok:true,status:200,text:JSON.stringify(value)};
 });
 const onError=vi.fn();render(<UserRagPanel t={key=>key} locale="fr" daemonPort={3876} documents={[document]} loading={false} error={null} onError={onError} onRefresh={vi.fn()} fetchEndpoint={fetchEndpoint} readFileAsBase64={vi.fn()}/>);return{fetchEndpoint,onError};
}
it("opens the original selected source after a real task result is received",async()=>{
 setup();fireEvent.click(screen.getByRole("checkbox",{name:"facture.txt"}));fireEvent.change(screen.getByLabelText("Votre question sur les documents sélectionnés"),{target:{value:"Quel montant ?"}});fireEvent.click(screen.getByRole("button",{name:"Comprendre ces documents"}));
 expect(await screen.findByText("Montant : 42 euros [1]")).toBeDefined();fireEvent.click(screen.getByText("[1] facture.txt · page 1"));fireEvent.click(screen.getByRole("button",{name:"Ouvrir la source [1]"}));
 expect(await screen.findByRole("dialog",{name:"facture.txt"})).toBeDefined();expect(screen.getByRole("button",{name:"Fermer la source"})).toBeDefined();
});
it("rejects oversized import batches before reading or sending a file",async()=>{
 const{fetchEndpoint,onError}=setup();const input=screen.getByLabelText("Choisir mes documents");
 fireEvent.change(input,{target:{files:Array.from({length:21},()=>new File(["demo"],"a.txt"))}});
 await waitFor(()=>expect(onError).toHaveBeenCalledWith("Choisissez au maximum 20 fichiers de 25 Mio chacun."));
 expect(fetchEndpoint.mock.calls.some(([path])=>path==="/api/user-rag/batches")).toBe(false);
});
