import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import ts from 'typescript';
import { test } from 'node:test';

function client() {
  const exports = {};
  const code = ts.transpileModule(readFileSync(new URL('../src/lib/ws.ts', import.meta.url), 'utf8'), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 }
  }).outputText;
  vm.runInNewContext(code, { exports, console, setTimeout, clearTimeout, WebSocket: { OPEN: 1 } });
  const client = exports.wsClient;
  const sent = [];
  client.ws = { readyState: 1, send: text => sent.push(JSON.parse(text)) };
  return { client, sent, reply: msg => client.handleMessage({ data: JSON.stringify(msg) }) };
}

test('conversations and actions receive their own out-of-order responses', async () => {
  const { client: c, sent, reply } = client();
  const conversation = c.request('getConversation', { sessionId: 'one' });
  const action = c.request('openSession', { pid: 123 });
  reply({ type: 'ok', requestId: sent[1].requestId });
  assert.equal((await action).type, 'ok');
  reply({ type: 'conversation', requestId: sent[0].requestId, data: { sessionId: 'one' } });
  assert.equal((await conversation).sessionId, 'one');
});

test('superseded request rejects without settling the next conversation', async () => {
  const { client: c, sent, reply } = client();
  const first = c.request('getConversation', { sessionId: 'one' });
  const rejected = assert.rejects(first, /superseded/);
  const second = c.request('getConversation', { sessionId: 'two' });
  reply({ type: 'error', requestId: sent[0].requestId, message: 'Conversation request superseded' });
  await rejected;
  reply({ type: 'conversation', requestId: sent[0].requestId, data: { sessionId: 'stale' } });
  reply({ type: 'conversation', requestId: sent[1].requestId, data: { sessionId: 'two' } });
  assert.equal((await second).sessionId, 'two');
});

test('push events do not settle requests and disconnect rejects all pending requests', async () => {
  const { client: c, sent, reply } = client();
  let updates = 0;
  c.on('sessionsUpdated', () => updates++);
  const one = assert.rejects(c.request('getSessions'), /closed/);
  const two = assert.rejects(c.request('getConversation'), /closed/);
  reply({ type: 'sessionsUpdated', data: [] });
  reply({ type: 'conversationProgress', data: { requestId: sent[1].requestId } });
  assert.equal(updates, 1);
  assert.equal(c.pending.size, 2);
  c.rejectPending('Connection closed');
  await Promise.all([one, two]);
  assert.equal(c.pending.size, 0);
});

// ── Handshake: token in the first frame, pairing, reconnect ──────────

function harness({ storage = {} } = {}) {
  const exports = {};
  const code = ts.transpileModule(readFileSync(new URL('../src/lib/ws.ts', import.meta.url), 'utf8'), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 }
  }).outputText;
  const sockets = [];
  class FakeSocket {
    static OPEN = 1;
    constructor(url) {
      this.url = url;
      this.readyState = 0;
      this.sent = [];
      sockets.push(this);
    }
    send(text) { this.sent.push(JSON.parse(text)); }
    close() { this.readyState = 3; }
    open() { this.readyState = 1; this.onopen?.(); }
    reply(msg) { this.onmessage?.({ data: JSON.stringify(msg) }); }
    drop(code = 1006) { this.readyState = 3; this.onclose?.({ code }); }
  }
  const store = { ...storage };
  const localStorage = {
    getItem: (k) => (k in store ? store[k] : null),
    setItem: (k, v) => { store[k] = String(v); },
    removeItem: (k) => { delete store[k]; },
  };
  const timers = [];
  const fakeSetTimeout = (fn, ms) => { const t = { fn, ms, cleared: false }; timers.push(t); return t; };
  const fakeClearTimeout = (t) => { if (t) t.cleared = true; };
  const runTimers = (ms) => {
    for (const t of timers.splice(0)) if (!t.cleared && t.ms === ms) t.fn(); else if (!t.cleared) timers.push(t);
  };
  vm.runInNewContext(code, {
    exports, console: { log() {}, error() {}, warn() {} },
    setTimeout: fakeSetTimeout, clearTimeout: fakeClearTimeout,
    WebSocket: FakeSocket, localStorage,
  });
  return { ws: exports, client: exports.wsClient, sockets, store, timers, runTimers };
}

test('the first frame after open is auth, and the URL carries no token', async () => {
  const { client, sockets } = harness();
  const done = client.connect('10.0.0.2:9210', { token: 'abc' });
  assert.equal(sockets.length, 1);
  assert.equal(sockets[0].url, 'ws://10.0.0.2:9210/ws');
  assert.doesNotMatch(sockets[0].url, /token/);
  sockets[0].open();
  assert.deepEqual(sockets[0].sent, [{ type: 'auth', token: 'abc' }]);
  sockets[0].reply({ type: 'authOk', clientId: 'c1' });
  await done;
  assert.equal(client.isConnected, true);
});

test('connect does not resolve before the ack', async () => {
  const { client, sockets } = harness();
  let settled = false;
  const done = client.connect('h:9210', { token: 'abc' }).then(() => (settled = true));
  sockets[0].open();
  await new Promise((r) => setImmediate(r));
  assert.equal(settled, false);
  assert.equal(client.isConnected, false);
  sockets[0].reply({ type: 'authOk', clientId: 'c1' });
  await done;
  assert.equal(settled, true);
});

test('the connect timeout covers the ack', async () => {
  const { client, sockets, runTimers } = harness();
  const done = assert.rejects(client.connect('h:9210', { token: 'abc' }), /timeout/i);
  sockets[0].open();
  runTimers(5000);
  await done;
  assert.equal(client.isConnected, false);
});

test('4401 stops reconnecting, clears the token and emits authRequired', async () => {
  const { client, sockets, store, timers } = harness({
    storage: { 'c9watch-ws-host': 'h:9210', 'c9watch-client-token': 'abc' },
  });
  let required = 0;
  client.on('authRequired', () => required++);
  const done = client.connect('h:9210', { token: 'abc' });
  sockets[0].open();
  sockets[0].reply({ type: 'authOk', clientId: 'c1' });
  await done;
  sockets[0].drop(4401);
  assert.equal(required, 1);
  assert.equal(timers.filter((t) => !t.cleared && t.ms === 3000).length, 0, 'no reconnect scheduled');
  assert.equal(store['c9watch-client-token'], undefined);
});

test('4401 during connect rejects it and emits authRequired', async () => {
  const { client, sockets } = harness();
  let required = 0;
  client.on('authRequired', () => required++);
  const done = assert.rejects(client.connect('h:9210', { token: 'bad' }));
  sockets[0].open();
  sockets[0].drop(4401);
  await done;
  assert.equal(required, 1);
});

test('a normal drop reconnects and sends auth again', async () => {
  const { client, sockets, runTimers } = harness();
  const done = client.connect('h:9210', { token: 'abc' });
  sockets[0].open();
  sockets[0].reply({ type: 'authOk', clientId: 'c1' });
  await done;
  sockets[0].drop(1012);
  assert.equal(client.isConnected, false);
  runTimers(3000);
  assert.equal(sockets.length, 2);
  sockets[1].open();
  assert.deepEqual(sockets[1].sent, [{ type: 'auth', token: 'abc' }]);
});

test('pairing stores the token, not the code, and reconnects with the token', async () => {
  const { client, sockets, store, runTimers } = harness();
  const done = client.connect('h:9210', { code: 'ABCD2345' });
  sockets[0].open();
  assert.deepEqual(sockets[0].sent, [{ type: 'pair', code: 'ABCD2345' }]);
  sockets[0].reply({ type: 'paired', clientId: 'c1', token: 'tok' });
  await done;
  assert.equal(store['c9watch-ws-host'], 'h:9210');
  assert.equal(store['c9watch-client-token'], 'tok');
  assert.ok(!Object.values(store).includes('ABCD2345'));
  sockets[0].drop(1006);
  runTimers(3000);
  sockets[1].open();
  assert.deepEqual(sockets[1].sent, [{ type: 'auth', token: 'tok' }]);
});

test('a failed pairing does not retry the code', async () => {
  const { client, sockets, timers } = harness();
  const done = assert.rejects(client.connect('h:9210', { code: 'ABCD2345' }));
  sockets[0].open();
  sockets[0].drop(1006);
  await done;
  assert.equal(timers.filter((t) => !t.cleared && t.ms === 3000).length, 0);
});

test('a legacy token URL is removed and flagged for re-pairing', () => {
  const { ws, store } = harness({ storage: { 'c9watch-ws-url': 'ws://h:9210/ws?token=abc' } });
  assert.equal(ws.migrateLegacyWsUrl(), true);
  assert.equal(store['c9watch-ws-url'], undefined);
  assert.equal(ws.migrateLegacyWsUrl(), false);
  assert.equal(ws.getStoredConnection(), null);
});

test('stored connection reads host and token; useWebSocket follows it', () => {
  const { ws } = harness({ storage: { 'c9watch-ws-host': 'h:9210', 'c9watch-client-token': 'abc' } });
  assert.deepEqual({ ...ws.getStoredConnection() }, { host: 'h:9210', token: 'abc' });
  assert.equal(ws.useWebSocket(), true);
});

// ── Remote capabilities (C0h-15) ─────────────────────────────────────

async function signedIn(capabilities) {
  const h = harness();
  const done = h.client.connect('h:9210', { token: 'abc' });
  h.sockets[0].open();
  h.sockets[0].reply({ type: 'authOk', clientId: 'c1', capabilities });
  await done;
  return h;
}

test('a turned-off capability throws before anything is sent', async () => {
  const { ws, client, sockets } = await signedIn({ conversation: false, open: false });
  sockets[0].readyState = 1;
  for (const type of ['getConversation', 'openSession']) {
    const error = await client.request(type, { sessionId: 's' }).then(() => null, (e) => e);
    assert.ok(error instanceof ws.CapabilityDisabledError, type);
    assert.equal(error.capability, type === 'getConversation' ? 'conversation' : 'open');
  }
  assert.deepEqual(sockets[0].sent.map((f) => f.type), ['auth'], 'only the auth frame went out');
  assert.equal(client.allows('conversation'), false);
  assert.equal(client.allows('open'), false);
  // Watching cards stays allowed.
  void client.request('getSessions');
  assert.equal(sockets[0].sent.at(-1).type, 'getSessions');
});

test('capabilities that are on let the request through', async () => {
  const { client, sockets } = await signedIn({ conversation: true, open: true });
  sockets[0].readyState = 1;
  void client.request('getConversation', { sessionId: 's' });
  void client.request('openSession', { addr: 'claudeCode:s' });
  assert.deepEqual(sockets[0].sent.map((f) => f.type), ['auth', 'getConversation', 'openSession']);
});

test('a capabilityDisabled error from the server rejects with CapabilityDisabledError', async () => {
  const { ws, client, sockets } = await signedIn({ conversation: true, open: true });
  sockets[0].readyState = 1;
  const request = client.request('getConversation', { sessionId: 's' });
  const id = sockets[0].sent.at(-1).requestId;
  sockets[0].reply({ type: 'error', requestId: id, code: 'capabilityDisabled', capability: 'conversation', message: 'off' });
  const error = await request.then(() => null, (e) => e);
  assert.ok(error instanceof ws.CapabilityDisabledError);
  assert.equal(error.capability, 'conversation');
});

test('the capabilities from each ack are emitted, so a reconnect can change them', async () => {
  const { client, sockets, runTimers } = await signedIn({ conversation: false, open: false });
  const seen = [];
  client.on('capabilities', (caps) => seen.push(caps));
  sockets[0].drop(1012);
  runTimers(3000);
  sockets[1].open();
  sockets[1].reply({ type: 'authOk', clientId: 'c1', capabilities: { conversation: true, open: false } });
  await new Promise((r) => setImmediate(r));
  assert.equal(seen.length, 1);
  assert.equal(seen[0].conversation, true);
  assert.equal(client.allows('conversation'), true);
  assert.equal(client.allows('open'), false);
});
