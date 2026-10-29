// Checks that every IPC contract string has both a producer and a consumer:
// Tauri events, Tauri commands, and WebSocket client requests.
// A name on only one side is dead surface (or a broken feature).
// Also checks that the c9p/1 engine schema and its TS types have not drifted.
import assert from 'node:assert/strict';
import { readFileSync, readdirSync, statSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';
import { generate, SCHEMA_PATH, TYPES_PATH } from './gen-c9p-types.mjs';

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

// c9p/1: docs/api/c9p-1.json comes from the engine's serde types (the Rust
// test `schema_file_is_current` fails when it is stale), and src/lib/c9p-1.ts
// comes from that file.
const c9pSchema = JSON.parse(readFileSync(SCHEMA_PATH, 'utf8'));
const engineApi = readFileSync(join(root, 'src-tauri/src/engine/api.rs'), 'utf8');
const wire = readFileSync(join(root, 'src-tauri/src/engine_proto/wire.rs'), 'utf8');

test('c9p/1 TS types are generated from the current schema', () => {
  assert.equal(
    readFileSync(TYPES_PATH, 'utf8'),
    generate(c9pSchema),
    'src/lib/c9p-1.ts is stale: run `node scripts/gen-c9p-types.mjs`'
  );
});

test('c9p/1 schema lists exactly the methods the engine serves', () => {
  const list = engineApi.match(/pub const METHODS: &\[&str\] = &\[([^\]]*)\]/);
  assert.ok(list, 'METHODS not found in engine/api.rs');
  const methods = matches(list[1], /"([a-z._]+)"/g);
  assert.deepEqual(sorted(Object.keys(c9pSchema.methods)), sorted(methods));
  const dispatch = engineApi.match(/async fn dispatch\([\s\S]*?\n\}/);
  assert.ok(dispatch, 'dispatch not found in engine/api.rs');
  const served = matches(dispatch[0], /^\s*"([a-z._]+)" =>/gm);
  served.add('hello'); // handled before dispatch
  // Hook-class methods (C1a-21): `task.done` and `agent.handoff` by name,
  // every `hook.*` in HOOK_METHODS.
  const hookClass = engineApi.match(/async fn hook_class\([\s\S]*?\n\}/);
  assert.ok(hookClass, 'hook_class not found in engine/api.rs');
  for (const m of matches(hookClass[0], /^\s*"([a-z._]+)" =>/gm)) served.add(m);
  const hooks = wire.match(/pub const HOOK_METHODS: &\[&str\] = &\[([^\]]*)\]/);
  assert.ok(hooks, 'HOOK_METHODS not found in engine_proto/wire.rs');
  for (const m of matches(hooks[1], /"([a-z._]+)"/g)) served.add(m);
  assert.deepEqual(sorted(served), sorted(methods));
});

test('every c9p/1 $ref resolves', () => {
  const refs = matches(JSON.stringify(c9pSchema), /"\$ref":"#\/\$defs\/([A-Za-z0-9]+)"/g);
  assert.ok(refs.size > 0);
  for (const name of refs) assert.ok(name in c9pSchema.$defs, `dangling $ref ${name}`);
  for (const part of ['request', 'response', 'event']) assert.ok(c9pSchema.frames[part], part);
});
