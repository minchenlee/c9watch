// Checks that every IPC contract string has both a producer and a consumer:
// Tauri events, Tauri commands, and WebSocket client requests.
// A name on only one side is dead surface (or a broken feature).
import assert from 'node:assert/strict';
import { readFileSync, readdirSync, statSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';

const root = fileURLToPath(new URL('..', import.meta.url));

function files(dir, exts) {
  const out = [];
  for (const name of readdirSync(dir)) {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) out.push(...files(path, exts));
    else if (exts.some((ext) => name.endsWith(ext))) out.push(path);
  }
  return out;
}

const frontend = files(join(root, 'src'), ['.ts', '.svelte'])
  .map((path) => readFileSync(path, 'utf8'))
  .join('\n');
const backend = files(join(root, 'src-tauri/src'), ['.rs'])
  .map((path) => readFileSync(path, 'utf8'))
  .join('\n');
const libRs = readFileSync(join(root, 'src-tauri/src/lib.rs'), 'utf8');
const webServer = readFileSync(join(root, 'src-tauri/src/web_server.rs'), 'utf8');

const matches = (text, re) => new Set([...text.matchAll(re)].map((m) => m[1]));
const sorted = (set) => [...set].sort();

test('every Tauri event is both emitted and listened to', () => {
  const listened = matches(frontend, /\blisten(?:<.*?>)?\(\s*'([a-z][a-z0-9-]*)'/g);
  const emitted = matches(backend, /\.emit\(\s*"([a-z][a-z0-9-]*)"/g);
  assert.deepEqual(sorted(listened), sorted(emitted));
});

test('every registered Tauri command is invoked, and every invoke is registered', () => {
  const handler = libRs.match(/generate_handler!\[([\s\S]*?)\]/);
  assert.ok(handler, 'generate_handler! not found in lib.rs');
  const registered = new Set(
    handler[1]
      .split(',')
      .map((entry) => entry.trim().split('::').pop())
      .filter(Boolean)
  );
  const invoked = matches(frontend, /\binvoke(?:<.*?>)?\(\s*'([a-z_0-9]+)'/g);
  for (const name of invoked) assert.ok(registered.has(name), `invoked but not registered: ${name}`);
  for (const name of registered) assert.ok(invoked.has(name), `registered but never invoked: ${name}`);
});

// Requests the server still parses only to answer "removed"; the frontend
// must not send them.
const REMOVED_WS_REQUESTS = new Set(['stopSession']);

test('every WebSocket client request is sent by the frontend, and vice versa', () => {
  const clientEnum = webServer.match(/enum ClientMsg \{([\s\S]*?)\n\}/);
  assert.ok(clientEnum, 'ClientMsg not found in web_server.rs');
  const handled = matches(clientEnum[1], /#\[serde\(rename = "([A-Za-z.]+)"\)\]\s*[A-Z]/g);
  const requested = matches(frontend, /wsClient\.request(?:<.*?>)?\(\s*'([A-Za-z.]+)'/g);
  for (const name of REMOVED_WS_REQUESTS) {
    assert.ok(handled.has(name), `removed request no longer parsed: ${name}`);
    handled.delete(name);
    assert.ok(!frontend.includes(`'${name}'`), `frontend still sends removed request: ${name}`);
  }
  assert.ok(handled.has('session.stop') && requested.has('session.stop'));
  assert.deepEqual(sorted(requested), sorted(handled));
});

test('stop_session is invoked with a session key, not a bare pid', () => {
  const call = frontend.match(/\binvoke(?:<.*?>)?\(\s*'stop_session',\s*\{([^}]*)\}/);
  assert.ok(call, "invoke('stop_session', {...}) not found");
  const keys = call[1].split(',').map((entry) => entry.split(':')[0].trim());
  assert.ok(keys.includes('sessionKey'), `stop_session args: ${keys.join(', ')}`);
  assert.match(libRs, /async fn stop_session\([^)]*session_key: String/);
});
