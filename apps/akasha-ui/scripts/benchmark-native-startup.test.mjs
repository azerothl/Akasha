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
