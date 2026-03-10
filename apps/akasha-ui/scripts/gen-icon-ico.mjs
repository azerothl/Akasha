#!/usr/bin/env node
/**
 * Generate icon.ico from icon.png for Tauri (Windows icon).
 * Run from apps/akasha-ui: npm run gen-icon-ico
 * Requires: png-to-ico
 */
import fs from 'fs';
import path from 'path';
import { fileURLToPath } from 'url';
import pngToIco from 'png-to-ico';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const root = path.join(__dirname, '..');
const iconsDir = path.join(root, 'src-tauri', 'icons');
const pngPath = path.join(iconsDir, 'icon.png');
const icoPath = path.join(iconsDir, 'icon.ico');

if (!fs.existsSync(pngPath)) {
  console.error('icon.png not found at', pngPath);
  process.exit(1);
}

try {
  const buf = await pngToIco(pngPath);
  fs.writeFileSync(icoPath, buf);
  console.log('Written', icoPath);
} catch (err) {
  console.error(err);
  process.exit(1);
}
