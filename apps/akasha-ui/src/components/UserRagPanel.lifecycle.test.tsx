// @vitest-environment jsdom
import {cleanup,fireEvent,render,screen,waitFor} from "@testing-library/react";
import {afterEach,expect,it,vi} from "vitest";
import {UserRagPanel} from "./UserRagPanel";

afterEach(()=>{cleanup();localStorage.clear();});
it("continues observing an import after leaving and remounting the document panel",async()=>{
  const document={id:"pending-source",name:"scan.pdf",mime_type:"application/pdf",added_at:"2026-10-27",index_status:"indexing"};
  let ready=false;let reads=0;
  const fetchEndpoint=vi.fn(async(path:string)=>{
    if(path==="/api/privacy")return {ok:true,status:200,text:JSON.stringify({services:[],search_services:[],memory:{state:"encrypted"}})};
    expect(path).toBe("/api/user-rag/documents");reads++;
    return {ok:true,status:200,text:JSON.stringify({documents:[{...document,index_status:ready?"ready":"indexing"}]})};
  });
  const props={t:(key:string)=>key,locale:"fr",daemonPort:3876,documents:[document],loading:false,error:null,onError:vi.fn(),onRefresh:vi.fn(),fetchEndpoint,readFileAsBase64:vi.fn()};
  const first=render(<UserRagPanel {...props}/>);
  await waitFor(()=>expect(reads).toBe(1));first.unmount();
  render(<UserRagPanel {...props}/>);
  await waitFor(()=>expect(reads).toBe(2));
  fireEvent.click(screen.getByRole("checkbox",{name:"scan.pdf"}));
  const understand=screen.getByRole("button",{name:"Comprendre ces documents"}) as HTMLButtonElement;
  expect(understand.disabled).toBe(true);
  ready=true;
  await waitFor(()=>expect(understand.disabled).toBe(false),{timeout:3000});
  expect(reads).toBeGreaterThanOrEqual(3);
});
