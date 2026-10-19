// node scripts/benchmark-native-startup.mjs <debug-exe> <before-dist> <after-dist> <output-dir>
// Same native shell, URL, preferences and 800x600 viewport for both production frontends.
import fs from 'node:fs/promises';
import path from 'node:path';
import http from 'node:http';
import { spawn } from 'node:child_process';
const args = process.argv.slice(2);
if (args.length !== 4) throw new Error('Expected exe, before dist, after dist, output dir');
const [exe, before, after, destination] = args.map(p => path.resolve(p));
await fs.mkdir(destination, {recursive:true});
// Reject old samples and profiles before launching anything: retries must use a new directory.
if ((await fs.readdir(destination)).length !== 0) {
  throw new Error('Benchmark output directory must be empty; choose a new directory for each run');
}
// Reserve this directory so concurrent invocations cannot record into the same sample files.
await fs.writeFile(path.join(destination, '.benchmark-run'), '', {flag:'wx'});
let root = before;
const server = http.createServer(async (req, res) => {
  try {
    if (req.url === '/__benchmark-seed.js') {
      res.setHeader('Content-Type','application/javascript');
      res.end("localStorage.setItem('akasha_setup_wizard_done','1');localStorage.setItem('akasha_ui_mode','simple');localStorage.setItem('akasha_locale','fr');localStorage.setItem('akasha_task_sidebar_open','0');");
      return;
    }
    const relative = decodeURIComponent(new URL(req.url, 'http://localhost').pathname).replace(/^\/+/, '') || 'index.html';
    const target = path.resolve(root, relative);
    if (!target.startsWith(root + path.sep)) { res.writeHead(403).end(); return; }
    let data = await fs.readFile(target);
    if (relative === 'index.html') data = Buffer.from(data.toString().replace('<head>', '<head><script src="/__benchmark-seed.js"></script>'));
    res.setHeader('Content-Type', ({'.html':'text/html','.js':'application/javascript','.css':'text/css','.json':'application/json','.png':'image/png','.svg':'image/svg+xml'})[path.extname(target)] || 'application/octet-stream');
    // Warm runs reuse the same profile and HTTP cache; cold-profile runs use a new profile.
    res.setHeader('Cache-Control','max-age=3600'); res.end(data);
  } catch { res.writeHead(404).end(); }
});
await new Promise((resolve,reject)=>{server.once('error',reject);server.listen(5297,'127.0.0.1',resolve);});
const observations=[];
try {
  for (const [variant, directory] of [['before',before],['after',after]]) {
    root=directory;
    const profile=path.join(destination,variant+'-fresh-2');
    for (const cache of ['fresh-profile','warm-profile']) for(let i=0;i<3;i++) {
      const output=path.join(destination,`${variant}-${cache}-${i}.json`);
      const dataProfile=cache==='fresh-profile'?path.join(destination,`${variant}-fresh-${i}`):profile;
      const child=spawn(exe,[],{env:{...process.env,AKASHA_UI_STARTUP_PROBE:output,AKASHA_UI_STARTUP_PROFILE:dataProfile},stdio:'ignore'});
      try {
        let found=false;
        for(let tick=0;tick<400;tick++){try{await fs.access(output);found=true;break;}catch{}await new Promise(r=>setTimeout(r,100));}
        if(!found) throw new Error(`No interactive-composer sample: ${variant}/${cache}/${i}`);
        const row=JSON.parse(await fs.readFile(output,'utf8'));
        if(!row.isolated_profile)throw new Error('WebView profile isolation not confirmed');
        observations.push({variant,cache,run:i+1,...row});
        await fs.writeFile(path.join(destination,'observations.json'),JSON.stringify(observations,null,2));
        console.log(`${variant} ${cache} ${i+1}: ${row.native_ms.toFixed(1)} ms`);
      } finally {
        if(child.exitCode===null) await Promise.race([new Promise(r=>child.once('exit',r)),new Promise(r=>setTimeout(r,3000))]);
        if(child.exitCode===null)child.kill();
      }
      await new Promise(r=>setTimeout(r,400));
    }
  }
} finally { server.close(); }
