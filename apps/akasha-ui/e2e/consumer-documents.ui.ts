import {expect,test} from "@playwright/test";
for(const width of [1440,900])test(`document result and source at ${width}px`,async({page})=>{
 await page.setViewportSize({width,height:1000});
 await page.addInitScript(()=>{
  localStorage.clear();localStorage.setItem("akasha_setup_wizard_done","1");localStorage.setItem("akasha_onboarding_dismissed","1");localStorage.setItem("akasha_locale","fr");
  Object.assign(window,{__TAURI_INTERNALS__:{invoke:async(command:string,args:unknown)=>(await fetch("/__document_ipc",{method:"POST",body:JSON.stringify({command,args})})).json(),transformCallback:()=>1,unregisterCallback:()=>{}}});
 });
 const fixtureDoc={id:"11111111-1111-4111-8111-111111111111",name:"facture.txt",mime_type:"text/plain",added_at:"2026-10-26",index_status:"ready",index_version:2,pages:1};
 await page.route("**/__document_ipc",async route=>{
  const{command,args={}}=route.request().postDataJSON();let value:unknown={};
  if(command==="check_health")value={ok:true,port:3876};
  if(command==="get_user_rag_documents")value={documents:[fixtureDoc]};
  if(command==="daemon_get_text"||command==="daemon_request"){
   const data=args.path==="/api/privacy"?{consumer_beta_build:true,services:[],search_services:[],grants:[],memory:{state:"encrypted",encrypted_at_rest:true}}
    :args.path==="/api/user-rag/documents"?{documents:[fixtureDoc]}
    :args.path==="/api/message"?{task_id:"document-answer"}
    :args.path==="/api/tasks/document-answer"?{status:"completed",outcome:{response:"Le montant est de 42 euros [1].",references:[{citation:1,excerpt:"Montant : 42 euros",reference:{document_id:fixtureDoc.id,document_name:fixtureDoc.name,page:1,source_url:"/api/user-rag/documents/source",method:"utf8_text",ocr_confidence:null}}]}}
    :args.path==="/api/user-rag/documents/source"?{content_base64:btoa("Montant : 42 euros")}:{};
   if(args.path==="/api/message")expect(JSON.parse(args.body).document_ids).toEqual([fixtureDoc.id]);
   value={ok:true,status:200,text:JSON.stringify(data)};
  }
  if(["get_tasks","get_sessions","get_notes"].includes(command))value={tasks:[],sessions:[],notes:[]};
  if(command==="get_pending_human_input")value={pending:[]};
  await route.fulfill({json:value});
 });
 await page.goto("/");await page.locator("#tab-settings").click();
 await page.locator(".settings-tabs button").nth(4).click();
 const workspace=page.getByRole("region",{name:"Comprendre mes documents"});await expect(workspace).toBeVisible();
 await page.screenshot({path:`test-results/consumer-documents-initial-${width}.png`,fullPage:true});
 await workspace.getByRole("checkbox",{name:"facture.txt"}).check();
 await workspace.getByLabel("Votre question sur les documents sélectionnés").fill("Quel montant dois-je payer ?");
 await workspace.getByRole("button",{name:"Comprendre ces documents",exact:true}).click();
 await expect(workspace.getByText("Le montant est de 42 euros [1].")).toBeVisible();
 await workspace.locator("summary").filter({hasText:"[1] facture.txt"}).click();
 await workspace.getByRole("button",{name:"Ouvrir la source [1]"}).click();
 await expect(page.getByRole("dialog",{name:"facture.txt"})).toBeVisible();
 await page.getByRole("button",{name:"Fermer la source"}).click();
 expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);
 await page.screenshot({path:`test-results/consumer-documents-${width}.png`,fullPage:true});
 await page.unrouteAll({behavior:"wait"});
});
