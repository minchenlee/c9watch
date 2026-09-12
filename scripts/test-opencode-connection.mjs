import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import { test } from 'node:test';
import ts from 'typescript';

function harness() {
  const requests = [], timers = new Map();
  let mount, timerId = 0;
  const exports = {};
  const source = readFileSync(new URL('../src/lib/components/OpenCodeConnection.svelte', import.meta.url), 'utf8').split('<script lang="ts">')[1].split('</script>')[0];
  const code = ts.transpileModule(source + '\nexports.connect=connect; exports.state=()=>({status,busy,error});', {
    compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS }
  }).outputText;
  vm.runInNewContext(code, {
    exports, $state: value => value,
    require(name) {
      if (name === 'svelte') return { onMount(fn) { mount = fn; } };
      if (name === '@tauri-apps/api/core') return { invoke: (command, args) => new Promise((resolve, reject) => requests.push({ command, args, resolve, reject })) };
      throw Error(name);
    },
    setTimeout(fn) { timers.set(++timerId, fn); return timerId; },
    clearTimeout(id) { timers.delete(id); }
  });
  const dispose = mount();
  return { ...exports, requests, dispose, timers, tick() { const callbacks = [...timers.values()]; timers.clear(); callbacks.forEach(fn => fn()); } };
}
const settle = async () => { await Promise.resolve(); await Promise.resolve(); };

test('connection status polling remains serial and ignores completion after disposal', async () => {
  const h = harness();
  for (let i = 0; i < 100; i++) h.tick();
  assert.equal(h.requests.length, 1);
  h.requests[0].resolve({ url: '', connected: false }); await settle();
  h.tick(); assert.equal(h.requests.length, 2);
  h.dispose();
  h.requests[1].resolve({ url: 'http://old/', connected: true }); await settle();
  assert.equal(h.state().status.connected, false);
  assert.equal(h.timers.size, 0);
});

test('disconnect invalidates an older status request and coalesces repeated clicks', async () => {
  const h = harness();
  try {
    const action = h.connect(true);
    h.connect(true);
    assert.equal(h.requests.length, 2);
    assert.equal(h.requests[1].args.url, '');
    h.requests[1].resolve(); await settle();
    assert.equal(h.requests.length, 3);
    h.requests[2].resolve({ url: '', connected: false }); await action;
    h.requests[0].resolve({ url: 'http://old/', connected: true }); await settle();
    assert.equal(h.state().status.connected, false);
    assert.equal(h.state().busy, false);
  } finally { h.dispose(); }
});
