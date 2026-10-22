import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { test } from 'node:test';

const url = path => new URL(path, import.meta.url);
const read = path => readFileSync(url(path), 'utf8');

test('removed dead components and barrel files stay deleted', () => {
  for (const path of [
    '../src/lib/components/SessionItem.svelte',
    '../src/lib/components/SessionList.svelte',
    '../src/lib/components/PromptInput.svelte',
    '../src/lib/components/index.ts',
    '../src/lib/index.ts'
  ]) {
    assert.equal(existsSync(url(path)), false, `${path} should not exist`);
  }
});

test('sessions store does not export the unused statusSummary or selectedSessionId', () => {
  const sessions = read('../src/lib/stores/sessions.ts');
  assert.doesNotMatch(sessions, /export const statusSummary\b/);
  assert.doesNotMatch(sessions, /export const selectedSessionId\b/);
});

test('monitor page keeps StatusBar fed by filteredSummary', () => {
  const page = read('../src/routes/(app)/+page.svelte');
  assert.doesNotMatch(page, /statusSummary/);
  assert.match(page, /<StatusBar[^>]*summary=\{filteredSummary\}/);
});

test('library README does not import from the removed $lib barrel', () => {
  const readme = read('../src/lib/README.md');
  assert.doesNotMatch(readme, /from '\$lib'/);
  assert.doesNotMatch(readme, /selectedSessionId/);
});
