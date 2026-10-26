import {useCallback,useEffect,useState} from "react";
import type {PrivacyFetch} from "./RemotePrivacy";
export function useConsumerRequest(fetchEndpoint:PrivacyFetch){return useCallback(async(path:string,body?:unknown,method="POST")=>{const response=await fetchEndpoint(path,body===undefined?undefined:{method,body:JSON.stringify(body)});const value=JSON.parse(response.text);if(!response.ok)throw new Error(value.error??`Opération indisponible (HTTP ${response.status}). Réessayez.`);return value;},[fetchEndpoint]);}
export function useConsumerJob(request:ReturnType<typeof useConsumerRequest>){
 const[id,setId]=useState<string|null>(null);const[result,setResult]=useState<{taskId:string;outcome:Record<string,any>}|null>(null);const[error,setError]=useState("");const[progress,setProgress]=useState("");
 useEffect(()=>{if(!id)return;let disposed=false;let timer:number;
  async function poll(){try{const task=await request(`/api/tasks/${id}`);if(disposed)return;setProgress(task.progress?.at(-1)?.message??"En cours…");if(["completed","failed","cancelled","interrupted"].includes(task.status)){setId(null);setProgress("");if(task.status==="completed")setResult({taskId:id!,outcome:task.outcome??{}});else setError(task.outcome?.response??"L’opération s’est arrêtée. Vous pouvez réessayer.");return;}}catch{if(!disposed)setProgress("Connexion interrompue ; nouvel essai…");}if(!disposed)timer=window.setTimeout(()=>void poll(),1000);}
  void poll();return()=>{disposed=true;window.clearTimeout(timer);};
 },[id,request]);
 const begin=useCallback((id:string)=>{setError("");setResult(null);setProgress("");setId(id);},[]);
 const cancel=useCallback(()=>{if(id)void request(`/api/tasks/${id}/cancel`,{}).catch(error=>setError(String(error)));},[id,request]);
 return{id,result,error,progress,begin,cancel,setError};
}
