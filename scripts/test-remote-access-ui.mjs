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

test('MOBILE modal shows a pairing code and its QR carries it in the fragment', () => {
  assert.match(modal, /createPairingCode\(\)/);
  assert.match(modal, /pageUrl = `http:\/\/\$\{info\.localIp\}:\$\{info\.port\}\/#pair=\$\{code\}`/);
  assert.match(modal, /class="pair-code"/);
  assert.match(modal, /onclick=\{newCode\}[^>]*>New code</);
  assert.match(modal, /Expires in/);
  assert.doesNotMatch(modal, /info\.token|copyToken/);
});

test('Settings can forget every paired device', () => {
  assert.match(remote, /aria-label="Forget paired devices"/);
  assert.match(remote, /await forgetPairedClients\(\)/);
  assert.match(api, /invoke<void>\('forget_paired_clients'\)/);
});

const connection = read('src/lib/components/ConnectionScreen.svelte');

test('the connection screen pairs with a code and reads it from the fragment', () => {
  assert.match(connection, /Enter pairing code/);
  assert.match(connection, /hash\.get\('pair'\)/);
  assert.match(connection, /history\.replaceState/);
  assert.match(connection, /Could not connect\. Check that remote access is on and the code has not expired\./);
  assert.doesNotMatch(connection, /params\.get\('(token|wsUrl)'\)/);
  assert.match(connection, /Pair again/);
});

test('a pairing code never costs a stored token for the same host', () => {
  const connect = connection.slice(connection.indexOf('async function doConnect'), connection.indexOf('function connectWithCode'));
  // The stored token is tried before the code, and only for its own host.
  assert.match(connect, /'code' in auth && stored\?\.host === host/);
  assert.ok(connect.indexOf('{ token: stored.token }') < connect.indexOf('attempt(host, auth)'));
  // A failed first session list is not reported as a pairing failure.
  assert.match(connection, /result === 'load' \? LOAD_ERROR : connectError/);
});

test('connecting to another host explains why it cannot work from this page', () => {
  const advanced = connection.slice(connection.indexOf('function connectAdvanced'), connection.indexOf('function show('));
  assert.match(advanced, /host === pageHost\(\) \? CONNECT_ERROR : otherHostError\(host\)/);
  assert.match(connection, /can only connect to the desktop that served it/);
  assert.match(connection, /Open http:\/\/\$\{host\}\/ on this device instead\./);
});

test('conversation and open capabilities are separate toggles that save through set_remote_access', () => {
  assert.match(remote, /aria-label="Conversations over remote"/);
  assert.match(remote, /aria-pressed=\{remote\.allowConversation\}/);
  assert.match(remote, /save\(\{ allowConversation: !remote!\.allowConversation \}\)/);
  assert.match(remote, /aria-label="Open terminal from remote"/);
  assert.match(remote, /aria-pressed=\{remote\.allowOpen\}/);
  assert.match(remote, /save\(\{ allowOpen: !remote!\.allowOpen \}\)/);
});

const overlay = read('src/lib/components/ExpandedCardOverlay.svelte');
const page = read('src/routes/(app)/+page.svelte');

test('the overlay shows a calm off state when conversations are off', () => {
  const off = overlay.slice(overlay.indexOf('{:else if !conversation && conversationsOff}'));
  assert.ok(off.indexOf('Conversations are off for remote devices.') > 0);
  assert.ok(off.indexOf('Turn on in desktop Settings → Remote access.') > 0);
  assert.ok(off.indexOf('Conversations are off') < off.indexOf('Could not load conversation'),
    'the off state comes before the error state');
});

test('a refused stop shows its reason in a toast', () => {
  const stop = page.slice(page.indexOf('async function handleStop'), page.indexOf('async function handleOpen'));
  assert.match(stop, /showInAppNotification\('Could not stop session', errorText\(error\)\)/);
});
