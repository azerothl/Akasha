import {afterEach,expect,it} from "vitest";
import {DAEMON_PORT,initializeDaemonPort} from "./daemonPort";
const initial=DAEMON_PORT;
afterEach(()=>initializeDaemonPort(true,async()=>({port:initial})));
it("awaits the native launch port before any caller can use it",async()=>{
 let resolve!:(value:{port:number})=>void;
 const loading=initializeDaemonPort(true,()=>new Promise(done=>{resolve=done;}));
 expect(DAEMON_PORT).toBe(initial);resolve({port:3880});await loading;expect(DAEMON_PORT).toBe(3880);
});
it("keeps web and legacy transports on the compiled port",async()=>{
 await initializeDaemonPort(false,async()=>{throw Error("must not call native");});
 await initializeDaemonPort(true,async()=>({}));expect(DAEMON_PORT).toBe(initial);
});
it("rejects invalid runtime ports and native failures",async()=>{
 await expect(initializeDaemonPort(true,async()=>({port:0}))).rejects.toThrow();
 await expect(initializeDaemonPort(true,async()=>{throw Error("offline");})).rejects.toThrow();
 expect(DAEMON_PORT).toBe(initial);
});
