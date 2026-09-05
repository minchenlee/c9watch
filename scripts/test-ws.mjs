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
