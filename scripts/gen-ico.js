#!/usr/bin/env node
/**
 * ICO for Tauri is generated from icon.png.
 * Run from repo root:
 *   cd apps/akasha-ui && npm run gen-icon-ico
 * Or from apps/akasha-ui: npm run gen-icon-ico
 *
 * Source: apps/akasha-ui/src-tauri/icons/icon.png
 * Output: apps/akasha-ui/src-tauri/icons/icon.ico
 */
const path = require('path');
const { execSync } = require('child_process');
const appDir = path.join(__dirname, '..', 'apps', 'akasha-ui');
execSync('npm run gen-icon-ico', { cwd: appDir, stdio: 'inherit' });
