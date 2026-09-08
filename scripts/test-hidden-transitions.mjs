import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import ts from 'typescript';

const source = readFileSync(new URL('../src/lib/transitions.ts', import.meta.url), 'utf8');
const wsCode = ts.transpileModule(readFileSync(new URL('../src/lib/ws.ts', import.meta.url), 'utf8'), { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022 } }).outputText;
const wsUrl = 'data:text/javascript;base64,' + Buffer.from(wsCode).toString('base64');
const code = ts.transpileModule(source, {
  compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022 }
}).outputText.replace("'./ws'", JSON.stringify(wsUrl)).replaceAll("'svelte/transition'", JSON.stringify(import.meta.resolve('svelte/transition')))
  .replaceAll("'svelte/easing'", JSON.stringify(import.meta.resolve('svelte/easing')));
const transitions = await import('data:text/javascript;base64,' + Buffer.from(code).toString('base64'));

test('hidden intros and outros bypass delayed keyframes, including explicit delays', () => {
  const node = { ownerDocument: { visibilityState: 'hidden' } };
  for (const name of ['fade', 'fadeIn', 'flyIn', 'flyInX', 'scale', 'slide']) {
    const config = transitions[name](node, { duration: 400, delay: 800, index: 10 });
    assert.equal(config.duration, 0, name);
    assert.equal(config.delay, 0, name);
    assert.equal(config.css, undefined, name);
  }
});

test('visible transitions retain motion and reevaluate visibility on every invocation', () => {
  globalThis.getComputedStyle = () => ({ opacity: '1', transform: 'none' });
  const node = { ownerDocument: { visibilityState: 'visible' } };
  assert.equal(transitions.fadeIn(node).duration, 320);
  assert.equal(transitions.flyIn(node, { index: 2 }).delay, 120);
  node.ownerDocument.visibilityState = 'hidden';
  assert.equal(transitions.fadeIn(node).duration, 0);
  node.ownerDocument.visibilityState = 'visible';
  assert.equal(transitions.fadeIn(node).duration, 320);
  delete globalThis.getComputedStyle;
});

test('reduced motion also bypasses supplied delays', () => {
  globalThis.window = { matchMedia: () => ({ matches: true }) };
  try {
    assert.deepEqual(transitions.fade({ ownerDocument: { visibilityState: 'visible' } }, { delay: 500 }), { duration: 0, delay: 0 });
  } finally { delete globalThis.window; }
});


test('native transitions never install a first frame that can freeze when hidden mid-intro', () => {
  globalThis.window = { __TAURI_INTERNALS__: { invoke() {} } };
  try {
    for (const name of ['fade', 'fadeIn', 'flyIn', 'flyInX', 'scale', 'slide']) {
      assert.deepEqual(transitions[name]({ ownerDocument: { visibilityState: 'visible' } }, { duration: 500, delay: 300 }), { duration: 0, delay: 0 });
    }
  } finally { delete globalThis.window; }
});

test('conversation bubbles do not add a second opacity animation outside the safe transition', () => {
  const bubble = readFileSync(new URL('../src/lib/components/MessageBubble.svelte', import.meta.url), 'utf8');
  assert.doesNotMatch(bubble, /animation\s*:\s*fade-in/);
});
