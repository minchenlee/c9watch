import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import ts from 'typescript';
import { writable, get } from 'svelte/store';

const source = readFileSync(new URL('../src/lib/stores/usage-preferences.ts', import.meta.url), 'utf8');
const events = new Map();
let stored = '{broken';
let fail = false;
const exports = {};
vm.runInNewContext(ts.transpileModule(source, {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 }
}).outputText, {
  exports,
  require(name) {
    if (name === '$app/environment') return { browser: true };
    if (name === 'svelte/store') return { writable };
    throw new Error(name);
  },
  localStorage: { getItem: () => stored, setItem: (_, value) => {
    if (fail) throw new Error('Storage unavailable');
    stored = value;
  } },
  window: { addEventListener: (name, fn) => events.set(name, fn) }
});
const { usagePreferences, saveUsagePreferences, normalizeUsagePreferences } = exports;
assert.equal(get(usagePreferences).percentages, 'auto', 'corrupt storage uses defaults');
const next = normalizeUsagePreferences({ percentages: 'never', colors: 'monochrome', icons: false, providers: { cursor: false } });
assert.equal(next.providers.codex, true, 'missing providers retain defaults');
assert.equal(saveUsagePreferences(next), null);
assert.equal(JSON.parse(stored).percentages, 'never');
assert.equal(get(usagePreferences).icons, false);
fail = true;
assert.ok(saveUsagePreferences(normalizeUsagePreferences(null)));
assert.equal(get(usagePreferences).percentages, 'never', 'failed save retains last saved state');
fail = false;
stored = JSON.stringify({ percentages: 'always' });
events.get('storage')({ key: 'unrelated' });
assert.equal(get(usagePreferences).percentages, 'never');
events.get('storage')({ key: 'c9watch.usagePreferences.v1' });
assert.equal(get(usagePreferences).percentages, 'always', 'other windows receive saved preferences');
stored = 'null';
events.get('focus')();
assert.equal(get(usagePreferences).percentages, 'auto', 'focus recovers missed storage updates');
console.log('PASS: corrupt storage, defaults, persistence failure, cross-window storage and focus refresh');
