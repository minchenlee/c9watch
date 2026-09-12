import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';

const settings = readFileSync(new URL('../src/lib/components/SettingsTab.svelte', import.meta.url), 'utf8');
const integration = readFileSync(new URL('../src/lib/components/IntegrationSettings.svelte', import.meta.url), 'utf8');
const appPage = readFileSync(new URL('../src/routes/(app)/+page.svelte', import.meta.url), 'utf8');

test('Settings exposes a shared Integration section instead of an OpenCode section', () => {
  assert.match(settings, /IntegrationSettings/);
  assert.match(settings, /activeSection === 'integration'/);
  assert.doesNotMatch(settings, /activeSection === 'opencode'/);
  assert.match(settings, />Integration<\/button>/);
  assert.doesNotMatch(settings, />OpenCode<\/button>/);
});

test('Integration section groups local providers and the configurable OpenCode connection', () => {
  for (const provider of ['Claude Code', 'Codex', 'Cursor', 'Pi']) assert.match(integration, new RegExp(provider));
  assert.match(integration, /Local integrations/);
  assert.match(integration, /Automatic detection/);
  assert.match(integration, /<OpenCodeConnection \/>/);
  assert.match(integration, /Remote integrations/);
});

test('Codex Desktop support lives alongside the unchanged OpenCode connection', () => {
  assert.match(integration, /<CodexDesktopSupport \/>/);
  assert.match(integration, /<OpenCodeConnection \/>/);
  assert.doesNotMatch(settings, /activeSection === 'codex'/);
  assert.match(integration, /Optional Desktop messaging support/);
});

test('Settings content uses the available width and aligns with the tab bar', () => {
  assert.match(appPage, /<main class="grid-container history-main settings-main"/);
  assert.match(appPage, /\.history-main\.settings-main\s*\{\s*padding-right:\s*var\(--space-md\);/);
  assert.match(settings, /\.settings-body \{[^}]*max-width:\s*none;[^}]*padding:\s*0 0 var\(--space-2xl\) var\(--space-lg\)/s);
});
