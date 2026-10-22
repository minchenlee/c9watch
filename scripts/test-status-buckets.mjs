import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import ts from 'typescript';

const read = path => readFileSync(new URL(path, import.meta.url), 'utf8');
const transpile = source => ts.transpileModule(source, {
  compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022 }
}).outputText;
const dataUrl = source => 'data:text/javascript;base64,' + Buffer.from(source).toString('base64');

const typesUrl = dataUrl(transpile(read('../src/lib/types.ts')));
const utils = await import(dataUrl(
  transpile(read('../src/lib/status-utils.ts')).replace("from '$lib/types'", `from '${typesUrl}'`)
));
const { SessionStatus } = await import(typesUrl);

test('status bucket per status, unknown dropped', () => {
  const cases = [
    [SessionStatus.NeedsAttention, 'attention'],
    [SessionStatus.WaitingForInput, 'idle'],
    [SessionStatus.Working, 'working'],
    [SessionStatus.Connecting, 'working'],
    ['Bogus', null],
    [undefined, null]
  ];
  for (const [status, bucket] of cases) assert.equal(utils.statusBucket(status), bucket, String(status));
  for (const status of Object.values(SessionStatus)) assert.notEqual(utils.statusBucket(status), null, status);
});

test('bucket order and labels match the monitor grid', () => {
  assert.deepEqual(utils.STATUS_BUCKETS, [
    { id: 'attention', label: 'Needs Attention' },
    { id: 'idle', label: 'Idle' },
    { id: 'working', label: 'Working' }
  ]);
});

test('bucket counts on a mixed list keep the StatusBar shape', () => {
  const s = status => ({ status });
  const sessions = [
    s(SessionStatus.Working), s(SessionStatus.Connecting), s(SessionStatus.Working),
    s(SessionStatus.NeedsAttention), s(SessionStatus.WaitingForInput), s(SessionStatus.WaitingForInput),
    s('Bogus')
  ];
  const counts = utils.countStatusBuckets(sessions);
  assert.deepEqual(counts, { working: 3, permission: 1, input: 2 });
  assert.deepEqual(Object.keys(counts), ['working', 'permission', 'input']);
  assert.deepEqual(utils.countStatusBuckets([]), { working: 0, permission: 0, input: 0 });
});
