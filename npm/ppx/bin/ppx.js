#!/usr/bin/env node

const fs = require('fs');
const path = require('path');
const cp = require('child_process');

const binary = path.join(__dirname, '..', 'vendor', process.platform === 'win32' ? 'ppx.exe' : 'ppx');
if (!fs.existsSync(binary)) {
  console.error('[ppx] The native binary was not downloaded during installation.');
  console.error('[ppx] Reinstall with network access, or build ppx from source:');
  console.error('      cargo install --git https://github.com/PolderLabsVOF/ppexchanger --locked');
  process.exit(1);
}

const result = cp.spawnSync(binary, process.argv.slice(2), { stdio: 'inherit' });
if (result.error) {
  console.error(`[ppx] Could not start native binary: ${result.error.message}`);
  process.exit(1);
}
process.exit(result.status === null ? 1 : result.status);
