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

const settings = read('src/lib/components/SettingsTab.svelte');
const remote = read('src/lib/components/RemoteAccessSettings.svelte');

test('Settings has a desktop-only Remote access section', () => {
  assert.match(settings, /import RemoteAccessSettings from '\.\/RemoteAccessSettings\.svelte'/);
  assert.match(settings, /\{#if isTauri\(\)\}<button[^\n]*activeSection = 'remote'\}>Remote access<\/button>\{\/if\}/);
  assert.match(settings, /\{#if isTauri\(\)\}<div hidden=\{activeSection !== 'remote'\}><RemoteAccessSettings \/><\/div>\{\/if\}/);
});

test('LAN toggle saves through set_remote_access and is a pressed-state button', () => {
  assert.match(remote, /aria-label="LAN access"/);
  assert.match(remote, /aria-pressed=\{remote\.lanEnabled\}/);
  assert.match(remote, /save\(\{ lanEnabled: !remote!\.lanEnabled \}\)/);
  assert.match(api, /invoke<RemoteAccess>\('set_remote_access', \{ remote \}\)/);
});

test('LAN on shows the plain-ws warning in amber', () => {
  const lanBranch = remote.slice(remote.indexOf('{#if remote.lanEnabled}'));
  assert.ok(lanBranch.includes('class="warning"'));
  assert.ok(lanBranch.includes('Not encrypted.'));
  assert.ok(lanBranch.includes('plain <code>ws://</code>'));
  assert.match(remote, /\.warning \{[^}]*var\(--accent-amber\)/);
  assert.match(remote, /role="alert"/);
});

test('remote access settings do not promise tailscale works', () => {
  assert.doesNotMatch(remote, /tailscale|ts\.net/i);
});
