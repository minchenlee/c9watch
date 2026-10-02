import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import ts from 'typescript';

const read = path => readFileSync(new URL(path, import.meta.url), 'utf8');
const transpile = source => ts.transpileModule(source, {
  compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022 }
}).outputText;
const provider = await import('data:text/javascript;base64,' + Buffer.from(transpile(read('../src/lib/provider.ts'))).toString('base64'));
const stores = read('../src/lib/stores/sessions.ts');
const start = stores.indexOf('function attentionTime');
const end = stores.indexOf('\n/**', start);
const inbox = new Function('sessions', 'derived', 'SessionStatus', 'providerOf', 'sessionKeyOf', 'sessionReturnKind',
  transpile(stores.slice(start, end)).replace('export const attentionInbox', 'const attentionInbox') + '\nreturn attentionInbox;');
const page = read('../src/routes/(app)/+page.svelte');
const jumpStart = page.indexOf('\tasync function jumpToAttention()');
const jumpEnd = page.indexOf('\n\tfunction handleKeydown', jumpStart);
const dispatch = new Function('attentionItems', 'handleOpen', 'expandedSessionId',
  transpile(page.slice(jumpStart, jumpEnd)) + '\nreturn jumpToAttention();');
const session = (id, openTarget) => ({ id, provider: 'claudeCode', pid: 42, projectPath: '/tmp/shared',
  status: 'NeedsAttention', modified: '2026-10-02T00:00:00Z', canOpen: true, openTarget });
const items = sessions => inbox(sessions, (value, fn) => fn(value), { NeedsAttention: 'NeedsAttention' },
  provider.providerOf, provider.sessionKeyOf, provider.sessionReturnKind);

test('attention exposes only the backend-declared native guarantee', () => {
  assert.deepEqual(items([session('a', 'terminal'), session('b', 'project'), session('c', 'application'), session('d')])
    .map(item => item.returnKind), ['native', 'project', 'application', 'conversation']);
  assert.equal(items([{ ...session('codex', 'terminal'), provider: 'codex', canOpen: false }])[0].returnKind, 'conversation');
});

test('project open selects A conversation without asserting selection of its IDE terminal', async () => {
  const selections = [], opens = [];
  const list = items([session('a', 'project'), session('b', 'project')]);
  await dispatch(list, async (...args) => opens.push(args), { set: key => selections.push(key) });
  assert.deepEqual(selections, ['claudeCode:a']);
  assert.deepEqual(opens, [[42, '/tmp/shared']]);
});

test('exact terminal dispatch uses native focus and unknown capabilities use conversation', async () => {
  for (const target of ['terminal', undefined]) {
    const selections = [], opens = [];
    await dispatch(items([session('a', target)]), async (...args) => opens.push(args), { set: key => selections.push(key) });
    assert.equal(opens.length, target === 'terminal' ? 1 : 0);
    assert.deepEqual(selections, target === 'terminal' ? [] : ['claudeCode:a']);
  }
});
