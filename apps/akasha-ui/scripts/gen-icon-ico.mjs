#!/usr/bin/env node
/**
 * Generate icon.ico + icon.icns from icon.png for Tauri bundles.
 *
 * Run from apps/akasha-ui:
 *   npm run gen-icons
 *
 * Source:  src-tauri/icons/icon.png
 * Outputs: src-tauri/icons/icon.ico, src-tauri/icons/icon.icns
 *
 * Uses @ctjs/png2icons (platform-independent, no native deps).
 */
import fs from 'fs';
import path from 'path';
import { fileURLToPath } from 'url';
import { createICNS, createICO, setLogger, BICUBIC } from '@ctjs/png2icons';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const root = path.join(__dirname, '..');
const iconsDir = path.join(root, 'src-tauri', 'icons');
const pngPath = path.join(iconsDir, 'icon.png');
const icoPath = path.join(iconsDir, 'icon.ico');
const icnsPath = path.join(iconsDir, 'icon.icns');

if (!fs.existsSync(pngPath)) {
  console.error('icon.png not found at', pngPath);
  process.exit(1);
}

const input = fs.readFileSync(pngPath);

setLogger(console.log);
const icns = createICNS(input, BICUBIC, 0);
if (!icns) {
  console.error('Failed to generate icon.icns');
  process.exit(1);
}
fs.writeFileSync(icnsPath, icns);
console.log('Written', icnsPath);

// forWinExe=true => mix BMP/PNG for better Windows compatibility
const ico = createICO(input, BICUBIC, 0, false, true);
if (!ico) {
  console.error('Failed to generate icon.ico');
  process.exit(1);
}
fs.writeFileSync(icoPath, ico);
console.log('Written', icoPath);
