import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import ts from 'typescript';
import * as stores from 'svelte/store';

const sessions = stores.writable([]);
const requests = [];
const timers = new Set();
const exports = {};
const source = readFileSync(new URL('../src/lib/stores/subagents.ts', import.meta.url), 'utf8');
const compiled = ts.transpileModule(source, {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 }
}).outputText;
vm.runInNewContext(compiled, {
  exports,
  require(name) {
    if (name === 'svelte/store') return stores;
    if (name === '@tauri-apps/api/core') return {
      invoke: (command, args) => new Promise((resolve, reject) => requests.push({ command, args, resolve, reject }))
    };
    if (name === './sessions') return { sessions };
    if (name === '../ws') return { isTauri: () => true };
    if (name === '../provider') return {
      providerOf: (session) => session.provider || 'claudeCode',
      providerSessionKey: (p, id) => `${p}:${id}`
    };
    throw new Error(`Unexpected dependency: ${name}`);
  },
  setTimeout(fn) { timers.add(fn); return fn; },
  clearTimeout(fn) { timers.delete(fn); },
  Date, Map
});
const settle = () => new Promise(resolve => setImmediate(resolve));
const tick = () => {
  const work = [...timers];
  timers.clear();
  for (const timer of work) timer();
};
const assertPayload = (actual, expected, message) => {
  assert.equal(JSON.stringify(actual), JSON.stringify(expected), message);
};
sessions.set([
  { id: 'claude-live', provider: 'claudeCode' },
  { id: 'codex-live', provider: 'codex' }
]);
const stop = exports.initializeSubagentPolling();
assert.equal(requests.length, 1, 'initial subscription and initial refresh must coalesce');
assertPayload(requests[0].args, { sessionIds: ['claude-live'] }, 'backend receives raw Claude IDs only');
for (let i = 0; i < 100; i++) { sessions.set([]); tick(); }
assert.equal(requests.length, 1, 'slow scans must not accumulate requests');
requests[0].resolve({ 'codex:first': [] });
await settle();
assert(exports._snapshotForTests().has('codex:first'));
assert.equal(requests.length, 2, 'latest session update queues one follow-up scan');
assertPayload(requests[1].args, { sessionIds: [] }, 'queued follow-up uses the latest live IDs');
requests[1].reject(new Error('test scan failure'));
await settle();
tick();
assert.equal(requests.length, 3, 'failed scans must release the in-flight guard');
stop();
const restart = exports.initializeSubagentPolling();
assert.equal(requests.length, 3, 'reinitialize must wait for the old in-flight request');
requests[2].resolve({ 'cursor:stale': [] });
await settle();
assert(!exports._snapshotForTests().has('cursor:stale'), 'teardown must invalidate late responses');
assert.equal(requests.length, 4, 'new generation must refresh after old request settles');
assertPayload(requests[3].args, { sessionIds: [] }, 'new generation uses the current live IDs');
requests[3].resolve({ 'claudeCode:new': [] });
await settle();
assert(exports._snapshotForTests().has('claudeCode:new'));
restart();
assert.equal(timers.size, 0);
console.log('PASS: bounded polling, resume, failure recovery, raw live IDs, teardown reinitialize');
