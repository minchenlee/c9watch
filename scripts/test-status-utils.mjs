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

test('session status color per status', () => {
  assert.equal(utils.getSessionStatusColor(SessionStatus.Working), 'var(--status-working)');
  assert.equal(utils.getSessionStatusColor(SessionStatus.NeedsAttention), 'var(--status-permission)');
  assert.equal(utils.getSessionStatusColor(SessionStatus.WaitingForInput), 'var(--status-input)');
  assert.equal(utils.getSessionStatusColor(SessionStatus.Connecting), 'var(--status-working)');
  assert.equal(utils.getSessionStatusColor('Bogus'), 'var(--status-working)');
});

test('session status label per status', () => {
  assert.equal(utils.getSessionStatusLabel(SessionStatus.Working), 'Working');
  assert.equal(utils.getSessionStatusLabel(SessionStatus.NeedsAttention, 'Bash'), 'Approval Required');
  assert.equal(utils.getSessionStatusLabel(SessionStatus.NeedsAttention, null), 'Approval Required');
  assert.equal(utils.getSessionStatusLabel(SessionStatus.WaitingForInput), 'Ready');
  assert.equal(utils.getSessionStatusLabel(SessionStatus.Connecting), 'Connecting');
  assert.equal(utils.getSessionStatusLabel('Bogus'), 'Unknown');
});

test('question tools read as Waiting for Response', () => {
  assert.equal(utils.getSessionStatusLabel(SessionStatus.NeedsAttention, 'Question'), 'Waiting for Response');
  assert.equal(utils.getSessionStatusLabel(SessionStatus.NeedsAttention, 'AskUserQuestion'), 'Waiting for Response');
  assert.equal(utils.getSessionStatusLabel(SessionStatus.Working, 'AskUserQuestion'), 'Working');
});

test('isQuestionTool matches only the two question names', () => {
  assert.equal(utils.isQuestionTool('Question'), true);
  assert.equal(utils.isQuestionTool('AskUserQuestion'), true);
  assert.equal(utils.isQuestionTool('Bash'), false);
  assert.equal(utils.isQuestionTool('question'), false);
  assert.equal(utils.isQuestionTool(null), false);
  assert.equal(utils.isQuestionTool(undefined), false);
});
