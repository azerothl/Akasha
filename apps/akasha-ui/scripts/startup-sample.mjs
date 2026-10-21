import { readFile } from 'node:fs/promises';

/** A file exists before the native probe finishes write_all; only a complete row is ready. */
export async function waitForStartupSample(output, {timeoutMs=40_000, pollMs=100}={}) {
  const deadline=Date.now()+timeoutMs;
  while (Date.now()<deadline) {
    try {
      const row=JSON.parse(await readFile(output,'utf8'));
      if (row?.metric==='composer_focus_ready' && Number.isFinite(row.native_ms) && row.native_ms>=0
          && row.isolated_profile===true && row.focused===true && row.visible===true) return row;
    } catch (error) {
      if (!(error instanceof SyntaxError) && error.code!=='ENOENT') throw error;
    }
    await new Promise(resolve=>setTimeout(resolve,pollMs));
  }
  throw new Error(`No complete interactive-composer sample: ${output}`);
}
