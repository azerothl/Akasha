import {expect,test} from "@playwright/test";
for(const width of [1440,900])test(`organization watch and backup at ${width}px`,async({page})=>{
 await page.setViewportSize({width,height:1000});
 await page.addInitScript(()=>{localStorage.clear();localStorage.setItem("akasha_setup_wizard_done","1");localStorage.setItem("akasha_onboarding_dismissed","1");localStorage.setItem("akasha_locale","fr");localStorage.setItem("akasha_ui_mode","expert");Object.assign(window,{__TAURI_INTERNALS__:{invoke:async(command:string,args:unknown)=>(await fetch("/__flow_ipc",{method:"POST",body:JSON.stringify({command,args})})).json(),transformCallback:()=>1,unregisterCallback:()=>{}}});});
 await page.route("**/__flow_ipc",async route=>{
  const{command,args={}}=route.request().postDataJSON();let value:unknown={};if(command==="check_health")value={ok:true,port:3876};if(command==="get_user_rag_documents")value={documents:[]};
  if(command==="daemon_get_text"||command==="daemon_request"){
   const data=args.path==="/api/privacy"?{consumer_beta_build:true,services:[],search_services:[],grants:[],memory:{state:"encrypted",encrypted_at_rest:true}}
    :args.path==="/api/consumer/actions"?{actions:[{id:"one",title:"Payer la facture",completed:false,remind_at:"2026-11-10T09:00:00Z",reminder_status:"pending"}]}
    :args.path==="/api/consumer/watch/services"?{services:[{name:"searxng",endpoint:"https://search.example",recipient:"search:searxng:https://search.example"}]}
    :args.path==="/api/consumer/watch"?{watches:[],reports:[{task_id:"report",topic:"Aides à la rénovation",created_at:"2026-10-26T09:00:00Z",usable:true,items:[{title:"Source publique synthétique",url:"https://example.org/article",snippet:"Une information nouvelle à vérifier dans sa source.",novelty:"new"}],unavailable_sources:[{source:"https://unavailable.example",reason:"Source inaccessible",operation:"fetch"}]}]}
    :args.path==="/api/backup/artifacts"?{artifacts:[{id:"one",name:"sauvegarde.akbk",bytes:10240}]}
    :args.path==="/api/notes"?{notes:[]}:{};value={ok:true,status:200,text:JSON.stringify(data)};
  }
  if(["get_tasks","get_sessions","get_notes"].includes(command))value={tasks:[],sessions:[],notes:[]};if(command==="get_pending_human_input")value={pending:[]};await route.fulfill({json:value});
 });
 await page.goto("/");await page.locator("#tab-notes").click();await expect(page.getByRole("region",{name:"Mes notes en actions"})).toBeVisible();await expect(page.getByText("Rappel enregistré",{exact:false}).first()).toBeVisible();await page.screenshot({path:`test-results/consumer-organization-${width}.png`,fullPage:true});
 await page.locator("#tab-scheduled").click();await expect(page.getByRole("region",{name:"Ma veille personnelle"})).toBeVisible();await expect(page.getByText("Source inaccessible",{exact:false})).toBeVisible();await page.screenshot({path:`test-results/consumer-watch-${width}.png`,fullPage:true});
 await page.locator("#tab-settings").click();await page.locator(".settings-tabs button").nth(4).click();await expect(page.getByRole("region",{name:"Sauvegarder et retrouver mon profil"})).toBeVisible();await page.screenshot({path:`test-results/consumer-backup-${width}.png`,fullPage:true});expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);await page.unrouteAll({behavior:"wait"});
});
