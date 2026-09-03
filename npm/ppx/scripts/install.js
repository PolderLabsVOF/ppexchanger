#!/usr/bin/env node

// Download the matching single-file ppx release during npm install. This
// deliberately uses only Node's standard library so the package stays small
// and auditable.
const fs = require('fs');
const os = require('os');
const path = require('path');
const https = require('https');
const cp = require('child_process');

const pkg = require('../package.json');
// The npm wrapper can ship a small packaging fix without requiring a new
// native release. PPX_VERSION remains an explicit override for pinning.
const version = String(process.env.PPX_VERSION || pkg.nativeVersion || pkg.version).replace(/^v/, '');
const targets = {
  'linux:x64': 'x86_64-unknown-linux-gnu',
  'linux:arm64': 'aarch64-unknown-linux-gnu',
  'darwin:x64': 'x86_64-apple-darwin',
  'darwin:arm64': 'aarch64-apple-darwin',
  'win32:x64': 'x86_64-pc-windows-msvc'
};
const target = targets[`${process.platform}:${process.arch}`];
const vendor = path.join(__dirname, '..', 'vendor');
const binaryName = process.platform === 'win32' ? 'ppx.exe' : 'ppx';
const binary = path.join(vendor, binaryName);

if (fs.existsSync(binary)) process.exit(0);
if (!target) {
  console.warn(`[ppx] No prebuilt binary for ${process.platform}/${process.arch}.`);
  console.warn('[ppx] Install Rust and run: cargo install --git https://github.com/PolderLabsVOF/ppexchanger --locked');
  process.exit(0);
}

const asset = `ppexchanger-${version}-${target}.tar.gz`;
const url = `https://github.com/PolderLabsVOF/ppexchanger/releases/download/v${version}/${asset}`;
const temp = fs.mkdtempSync(path.join(os.tmpdir(), 'ppx-npm-'));
const archive = path.join(temp, asset);

function download(location, redirects = 0) {
  return new Promise((resolve, reject) => {
    if (redirects > 5) return reject(new Error('too many redirects'));
    https.get(location, { headers: { 'User-Agent': 'ppx-npm-installer' } }, (response) => {
      if (response.statusCode >= 300 && response.statusCode < 400 && response.headers.location) {
        response.resume();
        return download(response.headers.location, redirects + 1).then(resolve, reject);
      }
      if (response.statusCode !== 200) {
        response.resume();
        return reject(new Error(`GitHub returned HTTP ${response.statusCode}`));
      }
      const out = fs.createWriteStream(archive);
      response.pipe(out);
      out.on('finish', () => out.close(resolve));
      out.on('error', reject);
    }).on('error', reject);
  });
}

async function main() {
  try {
    await download(url);
    const extract = path.join(temp, 'extract');
    fs.mkdirSync(extract);
    cp.execFileSync('tar', ['-xzf', archive, '-C', extract], { stdio: 'ignore' });
    const unpacked = path.join(extract, 'bin', binaryName);
    if (!fs.existsSync(unpacked)) throw new Error('release did not contain ppx');
    fs.mkdirSync(vendor, { recursive: true });
    fs.copyFileSync(unpacked, binary);
    if (process.platform !== 'win32') fs.chmodSync(binary, 0o755);
    console.log(`[ppx] installed ppx ${version} for ${target}`);
  } catch (error) {
    console.warn(`[ppx] Could not download the native binary: ${error.message}`);
    console.warn('[ppx] The package is installed, but ppx needs a source build:');
    console.warn('      cargo install --git https://github.com/PolderLabsVOF/ppexchanger --locked');
    // Keep npm install usable in offline environments; the launcher gives the
    // same actionable message if the command is invoked before a build.
  } finally {
    fs.rmSync(temp, { recursive: true, force: true });
  }
}

main();
