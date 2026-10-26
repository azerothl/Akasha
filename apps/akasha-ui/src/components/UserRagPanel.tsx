import { useCallback, useEffect, useRef, useState } from "react";
import { RemoteConsentPicker, type PrivacyFetch, type RemoteConsent } from "./RemotePrivacy";
import { exportResearchReportPdf } from "../researchReportExport";
import { useFocusScope } from "../hooks/useFocusScope";

export type UserRagDocument = { id:string; name:string; mime_type:string; added_at:string; index_status?:string; indexed_at?:string|null; index_error?:string|null; index_version?:number; pages?:number; warnings?:{page:number;message:string}[] };
type Reference = { document_id:string;document_name:string;page:number|null;char_start:number|null;char_end:number|null;method:string;ocr_confidence:number|null;warning:string|null;source_sha256:string;source_url:string };
const extractionLabels:Record<string,string>={utf8_text:"texte original",pdfium_text:"texte du PDF",tesseract_5_fra_eng:"OCR local"};
type Report = { response:string;references?:{citation:number;reference:Reference;excerpt:string}[];warnings?:{page:number;message:string}[];missing?:string[];omitted_passages?:number;deliverables?:{url:string}[] };
type Job = { id:string;documentId?:string;kind:"index"|"answer";label:string };
type Props = { t:(key:string)=>string;locale:string;daemonPort:number|undefined;documents:UserRagDocument[];loading:boolean;error:string|null;onError:(message:string|null)=>void;onRefresh:()=>void;fetchEndpoint:PrivacyFetch;readFileAsBase64:(file:File)=>Promise<{content_base64:string;mime_type:string}> };

function SourceDialog({source,onClose}:{source:{name:string;image?:string;text?:string};onClose:()=>void}) {
  const dialog=useRef<HTMLDivElement>(null);useFocusScope(dialog,true,onClose,{inertOutside:true});
  return <div className="human-input-overlay" ref={dialog} role="dialog" aria-modal="true" aria-label={source.name} tabIndex={-1}><div className="human-input-modal document-source-modal"><h3>{source.name}</h3><button type="button" className="btn-secondary" onClick={onClose}>Fermer la source</button>{source.image?<img className="document-source-image" src={source.image} alt={source.name}/>:<pre className="consumer-demo-source">{source.text}</pre>}</div></div>;
}
export function UserRagPanel({t,locale,documents:initial,loading,error,onError,onRefresh,fetchEndpoint,readFileAsBase64}:Props) {
  const [documents,setDocuments]=useState(initial);const [selected,setSelected]=useState<string[]>([]);
  const [jobs,setJobs]=useState<Job[]>([]);const [progress,setProgress]=useState<Record<string,string>>({});
  const [uploading,setUploading]=useState(false);const [question,setQuestion]=useState("");const [report,setReport]=useState<Report|null>(null);
  const [source,setSource]=useState<{name:string;image?:string;text?:string}|null>(null);
  const [consents,setConsents]=useState<RemoteConsent[]>([]);const fileInput=useRef<HTMLInputElement>(null);
  const [lastTask,setLastTask]=useState<string|null>(()=>localStorage.getItem("akasha_document_last_task"));
  const request=useCallback(async(path:string,body?:unknown,method="POST")=>{const result=await fetchEndpoint(path,body===undefined?undefined:{method,body:JSON.stringify(body)});const value=JSON.parse(result.text);if(!result.ok)throw new Error(value.error??value.detail??`HTTP ${result.status}`);return value;},[fetchEndpoint]);
  const refresh=useCallback(async()=>{const result=await request("/api/user-rag/documents");setDocuments(result.documents??[]);onRefresh();},[request,onRefresh]);
  useEffect(()=>setDocuments(initial),[initial]);
  useEffect(()=>{if(!lastTask)return;let disposed=false;void request(`/api/tasks/${lastTask}`).then(task=>{if(disposed)return;if(task.status==="completed"&&task.outcome?.references)setReport(task.outcome);else if(["pending","running","queued"].includes(task.status))setJobs(current=>current.some(job=>job.id===lastTask)?current:[...current,{id:lastTask,kind:"answer",label:"Synthèse documentaire"}]);}).catch(()=>{ /* A restored profile can have no such task. */ });return()=>{disposed=true;};},[lastTask,request]);
  useEffect(()=>{
    if(jobs.length===0)return;let disposed=false;let timer:number;
    async function poll(){
      await Promise.all(jobs.map(async job=>{try{const task=await request(`/api/tasks/${job.id}`);if(disposed)return;setProgress(current=>({...current,[job.id]:task.progress?.at(-1)?.message??"En cours…"}));
        if(["completed","failed","cancelled","interrupted"].includes(task.status)){
          setJobs(current=>current.filter(item=>item.id!==job.id));
          if(job.kind==="answer"&&task.status==="completed")setReport(task.outcome);
          else if(task.status!=="completed")onError(task.outcome?.response??"L’opération s’est arrêtée. Vous pouvez réessayer.");
          if(job.kind==="index")await refresh();
        }
      }catch{if(!disposed)setProgress(current=>({...current,[job.id]:"Connexion interrompue ; nouvel essai…"}));}}));
      if(!disposed)timer=window.setTimeout(()=>void poll(),1000);
    }void poll();return()=>{disposed=true;window.clearTimeout(timer);};
  },[jobs,request,refresh,onError]);
  async function upload(files:File[]){
    if(files.length>20||files.some(file=>file.size>25*1024*1024)){onError("Choisissez au maximum 20 fichiers de 25 Mio chacun.");return;}
    setUploading(true);onError(null);
    try{const batch=await request("/api/user-rag/batches",{file_count:files.length});
      for(const file of files){const content=await readFileAsBase64(file);const result=await request("/api/user-rag/documents",{name:file.name,...content,batch_id:batch.batch_id});
        if(result.task_id)setJobs(current=>[...current,{id:result.task_id,documentId:result.id,kind:"index",label:file.name}]);
        setSelected(current=>[...current,result.id]);await refresh();}
    }catch(error){onError(String(error));}finally{setUploading(false);}
  }
  async function ask(){
    onError(null);setReport(null);
    try{const result=await request("/api/message",{message:question.trim()||"Résume les informations importantes de ces documents et signale les informations manquantes.",document_ids:selected,composer_mode:"ask",incognito:true,remote_consents:consents});setConsents([]);localStorage.setItem("akasha_document_last_task",result.task_id);setLastTask(result.task_id);setJobs(current=>[...current,{id:result.task_id,kind:"answer",label:"Synthèse documentaire"}]);}
    catch(error){onError(String(error));}
  }
  async function openSource(reference:Reference){
    try{const document=documents.find(doc=>doc.id===reference.document_id);const text=document?.name.match(/\.(txt|md)$/i);
      if(text){const original=await request(reference.source_url);const bytes=Uint8Array.from(atob(original.content_base64),c=>c.charCodeAt(0));setSource({name:reference.document_name,text:new TextDecoder().decode(bytes)});}
      else{const result=await request(`/api/user-rag/documents/${reference.document_id}/pages/${reference.page??1}`);setSource({name:`${reference.document_name} · page ${reference.page??1}`,image:`data:image/png;base64,${result.png_base64}`});}
    }catch(error){onError(String(error));}
  }
  async function exportReport(pdf:boolean){
    try{const url=report?.deliverables?.[0]?.url;if(!url)throw new Error("Rapport enregistré indisponible");const result=await request(url);
      if(pdf){const opened=await exportResearchReportPdf({topic:"Comprendre mes documents",dateLabel:new Date().toLocaleDateString(locale),reportMarkdown:result.markdown,locale:locale==="en"?"en":"fr"},{hint:"Choisissez Enregistrer au format PDF dans la fenêtre d’impression.",buttonLabel:"Enregistrer en PDF"});if(!opened)throw new Error("La fenêtre d’export a été bloquée. Autorisez son ouverture puis réessayez.");}
      else{const url=URL.createObjectURL(new Blob([result.markdown],{type:"text/markdown;charset=utf-8"}));const link=document.createElement("a");link.href=url;link.download=result.name;link.click();setTimeout(()=>URL.revokeObjectURL(url),1000);}
    }catch(error){onError(String(error));}
  }
  return <section aria-label="Comprendre mes documents" className="document-workspace">
    <h3 className="settings-subtitle">Comprendre mes documents</h3><p>Ajoutez vos documents, obtenez une synthèse puis vérifiez chaque référence dans l’original.</p>
    <p className="muted">PDF texte ou scanné, JPG, PNG, TXT et Markdown · 25 Mio et 100 pages par fichier · 20 fichiers par lot. Lecture locale ; écriture manuscrite non prise en charge.</p>
    <p>Les documents et conversations ne sont pas chiffrés par Akasha pendant cette bêta.</p>
    {error?<p role="alert">{error}</p>:null}
    <input ref={fileInput} type="file" multiple accept=".pdf,.jpg,.jpeg,.png,.txt,.md" className="sr-only" tabIndex={-1} aria-label="Choisir mes documents" onChange={event=>{const files=Array.from(event.target.files??[]);event.target.value="";if(files.length)void upload(files);}}/>
    <button type="button" className="btn-primary" disabled={uploading||loading} onClick={()=>fileInput.current?.click()}>{uploading?"Import en cours…":"Ajouter des documents"}</button>
    {jobs.map(job=><div key={job.id} role="status"><p>{job.label} · {progress[job.id]??"En attente…"}</p><button type="button" className="btn-secondary" onClick={()=>void request(`/api/tasks/${job.id}/cancel`,{}).catch(error=>onError(String(error)))}>Annuler {job.label}</button></div>)}
    {loading?<p aria-busy="true">{t("common.loading")}</p>:documents.length===0?<p className="empty-state">Ajoutez un premier fichier pour commencer.</p>:<ul className="settings-doc-list">{documents.map(doc=><li key={doc.id} className="settings-doc-item">
      <label><input type="checkbox" checked={selected.includes(doc.id)} onChange={event=>setSelected(current=>event.target.checked?[...current,doc.id]:current.filter(id=>id!==doc.id))}/>{doc.name}</label>
      <span>{doc.index_status==="ready"?"Prêt":doc.index_status==="partial"?"Partiellement lu":doc.index_status==="failed"?"Non lu":doc.index_status==="indexing"?"Lecture en cours":"En attente"}{doc.pages?` · ${doc.pages} page(s)`:""}</span>
      {doc.index_error?<p role="alert">{doc.index_error}</p>:null}{doc.warnings?.map(w=><p key={w.page}>Page {w.page} : {w.message}</p>)}
      <button type="button" className="btn-secondary" disabled={jobs.some(job=>job.documentId===doc.id)} onClick={()=>void request(`/api/user-rag/documents/${doc.id}/reindex`,{}).then(result=>setJobs(current=>[...current,{id:result.task_id,documentId:doc.id,kind:"index",label:doc.name}])).catch(error=>onError(String(error)))}>Relire ce document</button>
      <button type="button" className="settings-doc-delete" aria-label={`Supprimer ${doc.name}`} onClick={()=>void request(`/api/user-rag/documents/${doc.id}`,{},"DELETE").then(()=>{setSelected(current=>current.filter(id=>id!==doc.id));return refresh();}).catch(error=>onError(String(error)))}>Supprimer</button>
    </li>)}</ul>}
    <label htmlFor="document-question">Votre question sur les documents sélectionnés</label><textarea id="document-question" className="onboarding-wizard-input" value={question} onChange={event=>setQuestion(event.target.value)} placeholder="Ex. Quel montant dois-je payer et à quelle date ?"/>
    <RemoteConsentPicker fetchEndpoint={fetchEndpoint} value={consents} onChange={setConsents} disabled={jobs.some(job=>job.kind==="answer")} locale={locale==="en"?"en":"fr"}/>
    <button type="button" className="btn-primary" disabled={!selected.length||jobs.some(job=>job.kind==="answer")||selected.some(id=>!documents.some(doc=>doc.id===id&&["ready","partial"].includes(doc.index_status??"")))} onClick={()=>void ask()}>Comprendre ces documents</button>
    {report?<article aria-label="Synthèse documentaire"><h4>Votre synthèse</h4><p className="document-answer">{report.response}</p><p>Vérifiez l’interprétation dans les sources originales.</p>
      {report.references?.map(({citation,reference,excerpt})=><details key={`${citation}:${reference.document_id}`}><summary>[{citation}] {reference.document_name} · page {reference.page??"inconnue"}</summary><blockquote>{excerpt}</blockquote><p>{reference.warning??`Extraction : ${extractionLabels[reference.method]??"méthode inconnue"}`}{reference.ocr_confidence!==null?` · confiance OCR ${reference.ocr_confidence} %`:""}</p><button type="button" className="btn-secondary" onClick={()=>void openSource(reference)}>Ouvrir la source [{citation}]</button></details>)}
      {report.missing?.map((message,index)=><p key={index}>Information absente ou incertaine : {message}</p>)}{report.warnings?.map((warning,index)=><p key={index}>Page {warning.page} : {warning.message}</p>)}{report.omitted_passages?<p>{report.omitted_passages} passages n’ont pas été utilisés. Posez une question plus précise pour les consulter.</p>:null}
      <button type="button" className="btn-secondary" onClick={()=>void exportReport(false)}>Exporter en Markdown</button><button type="button" className="btn-secondary" onClick={()=>void exportReport(true)}>Exporter en PDF</button>
    </article>:null}{source?<SourceDialog source={source} onClose={()=>setSource(null)}/>:null}
  </section>;
}
