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
    if (name === '@tauri-apps/api/core') return { invoke: () => new Promise((resolve, reject) => requests.push({ resolve, reject })) };
    if (name === './sessions') return { sessions };
    if (name === '../ws') return { isTauri: () => true };
    if (name === '../provider') return { providerSessionKey: (p, id) => `${p}:${id}` };
    throw new Error(`Unexpected dependency: ${name}`);
  },
  setInterval(fn) { timers.add(fn); return fn; },
  clearInterval(fn) { timers.delete(fn); },
  Date, Map
});
const settle = () => new Promise(resolve => setImmediate(resolve));
const tick = () => { for (const timer of timers) timer(); };
const stop = exports.initializeSubagentPolling();
assert.equal(requests.length, 1, 'initial subscription and initial refresh must coalesce');
for (let i = 0; i < 100; i++) { sessions.set([]); tick(); }
assert.equal(requests.length, 1, 'slow scans must not accumulate requests');
requests[0].resolve({ 'codex:first': [] });
await settle();
assert(exports._snapshotForTests().has('codex:first'));
tick();
assert.equal(requests.length, 2, 'polling resumes after completion');
requests[1].reject(new Error('test scan failure'));
await settle();
tick();
assert.equal(requests.length, 3, 'failed scans must release the in-flight guard');
stop();
requests[2].resolve({ 'cursor:stale': [] });
await settle();
assert(!exports._snapshotForTests().has('cursor:stale'), 'teardown must invalidate late responses');
assert.equal(timers.size, 0);
console.log('PASS: bounded polling, resume, failure recovery, teardown invalidation');
