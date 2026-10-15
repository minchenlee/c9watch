import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import ts from 'typescript';

const read = path => readFileSync(new URL(path, import.meta.url), 'utf8');
const transpile = source => ts.transpileModule(source, {
  compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022 }
}).outputText;
const layout = await import('data:text/javascript;base64,' + Buffer.from(transpile(read('../src/lib/dense-layout.ts'))).toString('base64'));

const s = (id, startedAtMs) => ({ id, startedAtMs });
const group = (displayName, { attention = [], idle = [], working = [] } = {}) => ({
  path: `/code/${displayName}`, displayName, attention, idle, working
});

test('tiles: attention first, then idle, then all-working; alphabetical within a tier', () => {
  const tiles = layout.denseTiles([
    group('zeta', { working: [s('w1', 1)] }),
    group('beta', { idle: [s('i1', 1)] }),
    group('alpha', { working: [s('w2', 1)] }),
    group('omega', { attention: [s('a1', 1)], working: [s('w3', 1)] }),
    group('delta', { idle: [s('i2', 1)], working: [s('w4', 1)] })
  ]);
  assert.deepEqual(tiles.map(t => t.displayName), ['omega', 'beta', 'delta', 'alpha', 'zeta']);
  assert.deepEqual(tiles.map(t => t.tier), [0, 1, 1, 2, 2]);
});

test('tiles: rows grouped by status, oldest session first, id breaks ties', () => {
  const [tile] = layout.denseTiles([
    group('p', {
      attention: [s('a2', 20), s('a1', 10)],
      idle: [s('i1', 5)],
      working: [s('wb', null), s('w2', 30), s('wa', null)]
    })
  ]);
  assert.deepEqual(tile.sessions.map(x => x.id), ['a1', 'a2', 'i1', 'w2', 'wa', 'wb']);
});

test('tiles: inputs are not mutated', () => {
  const working = [s('b', 2), s('a', 1)];
  layout.denseTiles([group('p', { working })]);
  assert.deepEqual(working.map(x => x.id), ['b', 'a']);
});

test('column count fits the width and never exceeds the item count', () => {
  assert.equal(layout.columnCount(0, 300, 6, 5), 1);
  assert.equal(layout.columnCount(360, 300, 6, 5), 1);
  assert.equal(layout.columnCount(606, 300, 6, 5), 2);
  assert.equal(layout.columnCount(605, 300, 6, 5), 1);
  assert.equal(layout.columnCount(2000, 300, 6, 13), 6);
  assert.equal(layout.columnCount(2000, 300, 6, 3), 3);
  assert.equal(layout.columnCount(2000, 300, 6, 0), 1);
});

test('packing: first items read left to right, then fill the shortest column', () => {
  const items = [['a', 3], ['b', 1], ['c', 1], ['d', 1], ['e', 2]];
  const columns = layout.packColumns(items, 3, ([, h]) => h);
  assert.deepEqual(columns.map(c => c.map(([name]) => name)), [['a'], ['b', 'd'], ['c', 'e']]);
});

test('packing: one column keeps order; zero columns still yields one', () => {
  const items = [1, 2, 3];
  assert.deepEqual(layout.packColumns(items, 1, () => 1), [[1, 2, 3]]);
  assert.deepEqual(layout.packColumns(items, 0, () => 1), [[1, 2, 3]]);
});
