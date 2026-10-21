import assert from 'node:assert/strict';
import { mkdtemp, mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';
import test from 'node:test';
const script = fileURLToPath(new URL('./benchmark-native-startup.mjs', import.meta.url));
for (const leftover of ['before-fresh-profile-0.json', 'after-warm-profile-2.json', 'before-fresh-0', '.benchmark-run']) {
  test(`rejects reused benchmark directory containing ${leftover}`, async () => {
    const destination = await mkdtemp(path.join(tmpdir(), 'akasha-benchmark-guard-'));
    try {
      const target = path.join(destination, leftover);
      if (leftover === 'before-fresh-0') await mkdir(target);
      else await writeFile(target, 'previous observation');
      // This executable must never be launched; validation happens before the HTTP server or spawn.
      const result = spawnSync(process.execPath, [script, 'missing-native.exe', '.', '.', destination], {encoding:'utf8', timeout:5000});
      assert.equal(result.status, 1);
      assert.match(result.stderr, /Benchmark output directory must be empty/);
      assert.doesNotMatch(result.stderr, /ENOENT|EADDRINUSE/);
      if (leftover !== 'before-fresh-0') assert.equal(await readFile(target,'utf8'), 'previous observation');
    } finally {
      await rm(destination, {recursive:true, force:true});
    }
  });
}

const { waitForStartupSample } = await import('./startup-sample.mjs');
test('waits through missing, empty and partially written sample files', async () => {
  const destination=await mkdtemp(path.join(tmpdir(),'akasha-benchmark-write-'));
  const output=path.join(destination,'sample.json');
  const row={metric:'composer_focus_ready',native_ms:123,isolated_profile:true,focused:true,visible:true};
  const waiting=waitForStartupSample(output,{timeoutMs:2000,pollMs:5});
  try {
    await new Promise(r=>setTimeout(r,20));
    await writeFile(output,'');
    await new Promise(r=>setTimeout(r,20));
    await writeFile(output,'{"metric":');
    await new Promise(r=>setTimeout(r,20));
    await writeFile(output,JSON.stringify(row));
    assert.deepEqual(await waiting,row);
  } finally { await rm(destination,{recursive:true,force:true}); }
});
test('rejects a sample that never becomes complete', async () => {
  const destination=await mkdtemp(path.join(tmpdir(),'akasha-benchmark-incomplete-'));
  try {
    const output=path.join(destination,'sample.json');
    await writeFile(output,'{}');
    await assert.rejects(waitForStartupSample(output,{timeoutMs:30,pollMs:5}),/No complete interactive-composer sample/);
  } finally { await rm(destination,{recursive:true,force:true}); }
});
