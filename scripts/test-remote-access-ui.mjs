// Checks the remote-access UI: with LAN off the MOBILE modal says so
// instead of showing a QR, and no copy promises that tailscale works.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';

const read = (path) => readFileSync(new URL(`../${path}`, import.meta.url), 'utf8');
const modal = read('src/lib/components/QRCodeModal.svelte');
const api = read('src/lib/api.ts');

test('ServerInfo carries the bind mode', () => {
  assert.match(api, /lanEnabled: boolean;/);
  assert.match(api, /bind: 'loopback' \| 'lan';/);
});

test('MOBILE modal shows the remote-off text instead of a QR when LAN is off', () => {
  assert.match(modal, /\{:else if !info\.lanEnabled\}/);
  assert.match(modal, /Remote access is off\. This Mac only accepts connections from itself\./);
  assert.match(modal, /if \(!info\.lanEnabled\) return;/, 'no QR is rendered for a loopback URL');
  const offBranch = modal.slice(modal.indexOf('{:else if !info.lanEnabled}'));
  assert.ok(offBranch.indexOf('Remote access is off') < offBranch.indexOf('qr-image'));
});

test('no remote-access copy promises tailscale works', () => {
  assert.doesNotMatch(modal, /tailscale/i);
});
