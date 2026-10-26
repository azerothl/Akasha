// @vitest-environment jsdom
import {cleanup,render,screen} from "@testing-library/react";
import {afterEach,expect,it,vi} from "vitest";
import {OnboardingWizard} from "./OnboardingWizard";
afterEach(()=>{cleanup();localStorage.clear();});
it("rechecks consumer edition after daemon recovery instead of completing the legacy setup",async()=>{
 let available=false;
 const fetchEndpoint=vi.fn(async(path:string)=>{
  if(!available)throw new Error("daemon starting");
  return{ok:true,status:200,text:JSON.stringify(path==="/api/privacy"?{consumer_beta_build:true,memory:{state:"encrypted",encrypted_at_rest:true}}:{models:[]})};
 });
 const props={locale:"fr" as const,daemonOk:false,fetchEndpoint,onComplete:vi.fn(),t:(key:string)=>key};
 const view=render(<OnboardingWizard {...props}/>);
 expect(await screen.findByText("Préparation du démarrage. En attente d’Akasha…")).toBeDefined();
 expect(localStorage.getItem("akasha_setup_wizard_done")).toBeNull();
 available=true;view.rerender(<OnboardingWizard {...props} daemonOk/>);
 expect(await screen.findByRole("heading",{name:"Préparer Akasha"})).toBeDefined();
 expect(localStorage.getItem("akasha_setup_wizard_done")).toBeNull();
});
