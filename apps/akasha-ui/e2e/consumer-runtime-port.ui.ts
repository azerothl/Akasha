import {expect,test} from "@playwright/test";
test("the frontend uses the custom launch port before health or data requests",async({page})=>{
 await page.addInitScript(()=>{localStorage.clear();localStorage.setItem("akasha_locale","fr");localStorage.setItem("akasha_setup_wizard_done","1");localStorage.setItem("akasha_ui_mode","simple");Object.assign(window,{__TAURI_INTERNALS__:{invoke:async(command:string,args:unknown)=>(await fetch("/__launch_port",{method:"POST",body:JSON.stringify({command,args})})).json(),transformCallback:()=>1,unregisterCallback:()=>{}}});});
 const commands:string[]=[];await page.route("**/__launch_port",async route=>{
  const {command,args={}}=route.request().postDataJSON();commands.push(command);let value:unknown={};
  if(command==="get_daemon_launch_config")value={port:3880};else if(args.port!==undefined)expect(args.port).toBe(3880);
  if(command==="check_health")value={ok:true,port:3880};
  if(command==="daemon_request"||command==="daemon_get_text"){let data:unknown={};if(args.path==="/api/privacy")data={consumer_beta_build:true,services:[],grants:[],memory:{state:"encrypted",encrypted_at_rest:true}};if(args.path==="/api/user-rag/documents")data={documents:[]};value={ok:true,status:200,text:JSON.stringify(data)};}
  if(["get_tasks","get_sessions","get_notes"].includes(command))value={tasks:[],sessions:[],notes:[]};if(command==="get_pending_human_input")value={pending:[]};await route.fulfill({json:value});
 });
 await page.goto("/");await expect(page.getByRole("region",{name:"Mon espace personnel"})).toBeVisible();expect(commands[0]).toBe("get_daemon_launch_config");expect(commands).toContain("check_health");await page.unrouteAll({behavior:"wait"});
});
