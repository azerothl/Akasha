#!/usr/bin/env node
/**
 * Generates a minimal 32x32 icon.ico for Tauri (single 32bpp image with AND mask).
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
// AND mask: 1 bpp, rows padded to 4-byte boundary
const AND_MASK_ROW_BYTES = Math.ceil(W / 32) * 4; // = 4 bytes for W=32
const AND_MASK = H * AND_MASK_ROW_BYTES;           // = 128 bytes, all zeros (fully opaque)
const IMAGE_SIZE = BMP_HEADER + PIXELS + AND_MASK;
const OFFSET = 6 + 16; // ICONDIR + ICONDIRENTRY

// ICO: ICONDIR (6) + ICONDIRENTRY (16) + BMP header + pixel data + AND mask
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
buf.writeInt32LE(H * 2, o); o += 4; // height * 2 for ICO (XOR height + AND mask height)
buf.writeUInt16LE(1, o); o += 2;    // planes
buf.writeUInt16LE(BPP, o); o += 2;
buf.writeUInt32LE(0, o); o += 4;    // compression (BI_RGB)
buf.writeUInt32LE(PIXELS, o); o += 4; // size of XOR pixel data only (standard for BI_RGB in ICO)
buf.writeInt32LE(0, o); o += 4;
buf.writeInt32LE(0, o); o += 4;
buf.writeUInt32LE(0, o); o += 4;
buf.writeUInt32LE(0, o); o += 4;

// XOR pixel data (32x32 BGRA, bottom-up) - solid blue
for (let y = H - 1; y >= 0; y--) {
  for (let x = 0; x < W; x++) {
    buf.writeUInt8(0xeb, o); o += 1; // B
    buf.writeUInt8(0x63, o); o += 1; // G
    buf.writeUInt8(0x25, o); o += 1; // R
    buf.writeUInt8(0xff, o); o += 1; // A
  }
}

// AND mask (1 bpp, bottom-up, padded to 4-byte rows) — all zeros = fully opaque
// Buffer is already zero-initialized so nothing to write; just advance the offset.
o += AND_MASK;

fs.mkdirSync(outDir, { recursive: true });
fs.writeFileSync(outPath, buf);
console.log('Written', outPath);
