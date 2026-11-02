import {useState} from "react";
import type {PrivacyFetch} from "./RemotePrivacy";
import {useConsumerRequest} from "./consumerHooks";
export function MemoryEntryActions({id,content,fetchEndpoint,onChanged}:{id:string;content:string;fetchEndpoint:PrivacyFetch;onChanged:()=>void}){
 const request=useConsumerRequest(fetchEndpoint);const[editing,setEditing]=useState(false);const[value,setValue]=useState(content);const[confirmed,setConfirmed]=useState(false);const[removing,setRemoving]=useState(false);const[busy,setBusy]=useState(false);const[error,setError]=useState("");
 async function save(remove:boolean){setBusy(true);setError("");try{await request(`/api/memory/long-term/${id}`,remove?{}:{content:value,confirmed},remove?"DELETE":"PUT");setEditing(false);setRemoving(false);setConfirmed(false);onChanged();}catch(error){setError(String(error));}finally{setBusy(false);}}
 return <div className="memory-entry-actions" onClick={event=>event.stopPropagation()}>
  {!editing&&!removing?<><button type="button" className="btn-secondary" onClick={()=>{setValue(content);setEditing(true);}}>Corriger ce souvenir</button><button type="button" className="btn-secondary" onClick={()=>setRemoving(true)}>Supprimer ce souvenir</button></>:null}
  {editing?<><label htmlFor={`memory-edit-${id}`}>Contenu corrigé · provenance conservée</label><textarea id={`memory-edit-${id}`} value={value} maxLength={65536} onChange={event=>{setValue(event.target.value);setConfirmed(false);}}/><label><input type="checkbox" checked={confirmed} onChange={event=>setConfirmed(event.target.checked)}/>Je confirme cette correction et le retrait des anciens faits dérivés.</label><button className="btn-primary" type="button" disabled={busy||!confirmed||!value.trim()} onClick={()=>void save(false)}>Enregistrer la correction</button></>:null}
  {removing?<><p>Supprimer ce souvenir, ses faits dérivés et ses liens ? Les conversations d’origine restent dans l’historique.</p><button className="btn-primary" type="button" disabled={busy} onClick={()=>void save(true)}>Confirmer la suppression</button></>:null}
  {editing||removing?<button type="button" className="btn-secondary" disabled={busy} onClick={()=>{setEditing(false);setRemoving(false);setConfirmed(false);}}>Annuler</button>:null}{error?<p role="alert">{error}</p>:null}
 </div>;
}
