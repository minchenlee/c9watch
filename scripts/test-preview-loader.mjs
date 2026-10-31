import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import ts from 'typescript';

// Load conversation-loader.ts with $lib and Tauri imports stubbed; the
// preview loader's own dependencies are injected per test.
const read = path => readFileSync(new URL(path, import.meta.url), 'utf8');
const transpile = source => ts.transpileModule(source, {
  compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022 }
}).outputText;
const dataUrl = code => 'data:text/javascript;base64,' + Buffer.from(code).toString('base64');
const stubs = {
  'svelte/store': import.meta.resolve('svelte/store'),
  '$lib/data/platform': dataUrl('export const listen = async () => () => {};'),
  '$lib/api': dataUrl('export const getConversation = () => { throw new Error("not injected"); }; export class CapabilityDisabledError extends Error {}'),
  '$lib/ws': dataUrl('export const isTauri = () => false; export const useWebSocket = () => false; export const wsClient = { on() {} };'),
  '$lib/provider': dataUrl(transpile(read('../src/lib/provider.ts')))
};
const code = transpile(read('../src/lib/data/stores/conversation-loader.ts'))
  .replace(/from '([^']+)'/g, (_, spec) => `from '${stubs[spec] ?? spec}'`);
const { writable, get } = await import(stubs['svelte/store']);
const loader = await import(dataUrl(code));

function harness() {
  const requests = [];
  const deps = {
    getConversation: (sessionId, provider) =>
      new Promise((resolve, reject) => requests.push({ sessionId, provider, resolve, reject })),
    withConversationLoader: (_id, _provider, _kind, task) => task(),
    toolsLoadedFor: writable('stale')
  };
  let requestId = 0;
  let conversation = null;
  const open = entry => {
    const id = ++requestId;
    conversation = null;
    return loader.loadPreviewConversation(entry, () => id === requestId, conv => { conversation = conv; }, deps);
  };
  return { requests, deps, open, conversation: () => conversation };
}

const conv = (sessionId, provider) => ({ sessionId, provider, messages: [] });

test('opening a preview clears toolsLoadedFor and asks for no tool dumps', async () => {
  const h = harness();
  const done = h.open({ sessionId: 'a', provider: 'codex' });
  assert.equal(get(h.deps.toolsLoadedFor), null);
  assert.deepEqual(h.requests.map(r => [r.sessionId, r.provider]), [['a', 'codex']]);
  h.requests[0].resolve(conv('a', 'codex'));
  await done;
  assert.equal(h.conversation()?.sessionId, 'a');
});

test('a late response for an older request is dropped', async () => {
  const h = harness();
  const a = h.open({ sessionId: 'a' });
  const b = h.open({ sessionId: 'b' });
  h.requests[0].resolve(conv('a'));
  await a;
  assert.equal(h.conversation(), null);
  h.requests[1].resolve(conv('b'));
  await b;
  assert.equal(h.conversation()?.sessionId, 'b');
});

test('a response for another provider is rejected', async () => {
  const h = harness();
  const done = h.open({ sessionId: 'same', provider: 'codex' });
  h.requests[0].resolve(conv('same', 'cursor'));
  await done;
  assert.equal(h.conversation(), null);
});

test('a response without provider falls back to the requested provider', async () => {
  const h = harness();
  const done = h.open({ sessionId: 'same', provider: 'codex' });
  h.requests[0].resolve(conv('same', undefined));
  await done;
  assert.equal(h.conversation()?.sessionId, 'same');
});

test('the response is dropped when tools already loaded for the session', async () => {
  const h = harness();
  const done = h.open({ sessionId: 'a', provider: 'codex' });
  h.deps.toolsLoadedFor.set('codex:a');
  h.requests[0].resolve(conv('a', 'codex'));
  await done;
  assert.equal(h.conversation(), null);
});

test('errors are logged only for the current request', async () => {
  const h = harness();
  const logged = [];
  const original = console.error;
  console.error = (...args) => logged.push(args);
  try {
    const a = h.open({ sessionId: 'a' });
    const b = h.open({ sessionId: 'b' });
    h.requests[0].reject(new Error('old'));
    await a;
    assert.equal(logged.length, 0);
    h.requests[1].reject(new Error('new'));
    await b;
    assert.equal(logged.length, 1);
    assert.equal(logged[0][0], 'Failed to load conversation:');
  } finally {
    console.error = original;
  }
});

test('acceptsConversation matches provider-scoped session keys', () => {
  assert.equal(loader.acceptsConversation(null, conv('a')), false);
  assert.equal(loader.acceptsConversation({ sessionId: 'a' }, conv('a')), true);
  assert.equal(loader.acceptsConversation({ sessionId: 'a' }, conv('a', 'claudeCode')), true);
  assert.equal(loader.acceptsConversation({ sessionId: 'a', provider: 'codex' }, conv('a', 'cursor')), false);
  assert.equal(loader.acceptsConversation({ sessionId: 'a', provider: 'codex' }, conv('b', 'codex')), false);
});

function toolsHarness() {
  const requests = [];
  const deps = {
    getConversation: (sessionId, provider, includeTools) =>
      new Promise((resolve, reject) => requests.push({ sessionId, provider, includeTools, resolve, reject })),
    withConversationLoader: (_id, _provider, kind, task) => { deps.kinds.push(kind); return task(); },
    toolsLoadedFor: writable(null),
    kinds: []
  };
  let current = true;
  let conversation = null;
  const load = target => loader.loadToolsConversation(target, () => current, conv => { conversation = conv; }, deps);
  return { requests, deps, load, stale: () => { current = false; }, conversation: () => conversation };
}

test('loadToolsConversation applies tool dumps and marks the session loaded', async () => {
  const h = toolsHarness();
  const done = h.load({ sessionId: 'a', provider: 'codex' });
  assert.deepEqual(h.requests.map(r => [r.sessionId, r.provider, r.includeTools]), [['a', 'codex', true]]);
  assert.deepEqual(h.deps.kinds, ['tools']);
  h.requests[0].resolve(conv('a', 'codex'));
  assert.equal(await done, true);
  assert.equal(h.conversation()?.sessionId, 'a');
  assert.equal(get(h.deps.toolsLoadedFor), 'codex:a');
});

test('loadToolsConversation rejects a stale response', async () => {
  const h = toolsHarness();
  const done = h.load({ sessionId: 'a', provider: 'codex' });
  h.stale();
  h.requests[0].resolve(conv('a', 'codex'));
  assert.equal(await done, false);
  assert.equal(h.conversation(), null);
  assert.equal(get(h.deps.toolsLoadedFor), null);
});

test('loadToolsConversation rejects a response for another provider', async () => {
  const h = toolsHarness();
  const done = h.load({ sessionId: 'same', provider: 'codex' });
  h.requests[0].resolve(conv('same', 'cursor'));
  assert.equal(await done, false);
  assert.equal(h.conversation(), null);
  assert.equal(get(h.deps.toolsLoadedFor), null);
});

test('loadToolsConversation falls back to the requested provider', async () => {
  const h = toolsHarness();
  const done = h.load({ sessionId: 'same', provider: 'codex' });
  h.requests[0].resolve(conv('same', undefined));
  assert.equal(await done, true);
  assert.equal(get(h.deps.toolsLoadedFor), 'codex:same');
});

test('loadToolsConversation logs errors and reports failure', async () => {
  const h = toolsHarness();
  const logged = [];
  const original = console.error;
  console.error = (...args) => logged.push(args);
  try {
    const done = h.load({ sessionId: 'a' });
    h.requests[0].reject(new Error('boom'));
    assert.equal(await done, false);
    assert.equal(logged.length, 1);
    assert.equal(logged[0][0], 'Failed to load tools:');
    assert.equal(get(h.deps.toolsLoadedFor), null);
  } finally {
    console.error = original;
  }
});
