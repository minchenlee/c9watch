import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import ts from 'typescript';

const read = path => readFileSync(new URL(path, import.meta.url), 'utf8');
const transpile = source => ts.transpileModule(source, {
  compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022 }
}).outputText;
const utils = await import('data:text/javascript;base64,' + Buffer.from(transpile(read('../src/lib/session-utils.ts'))).toString('base64'));

const activity = (over = {}) => ({
  sinceMs: 0, toolCalls: 0, filesEdited: 0, commandsRun: 0, subagentsCompleted: 0, partial: false, ...over
});

test('waiting-for label names the tool only', () => {
  assert.equal(utils.waitingForLabel({ kind: 'approval', tool: 'Bash' }), 'Approval: Bash');
  assert.equal(utils.waitingForLabel({ kind: 'question' }), 'Question');
  assert.equal(utils.waitingForLabel({ kind: 'attention' }), 'Needs attention');
  assert.equal(utils.waitingForLabel({ kind: 'input' }), 'Idle, your turn');
  assert.equal(utils.waitingForLabel(undefined), null);
});

test('away summary shows a dash when the provider has no data', () => {
  assert.equal(utils.awayActivityLabel(undefined), '—');
  assert.equal(utils.awayActivityLabel(null), '—');
});

test('away summary lists only non-zero counts with singular forms', () => {
  assert.equal(utils.awayActivityLabel(activity()), 'No activity');
  assert.equal(
    utils.awayActivityLabel(activity({ toolCalls: 12, filesEdited: 4, commandsRun: 6, subagentsCompleted: 2 })),
    '4 files edited · 6 commands · 2 subagents done · 12 tool calls'
  );
  assert.equal(
    utils.awayActivityLabel(activity({ toolCalls: 1, filesEdited: 1 })),
    '1 file edited · 1 tool call'
  );
  assert.equal(
    utils.awayActivityLabel(activity({ toolCalls: 3, commandsRun: 3, partial: true })),
    '≥ 3 commands · ≥ 3 tool calls'
  );
});

test('away start: last look, else window blur, else session start', () => {
  const session = { id: 's1', provider: 'claudeCode', startedAtMs: 100 };
  assert.equal(utils.awaySinceMs(session, 'claudeCode:s1', { 'claudeCode:s1': 300 }, 200), 300);
  assert.equal(utils.awaySinceMs(session, 'claudeCode:s1', {}, 200), 200);
  assert.equal(utils.awaySinceMs(session, 'claudeCode:s1', {}, null), 100);
  assert.equal(utils.awaySinceMs({ id: 's2' }, 'claudeCode:s2', {}, null), 0);
});

test('session display title follows the six-field precedence', () => {
  const all = {
    customTitle: 'custom', officialName: 'official', codexTitle: 'codex',
    cursorTitle: 'cursor', summary: 'summary', firstPrompt: 'prompt'
  };
  assert.equal(utils.sessionDisplayTitle(all), 'custom');
  assert.equal(utils.sessionDisplayTitle({ ...all, customTitle: null }), 'official');
  assert.equal(utils.sessionDisplayTitle({ ...all, customTitle: null, officialName: undefined }), 'codex');
  assert.equal(utils.sessionDisplayTitle({ ...all, customTitle: null, officialName: null, codexTitle: null }), 'cursor');
  assert.equal(
    utils.sessionDisplayTitle({ ...all, customTitle: null, officialName: null, codexTitle: null, cursorTitle: null }),
    'summary'
  );
  assert.equal(
    utils.sessionDisplayTitle({ customTitle: null, summary: null, firstPrompt: 'prompt' }),
    'prompt'
  );
});

test('session display title skips empty strings and returns the raw last value', () => {
  assert.equal(
    utils.sessionDisplayTitle({
      customTitle: '', officialName: '', codexTitle: '', cursorTitle: '', summary: '', firstPrompt: 'prompt'
    }),
    'prompt'
  );
  assert.equal(utils.sessionDisplayTitle({ customTitle: '', summary: null, firstPrompt: '' }), '');
  assert.equal(utils.sessionDisplayTitle({ customTitle: null, summary: null }), undefined);
});
