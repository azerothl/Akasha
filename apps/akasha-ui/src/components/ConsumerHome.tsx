import {useCallback,useEffect,useState} from "react";
import type {PrivacyFetch} from "./RemotePrivacy";
import {UserRagPanel,type UserRagDocument} from "./UserRagPanel";
import {ConsumerOrganizationPanel} from "./ConsumerOrganizationPanel";
import {ConsumerWatchPanel} from "./ConsumerWatchPanel";
import {ConsumerBackupPanel} from "./ConsumerBackupPanel";
import {ConsumerPilotPanel} from "./ConsumerPilotPanel";
type Props={fetchEndpoint:PrivacyFetch;daemonPort:number;locale:"fr"|"en";onChat:()=>void;onMemory:()=>void;readFileAsBase64:(file:File)=>Promise<{content_base64:string;mime_type:string}>;t:(key:string)=>string};
export function ConsumerHome({fetchEndpoint,daemonPort,locale,onChat,onMemory,readFileAsBase64,t}:Props){
 const[view,setView]=useState("documents");const[documents,setDocuments]=useState<UserRagDocument[]>([]);const[error,setError]=useState<string|null>(null);const loading=false;
 const refresh=useCallback(()=>{void fetchEndpoint("/api/user-rag/documents").then(response=>{if(!response.ok)throw Error("Lecture des documents indisponible. Réessayez.");setDocuments(JSON.parse(response.text).documents??[]);}).catch(error=>setError(String(error)));},[fetchEndpoint]);useEffect(refresh,[refresh]);
 return <section className="consumer-home" aria-label="Mon espace personnel"><header><p className="consumer-home-edition">Bêta Windows · espace individuel</p><h2>Que souhaitez-vous retrouver ou préparer ?</h2><p>Commencez avec la démonstration fictive, puis vérifiez chaque réponse dans sa source.</p><button type="button" className="btn-secondary" onClick={onChat}>Ouvrir une conversation</button><button type="button" className="btn-secondary" onClick={onMemory}>Consulter mes souvenirs</button></header>
  <div className="consumer-home-tabs" role="tablist" aria-label="Mes usages">{[["documents","Comprendre mes documents"],["organization","Mes notes en actions"],["watch","Ma veille personnelle"],["backup","Sauvegarder mon profil"]].map(([id,label])=><button type="button" key={id} role="tab" aria-selected={view===id} aria-controls={`consumer-home-${id}`} id={`consumer-home-tab-${id}`} onClick={()=>setView(id)}>{label}</button>)}</div>
  <div role="tabpanel" id={`consumer-home-${view}`} aria-labelledby={`consumer-home-tab-${view}`}>
  {view==="documents"?<UserRagPanel documents={documents} loading={loading} error={error} onError={setError} onRefresh={refresh} fetchEndpoint={fetchEndpoint} daemonPort={daemonPort} locale={locale} t={t} readFileAsBase64={readFileAsBase64}/>:null}
  {view==="organization"?<ConsumerOrganizationPanel fetchEndpoint={fetchEndpoint} locale={locale}/>:null}{view==="watch"?<ConsumerWatchPanel fetchEndpoint={fetchEndpoint}/>:null}{view==="backup"?<ConsumerBackupPanel fetchEndpoint={fetchEndpoint} daemonPort={daemonPort}/>:null}
  </div><p className="consumer-home-privacy">Modèles locaux par défaut. Recherche Web et modèles distants exigent un accord. Pendant la bêta, les conversations, notes et documents importés restent hors du chiffrement applicatif.</p><ConsumerPilotPanel fetchEndpoint={fetchEndpoint}/>
 </section>;
}
