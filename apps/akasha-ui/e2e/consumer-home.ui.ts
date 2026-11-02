import {expect,test} from "@playwright/test";
for(const width of [1440,900])test(`consumer Simple home at ${width}px`,async({page})=>{
 await page.setViewportSize({width,height:800});await page.addInitScript(()=>{localStorage.clear();localStorage.setItem("akasha_locale","fr");localStorage.setItem("akasha_ui_mode","simple");localStorage.setItem("akasha_setup_wizard_done","1");localStorage.setItem("akasha_onboarding_dismissed","1");Object.assign(window,{__TAURI_INTERNALS__:{invoke:async(command:string,args:unknown)=>(await fetch("/__home_ipc",{method:"POST",body:JSON.stringify({command,args})})).json(),transformCallback:()=>1,unregisterCallback:()=>{}}});});
 const documents:{id:string;name:string;index_status:string;mime_type:string}[]=[];let uploads=0;
 await page.route("**/__home_ipc",async route=>{const{command,args={}}=route.request().postDataJSON();let value:unknown={};if(command==="check_health")value={ok:true,port:3876};if(command==="get_user_rag_documents")value={documents};if(command==="daemon_get_text"||command==="daemon_request"){
  let data:unknown={};if(args.path==="/api/privacy")data={consumer_beta_build:true,services:[],grants:[],memory:{state:"encrypted",encrypted_at_rest:true}};
  if(args.path==="/api/user-rag/documents"&&args.method!=="POST")data={documents};
  if(args.path==="/api/consumer/demo")data={synthetic:true,files:["pdf","jpg","txt"].map(extension=>({name:`facture-fictive.${extension}`,mime_type:extension==="pdf"?"application/pdf":extension==="jpg"?"image/jpeg":"text/plain",content_base64:btoa("Montant : 42 euros")}))};
  if(args.path==="/api/user-rag/batches")data={batch_id:"demo"};
  if(args.path==="/api/user-rag/documents"&&args.method==="POST"){const body=JSON.parse(args.body);uploads++;const id=`demo-${uploads}`;documents.push({id,name:body.name,index_status:"ready",mime_type:body.mime_type});data={id,task_id:`index-${uploads}`};}
  if(args.path?.startsWith("/api/tasks/index-"))data={status:"completed",outcome:{response:"Document lu"}};
  if(args.path==="/api/consumer/actions")data={actions:[]};if(args.path==="/api/consumer/watch")data={watches:[],reports:[]};if(args.path==="/api/consumer/watch/services")data={services:[]};if(args.path==="/api/backup/artifacts")data={artifacts:[]};
  value={ok:true,status:200,text:JSON.stringify(data)};
 }if(["get_tasks","get_sessions","get_notes"].includes(command))value={tasks:[],sessions:[],notes:[]};if(command==="get_pending_human_input")value={pending:[]};await route.fulfill({json:value});});
 await page.goto("/");const home=page.getByRole("region",{name:"Mon espace personnel"});await expect(home).toBeVisible();await page.screenshot({path:`test-results/consumer-home-initial-${width}.png`,fullPage:true});await home.getByText("Essayer le dossier fictif · PDF, photo et texte").click();await expect(home.getByRole("checkbox",{name:"facture-fictive.txt"})).toBeVisible();expect(uploads).toBe(3);await page.screenshot({path:`test-results/consumer-home-demo-${width}.png`,fullPage:true});
 // Auto-scrolling a locator can hide a clipped flex child. Exercise the wheel
 // itself so a normal user can reach the question after importing documents.
 expect(await home.evaluate(element=>element.clientHeight)).toBeLessThanOrEqual(800);
 await home.hover({position:{x:300,y:200}});await page.mouse.wheel(0,1000);
 await expect.poll(()=>home.evaluate(element=>element.scrollTop)).toBeGreaterThan(0);
 await expect(home.getByRole("button",{name:"Comprendre ces documents",exact:true})).toBeInViewport();
 for(const label of ["Mes notes en actions","Ma veille personnelle","Sauvegarder mon profil"]){await home.getByRole("tab",{name:label,exact:true}).click();await expect(home.getByRole("tabpanel")).toBeVisible();}expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);await page.unrouteAll({behavior:"wait"});
});
