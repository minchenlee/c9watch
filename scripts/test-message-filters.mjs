import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import ts from 'typescript';

const source = readFileSync(new URL('../src/lib/message-filters.ts', import.meta.url), 'utf8');
const code = ts.transpileModule(source, {
  compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022 }
}).outputText;
const { isMessageVisible, isNavItem } = await import('data:text/javascript;base64,' + Buffer.from(code).toString('base64'));

const types = ['User', 'Assistant', 'Thinking', 'ToolUse', 'ToolResult', 'System'];
// [showTools, showThinking]
const flags = [[false, false], [false, true], [true, false], [true, true]];

// Reference copies of the inline expressions the overlays used before.
const oldVisible = (m, showTools, showThinking) =>
  (showTools || (m.messageType !== 'ToolUse' && m.messageType !== 'ToolResult')) &&
  (showThinking || m.messageType !== 'Thinking');
const oldNav = (m, showTools, showThinking) =>
  m.messageType === 'User' ||
  m.messageType === 'System' ||
  (showThinking && m.messageType === 'Thinking') ||
  (showTools && m.messageType === 'ToolUse' && m.content?.length > 0);

test('isMessageVisible truth table', () => {
  const expected = {
    User: [true, true, true, true],
    Assistant: [true, true, true, true],
    Thinking: [false, true, false, true],
    ToolUse: [false, false, true, true],
    ToolResult: [false, false, true, true],
    System: [true, true, true, true]
  };
  for (const messageType of types) {
    flags.forEach(([showTools, showThinking], i) => {
      const m = { messageType, content: 'x' };
      const label = `${messageType} tools=${showTools} thinking=${showThinking}`;
      assert.equal(isMessageVisible(m, showTools, showThinking), expected[messageType][i], label);
      assert.equal(isMessageVisible(m, showTools, showThinking), oldVisible(m, showTools, showThinking), label);
    });
  }
});

test('isNavItem truth table', () => {
  const expected = {
    User: [true, true, true, true],
    Assistant: [false, false, false, false],
    Thinking: [false, true, false, true],
    ToolUse: [false, false, true, true],
    ToolResult: [false, false, false, false],
    System: [true, true, true, true]
  };
  for (const messageType of types) {
    for (const content of ['x', '', undefined]) {
      flags.forEach(([showTools, showThinking], i) => {
        const m = { messageType, content };
        const want = messageType === 'ToolUse' && !content ? false : expected[messageType][i];
        const label = `${messageType} content=${content} tools=${showTools} thinking=${showThinking}`;
        assert.equal(isNavItem(m, showTools, showThinking), want, label);
        assert.equal(isNavItem(m, showTools, showThinking), oldNav(m, showTools, showThinking), label);
      });
    }
  }
});
