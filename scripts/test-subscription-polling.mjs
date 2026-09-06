import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import ts from 'typescript';
import { writable } from 'svelte/store';

// Exercise the component's real polling lifecycle, independent of DOM rendering.
const source = readFileSync(new URL('../src/lib/components/SubscriptionUsage.svelte', import.meta.url), 'utf8')
  .match(/<script lang="ts">([\s\S]*?)<\/script>/)[1];
const requests = [];
const failures = [];
const timers = new Set();
const windowEvents = new Map();
const documentEvents = new Map();
let mount;
let desktop = true;
const document = {
  hidden: true,
  addEventListener: (name, fn) => documentEvents.set(name, fn),
  removeEventListener: name => documentEvents.delete(name)
};
const context = vm.createContext({
  exports: {},
  require(name) {
    if (name === 'svelte') return { onMount: fn => mount = fn };
    if (name === '$lib/api') return { getSubscriptionUsage: () => new Promise((resolve, reject) => { requests.push(resolve); failures.push(reject); }) };
    if (name === '$lib/demo/mode') return { isDemoMode: writable(false) };
    if (name === '$lib/ws') return { isTauri: () => desktop };
    throw new Error(`Unexpected dependency: ${name}`);
  },
  $state: x => x, $derived: x => x, $props: () => ({}),
  $usagePreferences: {percentages:'auto', colors:'provider', icons:true, providers:{claudeCode:true,codex:true,cursor:true}},
  document,
  window: { addEventListener: (name, fn) => windowEvents.set(name, fn), removeEventListener: name => windowEvents.delete(name) },
  setInterval(fn) { timers.add(fn); return fn; },
  clearInterval: fn => timers.delete(fn),
  Date
});
vm.runInContext(ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText, context);
const settle = () => new Promise(resolve => setImmediate(resolve));
const tick = () => { for (const timer of timers) timer(); };
const stop = mount();
assert.equal(requests.length, 1);
requests[0]([]); await settle();
tick();
assert.equal(requests.length, 2, 'hidden desktop windows must continue quota refresh');
for (let i = 0; i < 100; i++) { tick(); windowEvents.get('focus')(); }
assert.equal(requests.length, 2, 'refresh remains single-flight under focus and timer bursts');
requests[1]([]); await settle();
desktop = false;
tick();
assert.equal(requests.length, 2, 'hidden browser tabs still pause polling');
document.hidden = false;
documentEvents.get('visibilitychange')();
assert.equal(requests.length, 3, 'returning to visible browser refreshes');
requests[2]([{provider:'codex',name:'Codex',windows:[{usedPercent:42,resetsAt:null}],updatedAt:1000,message:null}]); await settle();
windowEvents.get('focus')();
assert.equal(requests.length, 4, 'returning to the window refreshes immediately');
failures[3](new Error('Disconnected')); await settle();
const retained = vm.runInContext('usage[0]', context);
assert.equal(retained.windows[0].usedPercent, 42);
assert.equal(retained.updatedAt, 1000);
assert.match(retained.message, /Last known usage/);
windowEvents.get('focus')();
stop(); requests[4]([]); await settle();
assert.equal(timers.size, 0);
assert.equal(windowEvents.size, 0);
assert.equal(documentEvents.size, 0);
console.log('PASS: desktop background refresh, browser pause, single-flight, focus refresh, cleanup');
