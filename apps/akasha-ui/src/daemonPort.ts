/** Resolved once before mounting React: all commands share the launch port. */
export let DAEMON_PORT=Number(import.meta.env.VITE_AKASHA_DAEMON_PORT)||3876;
export async function initializeDaemonPort(native:boolean,read:()=>Promise<{port?:number}>):Promise<void>{
  if(!native)return;
  const config=await read();
  // Older transports and browser fixtures can omit this optional capability.
  if(config.port===undefined)return;
  if(!Number.isInteger(config.port)||config.port<1||config.port>65535)throw new Error("Invalid launch port");
  DAEMON_PORT=config.port;
}
