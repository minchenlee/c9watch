import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import ts from 'typescript';

const code = ts.transpileModule(readFileSync(new URL('../src/lib/provider.ts', import.meta.url), 'utf8'), {
  compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022 }
}).outputText;
const provider = await import('data:text/javascript;base64,' + Buffer.from(code).toString('base64'));

test('OpenCode selection stays separate from other providers with the same ID', () => {
  const session = { id: 'same', provider: 'opencode', sessionKey: 'claudeCode:same' };
  assert.equal(provider.sessionKeyOf(session), 'opencode:same');
  assert.equal(provider.providerLabel(provider.providerOf(session)), 'OPENCODE');
  assert.equal(provider.matchesProvider(session, 'opencode'), true);
  assert.equal(provider.matchesProvider(session, 'claudeCode'), false);
  for (const action of ['open', 'stop', 'rename']) assert.equal(provider.canSessionAction(session, action), false);
  assert.equal(provider.canSessionAction(session, 'conversation'), true);
});

test('OpenCode descendants attach only to their own provider and orphans remain visible', () => {
  const sessions = [
    { id: 'root', provider: 'claudeCode', agentKind: 'root' },
    { id: 'root', provider: 'opencode', agentKind: 'root' },
    { id: 'child', provider: 'opencode', agentKind: 'subagent', parentThreadId: 'root' },
    { id: 'orphan', provider: 'opencode', agentKind: 'subagent', parentThreadId: 'missing' }
  ];
  const hierarchy = provider.resolveCodexHierarchy(sessions);
  assert.deepEqual(hierarchy.subagentsByParent.get('opencode:root'), [sessions[2]]);
  assert.equal(hierarchy.subagentsByParent.has('claudeCode:root'), false);
  assert.equal(hierarchy.topLevelIds.has('opencode:orphan'), true);
});

test('directory-qualified OpenCode roots and children cannot collide', () => {
  const sessions = ['/a', '/b'].flatMap(directory => {
    const suffix = '?directory=' + encodeURIComponent(directory);
    return [
      { id: 'same' + suffix, provider: 'opencode', agentKind: 'root' },
      { id: 'child' + suffix, provider: 'opencode', agentKind: 'subagent', parentThreadId: 'same' + suffix }
    ];
  });
  const hierarchy = provider.resolveCodexHierarchy(sessions);
  assert.equal(hierarchy.topLevelIds.size, 2);
  assert.deepEqual(hierarchy.subagentsByParent.get('opencode:same?directory=%2Fa'), [sessions[1]]);
  assert.deepEqual(hierarchy.subagentsByParent.get('opencode:same?directory=%2Fb'), [sessions[3]]);
});

test('provider registry keeps labels, order and known-provider checks stable', () => {
  assert.deepEqual(provider.PROVIDERS.map(p => p.id), ['claudeCode', 'codex', 'cursor', 'pi', 'opencode']);
  const badge = { claudeCode: 'CLAUDE CODE', codex: 'CODEX', cursor: 'CURSOR', pi: 'PI', opencode: 'OPENCODE' };
  const filter = { claudeCode: 'Claude Code', codex: 'Codex', cursor: 'Cursor', pi: 'Pi', opencode: 'OpenCode' };
  for (const id of Object.keys(badge)) {
    assert.equal(provider.providerLabel(id), badge[id]);
    assert.equal(provider.providerFilterLabel(id), filter[id]);
    assert.equal(provider.isKnownProvider(id), true);
  }
  assert.equal(provider.providerFilterLabel('all'), 'All providers');
  assert.equal(provider.providerLabel('foo'), 'CLAUDE CODE');
  for (const value of ['foo', null, undefined, 'all', '']) assert.equal(provider.isKnownProvider(value), false);
  assert.equal(provider.providerOf({ provider: 'foo' }), 'claudeCode');
  assert.equal(provider.providerSessionKey(undefined, 'x'), 'claudeCode:x');
});

test('canSessionAction uses direct booleans, then provider defaults', () => {
  const claude = { id: 'c', provider: 'claudeCode' };
  for (const action of ['open', 'stop', 'rename', 'conversation']) assert.equal(provider.canSessionAction(claude, action), true);
  const locked = { ...claude, canOpen: false, canStop: false, canRename: false };
  for (const action of ['open', 'stop', 'rename']) assert.equal(provider.canSessionAction(locked, action), false);
  assert.equal(provider.canSessionAction(locked, 'conversation'), true);
  const codex = { id: 'x', provider: 'codex', canOpen: true };
  assert.equal(provider.canSessionAction(codex, 'open'), true);
  assert.equal(provider.canSessionAction(codex, 'stop'), false);
  assert.equal(provider.canSessionAction(codex, 'conversation'), true);
});

test('demo Codex and Cursor cards hide open, stop and rename', () => {
  const demo = readFileSync(new URL('../src/lib/data/demo/data.ts', import.meta.url), 'utf8');
  assert.equal(demo.includes('actionCapabilities'), false);
  assert.equal(demo.match(/canOpen: false, canStop: false, canRename: false/g)?.length, 4);
});
