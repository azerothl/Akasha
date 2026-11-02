import {expect,test} from "@playwright/test";
test("calendar reminders require confirmation and show a rejected write",async({page})=>{
 await page.addInitScript(()=>{
  localStorage.clear();for(const [key,value] of Object.entries({akasha_locale:"fr",akasha_setup_wizard_done:"1",akasha_ui_mode:"expert",akasha_session_id:"synthetic-session"}))localStorage.setItem(key,value);
  Object.assign(window,{__TAURI_INTERNALS__:{invoke:async(command:string,args:unknown)=>(await fetch("/__calendar_ipc",{method:"POST",body:JSON.stringify({command,args})})).json(),transformCallback:()=>1,unregisterCallback:()=>{}}});
 });
 const writes:unknown[]=[];
 await page.route("**/__calendar_ipc",async route=>{
  const {command,args={}}=route.request().postDataJSON();let value:unknown={};
  if(command==="check_health")value={ok:true,port:3876};
  if(command==="get_memory_short_term")value={session_id:"synthetic-session",turns:[]};
  if(command==="daemon_get_text"||command==="daemon_request"){
   let data:unknown={};if(args.path==="/api/privacy")data={services:[],search_services:[],grants:[],memory:{state:"encrypted",encrypted_at_rest:true},consumer_beta_build:true};
   if(args.path==="/api/wakeups")data={wakeups:[]};
   value={ok:true,status:200,text:JSON.stringify(data)};
   if(args.path==="/api/wakeups"&&args.method==="POST"){writes.push(JSON.parse(args.body));value={ok:false,status:400,text:"Rappel synthétique refusé"};}
  }
  if(["get_tasks","get_sessions","get_notes"].includes(command))value={tasks:[],sessions:[],notes:[]};if(command==="get_pending_human_input")value={pending:[]};await route.fulfill({json:value});
 });
 await page.goto("/#/calendar");await page.getByRole("tab",{name:"Rappels agent",exact:false}).click();
 await page.getByPlaceholder("Ex. Relancer la revue du PR").fill("Rappel fictif");
 page.once("dialog",dialog=>dialog.dismiss());await page.getByRole("button",{name:"Programmer",exact:true}).click();expect(writes).toHaveLength(0);
 page.once("dialog",dialog=>dialog.accept());await page.getByRole("button",{name:"Programmer",exact:true}).click();
 await expect(page.getByRole("alert").filter({hasText:"Rappel synthétique refusé"})).toBeVisible();
 expect(writes).toHaveLength(1);expect(writes[0]).toMatchObject({confirmed:true,message:"Rappel fictif",session_id:"synthetic-session"});await page.unrouteAll({behavior:"wait"});
});
