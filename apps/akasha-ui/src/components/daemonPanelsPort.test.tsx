// @vitest-environment jsdom
import {afterEach,beforeEach,expect,it,vi} from "vitest";
import {cleanup,fireEvent,render,screen,waitFor} from "@testing-library/react";
import {invoke} from "@tauri-apps/api/core";
import {DAEMON_PORT,initializeDaemonPort} from "../daemonPort";
import {UsageDashboardPanel} from "./UsageDashboardPanel";
import {EmbeddedLocalModelSettings} from "./EmbeddedLocalModelSettings";
import {CreateTaskDialog} from "./CreateTaskDialog";
import {listNotes} from "../notesApi";
import {e2eDaemonHttpUrl} from "../e2eDaemon";
import {I18nProvider} from "../useI18n";
vi.mock("@tauri-apps/api/core",()=>({invoke:vi.fn()}));
const original=DAEMON_PORT;
const t=(key:string)=>key;
beforeEach(async()=>{
 await initializeDaemonPort(true,async()=>({port:3880}));
 vi.mocked(invoke).mockImplementation(async(command,args)=>{
  expect((args as {port?:number})?.port).toBe(3880);
  if(command==="get_embedded_status")return {embedded_available:true,active_model_id:"synthetic"};
  if(command==="get_embedded_runtime")return {active_model_id:"synthetic",engine_mode:"cpu",models:[{id:"synthetic",label:"Synthetic",filename:"test.gguf"}]};
  if(command==="get_notes")return {notes:[]};
  return {};
 });
});
afterEach(async()=>{cleanup();vi.clearAllMocks();await initializeDaemonPort(true,async()=>({port:original}));});
it("loads usage metrics from the launch instance",async()=>{
 render(<I18nProvider><UsageDashboardPanel t={t} locale="fr"/></I18nProvider>);
 await waitFor(()=>expect(invoke).toHaveBeenCalledWith("get_router_metrics",{port:3880,period:"week"}));
});
it("loads saves and reloads embedded settings on the launch instance",async()=>{
 render(<I18nProvider><EmbeddedLocalModelSettings t={t} locale="fr"/></I18nProvider>);
 await waitFor(()=>expect(invoke).toHaveBeenCalledWith("get_embedded_runtime",{port:3880}));
 await waitFor(()=>expect(invoke).toHaveBeenCalledWith("get_embedded_status",{port:3880}));
 const save=screen.getByRole("button",{name:"settings.embedded_save"});
 await waitFor(()=>expect((save as HTMLButtonElement).disabled).toBe(false));fireEvent.click(save);
 await waitFor(()=>expect(invoke).toHaveBeenCalledWith("set_embedded_runtime",{port:3880,modelId:"synthetic",engineMode:"cpu"}));
 const reload=screen.getByRole("button",{name:"settings.embedded_reload"});
 await waitFor(()=>expect((reload as HTMLButtonElement).disabled).toBe(false));fireEvent.click(reload);
 await waitFor(()=>expect(invoke).toHaveBeenCalledWith("embedded_reload",{port:3880}));
});
it("submits a task to the launch instance",async()=>{
 render(<I18nProvider><CreateTaskDialog open onClose={vi.fn()} sessionId="synthetic" t={t}/></I18nProvider>);
 fireEvent.change(screen.getByRole("textbox"),{target:{value:"Tâche fictive"}});
 fireEvent.click(screen.getByRole("button",{name:"tasks.create_submit"}));
 await waitFor(()=>expect(invoke).toHaveBeenCalledWith("send_message_ack",{message:"Tâche fictive",sessionId:"synthetic",port:3880}));
});
it("uses the same instance for default note calls and HTTP fallbacks",async()=>{
 await listNotes();expect(invoke).toHaveBeenCalledWith("get_notes",{port:3880});
 expect(e2eDaemonHttpUrl("/api/status")).toBe("http://127.0.0.1:3880/api/status");
});
