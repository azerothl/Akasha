// @vitest-environment jsdom
import {cleanup,fireEvent,render,screen,waitFor} from "@testing-library/react";
import {afterEach,expect,it,vi} from "vitest";
import {ConsumerOrganizationPanel} from "./ConsumerOrganizationPanel";
import {ConsumerWatchPanel} from "./ConsumerWatchPanel";
import {ConsumerBackupPanel} from "./ConsumerBackupPanel";
import {MemoryEntryActions} from "./MemoryEntryActions";
import type {PrivacyFetch} from "./RemotePrivacy";
vi.mock("@tauri-apps/api/core",()=>({invoke:vi.fn(async(command:string)=>command==="choose_profile_backup"?"C:/synthetic/profile.akbk":null)}));
afterEach(()=>cleanup());
function response(value:unknown,ok=true){return{ok,status:ok?200:400,text:JSON.stringify(value)};}
it("keeps ambiguous action dates empty and requires confirmation before persistence",async()=>{
 const fetch=vi.fn(async(path:string,init?:RequestInit)=>{
  if(path==="/api/privacy")return response({memory:{state:"encrypted"},services:[]});
  if(path==="/api/notes")return response({notes:[]});if(path==="/api/consumer/actions")return response({actions:[]});
  if(path==="/api/message")return response({task_id:"proposal-task"});
  if(path==="/api/tasks/proposal-task")return response({status:"completed",outcome:{preview:{id:"preview",proposals:[{id:"p1",title:"Payer",suggested_date:"lundi",explanation:"Heure à confirmer"}]}}});
  if(path==="/api/consumer/actions/confirm"){const input=JSON.parse(init!.body as string);expect(input.confirmed).toBe(true);expect(input.actions[0].remind_local).toBe("2027-06-21T10:30");return response({status:"completed",outcome:{response:"Rappel effectivement enregistré"}});}
  throw Error(path);
 });render(<ConsumerOrganizationPanel fetchEndpoint={fetch} locale="fr"/>);
 fireEvent.change(screen.getByLabelText("Votre note"),{target:{value:"Payer lundi"}});fireEvent.click(screen.getByText("Proposer des actions"));await screen.findByText("Heure à confirmer");expect(fetch.mock.calls.some(([path])=>path==="/api/consumer/actions/confirm")).toBe(false);
 fireEvent.click(screen.getByLabelText("Ajouter un rappel"));const date=screen.getByLabelText(/Date et heure confirmées/);expect((date as HTMLInputElement).value).toBe("");const submit=screen.getByText("Enregistrer les actions et rappels confirmés") as HTMLButtonElement;expect(submit.disabled).toBe(true);
 fireEvent.change(date,{target:{value:"2027-06-21T10:30"}});fireEvent.click(screen.getByLabelText("Je confirme les actions et les dates affichées."));fireEvent.click(submit);await screen.findByText("Rappel effectivement enregistré");
});
it("resets watch permissions after a source change and never sends private context",async()=>{
 const fetch=vi.fn(async(path:string,init?:RequestInit)=>{
  if(path==="/api/consumer/watch/services")return response({services:[{name:"searxng",endpoint:"https://search.test",recipient:"search:searxng:https://search.test"}]});
  if(path==="/api/consumer/watch"&&!init)return response({watches:[],reports:[]});
  if(path==="/api/consumer/watch"){const value=JSON.parse(init!.body as string);expect(Object.keys(value).sort()).toEqual(["confirmed","consents","frequency","provider","sources","topic"]);expect(value.consents).toHaveLength(2);return response({task_id:"watch-task"});}
  if(path==="/api/tasks/watch-task")return response({status:"completed",outcome:{}});throw Error(path);
 });render(<ConsumerWatchPanel fetchEndpoint={fetch}/>);await screen.findByText("searxng · https://search.test");fireEvent.change(screen.getByLabelText("Sujet de la veille"),{target:{value:"Sujet synthétique"}});fireEvent.change(screen.getByLabelText(/Adresses des sources/),{target:{value:"https://source.test/page"}});
 const authorize=()=>{fireEvent.click(screen.getByLabelText(/Recherche Web · searxng/));fireEvent.click(screen.getByLabelText(/Lecture de source · https:/));fireEvent.click(screen.getByLabelText(/Je confirme la fréquence/));};authorize();const submit=screen.getByText("Créer la veille et lancer son premier rapport") as HTMLButtonElement;expect(submit.disabled).toBe(false);
 fireEvent.change(screen.getByLabelText(/Adresses des sources/),{target:{value:"https://source.test/other"}});expect(submit.disabled).toBe(true);authorize();fireEvent.click(submit);await waitFor(()=>expect(fetch.mock.calls.filter(([path,init])=>path==="/api/consumer/watch"&&!!init)).toHaveLength(1));
});
it("keeps a failed restore isolated and never offers activation",async()=>{
 const fetch=vi.fn(async(path:string)=>path==="/api/backup/artifacts"?response({artifacts:[]}):path==="/api/backup/restore"?response({task_id:"restore"}):response({status:"failed",outcome:{response:"Archive altérée ou phrase incorrecte"}}));render(<ConsumerBackupPanel fetchEndpoint={fetch} daemonPort={1}/>);
 fireEvent.click(screen.getByText("Choisir une sauvegarde Akasha"));await screen.findByText(/Archive choisie/);fireEvent.change(screen.getByLabelText(/Phrase de sauvegarde/),{target:{value:"une phrase synthétique"}});fireEvent.click(screen.getByLabelText(/Je confirme la restauration/));fireEvent.click(screen.getByText("Vérifier la phrase et restaurer"));await screen.findByRole("alert");expect(screen.queryByText("Choisir le profil restauré")).toBeNull();expect(fetch.mock.calls.some(([path])=>path==="/api/backup/activate")).toBe(false);
});
it("requires an explicit memory correction and sends the original entry id",async()=>{
 const fetch=vi.fn<PrivacyFetch>(async()=>response({updated:true}));const changed=vi.fn();render(<MemoryEntryActions id="stable-id" content="Ancien fait" fetchEndpoint={fetch} onChanged={changed}/>);fireEvent.click(screen.getByText("Corriger ce souvenir"));fireEvent.change(screen.getByLabelText(/Contenu corrigé/),{target:{value:"Fait corrigé"}});expect((screen.getByText("Enregistrer la correction") as HTMLButtonElement).disabled).toBe(true);fireEvent.click(screen.getByLabelText(/Je confirme cette correction/));fireEvent.click(screen.getByText("Enregistrer la correction"));await waitFor(()=>expect(changed).toHaveBeenCalledOnce());expect(fetch.mock.calls[0][0]).toBe("/api/memory/long-term/stable-id");expect(fetch.mock.calls[0][1]?.method).toBe("PUT");
});
