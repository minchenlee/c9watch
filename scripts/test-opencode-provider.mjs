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
