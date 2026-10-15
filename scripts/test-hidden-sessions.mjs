import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import ts from 'typescript';

const read = path => readFileSync(new URL(path, import.meta.url), 'utf8');
const transpile = source => ts.transpileModule(source, {
  compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022 }
}).outputText;
const utils = await import('data:text/javascript;base64,' + Buffer.from(transpile(read('../src/lib/session-utils.ts'))).toString('base64'));

test('no hidden sessions shows nothing', () => {
  assert.equal(utils.hiddenSessionsSummary([]), null);
  assert.equal(utils.hiddenSessionsSummary(undefined), null);
});

test('summary names the count with the right plural', () => {
  const one = utils.hiddenSessionsSummary([{ pid: 1, cwd: '/a', reason: 'childSessionMarker', parentClaudePid: null }]);
  assert.match(one, /^1 Claude Code session is hidden by Claude Code itself/);
  const two = utils.hiddenSessionsSummary([
    { pid: 1, cwd: '/a', reason: 'childSessionMarker', parentClaudePid: null },
    { pid: 2, cwd: null, reason: 'childSessionMarker', parentClaudePid: 9 }
  ]);
  assert.match(two, /^2 Claude Code sessions are hidden by Claude Code itself/);
});

test('summary says why and how to fix it', () => {
  const text = utils.hiddenSessionsSummary([{ pid: 1, cwd: '/a', reason: 'childSessionMarker', parentClaudePid: null }]);
  assert.match(text, /another Claude session opened/);
  assert.match(text, /Quit that app and open it again from the Dock/);
});
