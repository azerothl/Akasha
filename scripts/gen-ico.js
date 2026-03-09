#!/usr/bin/env node
/**
 * Generates a minimal 32x32 icon.ico for Tauri (single 32bpp image).
 * Run from repo root: node scripts/gen-ico.js
 * Output: apps/akasha-ui/src-tauri/icons/icon.ico
 */
const fs = require('fs');
const path = require('path');

const outDir = path.join(__dirname, '..', 'apps', 'akasha-ui', 'src-tauri', 'icons');
const outPath = path.join(outDir, 'icon.ico');

const W = 32;
const H = 32;
const BPP = 32;
const BMP_HEADER = 40;
const PIXELS = W * H * (BPP / 8);
const IMAGE_SIZE = BMP_HEADER + PIXELS;
const OFFSET = 6 + 16; // ICONDIR + ICONDIRENTRY

// ICO: ICONDIR (6) + ICONDIRENTRY (16) + BMP
const buf = Buffer.alloc(6 + 16 + IMAGE_SIZE);
let o = 0;

// ICONDIR
buf.writeUInt16LE(0, o); o += 2;   // reserved
buf.writeUInt16LE(1, o); o += 2;   // type 1 = ICO
buf.writeUInt16LE(1, o); o += 2;   // number of images

// ICONDIRENTRY
buf.writeUInt8(W === 256 ? 0 : W, o); o += 1;
buf.writeUInt8(H === 256 ? 0 : H, o); o += 1;
buf.writeUInt8(0, o); o += 1;      // color count 0 = no palette
buf.writeUInt8(0, o); o += 1;      // reserved
buf.writeUInt16LE(1, o); o += 2;   // color planes
buf.writeUInt16LE(BPP, o); o += 2; // bits per pixel
buf.writeUInt32LE(IMAGE_SIZE, o); o += 4;
buf.writeUInt32LE(OFFSET, o); o += 4;

// BITMAPINFOHEADER (40 bytes)
buf.writeUInt32LE(40, o); o += 4;   // header size
buf.writeInt32LE(W, o); o += 4;
buf.writeInt32LE(H * 2, o); o += 4; // height * 2 for ICO (includes AND mask height)
buf.writeUInt16LE(1, o); o += 2;   // planes
buf.writeUInt16LE(BPP, o); o += 2;
buf.writeUInt32LE(0, o); o += 4;    // compression
buf.writeUInt32LE(PIXELS, o); o += 4;
buf.writeInt32LE(0, o); o += 4;
buf.writeInt32LE(0, o); o += 4;
buf.writeUInt32LE(0, o); o += 4;
buf.writeUInt32LE(0, o); o += 4;

// Pixel data (32x32 BGRA, bottom-up) - solid blue
for (let y = H - 1; y >= 0; y--) {
  for (let x = 0; x < W; x++) {
    buf.writeUInt8(0xeb, o); o += 1; // B
    buf.writeUInt8(0x63, o); o += 1; // G
    buf.writeUInt8(0x25, o); o += 1; // R
    buf.writeUInt8(0xff, o); o += 1; // A
  }
}

fs.mkdirSync(outDir, { recursive: true });
fs.writeFileSync(outPath, buf);
console.log('Written', outPath);
