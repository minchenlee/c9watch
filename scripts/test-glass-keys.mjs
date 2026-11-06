// Glass keyboard contract (R7-SPEC §A.2): src/shells/glass/keys.ts.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import ts from 'typescript';

const source = readFileSync(new URL('../src/shells/glass/keys.ts', import.meta.url), 'utf8');
const js = ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022 } }).outputText;
const keys = await import('data:text/javascript;base64,' + Buffer.from(js).toString('base64'));

const input = { tagName: 'INPUT', type: 'text' };
const textarea = { tagName: 'TEXTAREA' };
const button = { tagName: 'BUTTON' };

function harness() {
  const calls = [];
  const shell = {
    search: () => calls.push('search'),
    delegate: () => calls.push('delegate'),
    sidebar: () => calls.push('sidebar'),
    escape: () => { calls.push('shell-escape'); return false; }
  };
  const page = {
    option: n => calls.push(`option${n}`),
    submit: () => calls.push('submit'),
    openChat: () => calls.push('openChat'),
    next: () => calls.push('next'),
    prev: () => calls.push('prev'),
    undo: () => calls.push('undo'),
    escape: () => { calls.push('page-escape'); return true; }
  };
  return { calls, shell, page };
}
function press(shell, code, { target = button, ...mods } = {}) {
  const e = { code, target, prevented: false, stopped: false, ...mods,
    preventDefault() { this.prevented = true; }, stopPropagation() { this.stopped = true; } };
  return { cmd: keys.handleKey(e, shell), e };
}

test('outside a text field every contract key acts', () => {
  const { calls, shell, page } = harness();
  const off = keys.usePageKeys(page);
  for (const [code, mods] of [['KeyK', { metaKey: true }], ['KeyN', { metaKey: true }], ['BracketLeft', {}], ['Digit1', {}], ['Digit4', {}],
    ['Enter', { metaKey: true }], ['KeyO', { metaKey: true }], ['KeyJ', {}], ['KeyK', {}], ['KeyZ', { metaKey: true }], ['Escape', {}]]) {
    const { e } = press(shell, code, mods);
    assert.ok(e.prevented && e.stopped, code);
  }
  off();
  assert.deepEqual(calls, ['search', 'delegate', 'sidebar', 'option1', 'option4', 'submit', 'openChat', 'next', 'prev', 'undo', 'shell-escape', 'page-escape']);
});

test('in a text field only ⌘↵ and Esc act; other keys are typing', () => {
  const { calls, shell, page } = harness();
  const off = keys.usePageKeys(page);
  for (const target of [input, textarea, { tagName: 'DIV', isContentEditable: true }]) {
    for (const [code, mods] of [['KeyJ', {}], ['KeyK', {}], ['Digit1', {}], ['BracketLeft', {}], ['KeyK', { metaKey: true }], ['KeyN', { metaKey: true }], ['KeyZ', { metaKey: true }], ['KeyO', { metaKey: true }]]) {
      const { cmd, e } = press(shell, code, { target, ...mods });
      assert.equal(cmd, null, `${code} in ${target.tagName}`);
      assert.equal(e.prevented, false, 'the browser keeps the key');
    }
  }
  assert.equal(press(shell, 'Enter', { target: input, metaKey: true }).cmd, 'submit');
  assert.equal(press(shell, 'Escape', { target: textarea }).cmd, 'escape');
  off();
  assert.deepEqual(calls, ['submit', 'shell-escape', 'page-escape']);
});

test('a checkbox or radio is not a text field', () => {
  assert.equal(keys.isTextField({ tagName: 'INPUT', type: 'radio' }), false);
  assert.equal(keys.isTextField({ tagName: 'INPUT', type: 'checkbox' }), false);
  assert.equal(keys.isTextField({ tagName: 'INPUT', type: 'search' }), true);
  assert.equal(keys.isTextField({ tagName: 'INPUT' }), true);
});

test('while an input method composes, no key acts, not even ⌘↵ or Esc', () => {
  const { calls, shell, page } = harness();
  const off = keys.usePageKeys(page);
  for (const [code, mods] of [['KeyJ', {}], ['Enter', { metaKey: true }], ['Escape', {}], ['KeyK', { metaKey: true }]]) {
    assert.equal(press(shell, code, { isComposing: true, ...mods }).cmd, null);
    assert.equal(press(shell, code, { target: input, keyCode: 229, ...mods }).cmd, null);
  }
  off();
  assert.deepEqual(calls, []);
});

test('J / K use the key code, so a Chinese input method does not change them', () => {
  // Under Zhuyin, `key` is a Bopomofo letter; `code` stays KeyJ.
  assert.equal(keys.resolveKey({ code: 'KeyJ', key: 'ㄨ' }, false), 'next');
  assert.equal(keys.resolveKey({ code: 'KeyK', key: 'ㄜ' }, false), 'prev');
  assert.equal(keys.resolveKey({ code: 'KeyX', key: 'j' }, false), null);
});

test('a receipt page does not accept ⌘↵; J still moves on', () => {
  const { calls, shell, page } = harness();
  const off = keys.usePageKeys({ ...page, receipt: true });
  const { cmd, e } = press(shell, 'Enter', { metaKey: true });
  assert.equal(cmd, null);
  assert.equal(e.prevented, false);
  assert.equal(press(shell, 'KeyJ').cmd, 'next');
  off();
  assert.deepEqual(calls, ['next']);
});

test('the newest page gets the keys; unregister restores the one before', () => {
  const a = harness(); const b = harness();
  const offA = keys.usePageKeys(a.page);
  const offB = keys.usePageKeys(b.page);
  press(a.shell, 'KeyJ');
  offB();
  press(a.shell, 'KeyJ');
  offA();
  assert.equal(press(a.shell, 'KeyJ').cmd, null, 'no page, no J');
  assert.deepEqual(b.calls, ['next']);
  assert.deepEqual(a.calls, ['next']);
});

test('Esc: the shell closes first; in a field with nothing to close, the field blurs', () => {
  let blurred = false;
  const shell = { ...harness().shell, escape: () => true };
  assert.equal(press(shell, 'Escape', { target: input }).cmd, 'escape');
  const quiet = { ...harness().shell, escape: () => false };
  const field = { ...input, blur: () => { blurred = true; } };
  assert.equal(press(quiet, 'Escape', { target: field }).cmd, null);
  assert.ok(blurred);
});

test('modifier combinations outside the contract are ignored', () => {
  for (const e of [{ code: 'KeyJ', altKey: true }, { code: 'KeyJ', shiftKey: true }, { code: 'KeyK', metaKey: true, shiftKey: true },
    { code: 'Enter', metaKey: true, shiftKey: true }, { code: 'KeyJ', metaKey: true }, { code: 'Enter' }]) {
    assert.equal(keys.resolveKey(e, false), null, JSON.stringify(e));
  }
  assert.equal(keys.resolveKey({ code: 'KeyN', ctrlKey: true }, false), 'delegate');
});
