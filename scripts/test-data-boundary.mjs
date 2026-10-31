import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
const require = createRequire(import.meta.url);
const { ESLint } = require('eslint');
const eslint = new ESLint();
const samples = [
    ["import { invoke } from '@tauri-apps/api/core';", 'src/forbidden.ts'],
    ["import { openUrl } from '@tauri-apps/plugin-opener';", 'src/forbidden.ts'],
    ["const p = import('@tauri-apps/plugin-opener');", 'src/forbidden.ts'],
    ["const p = import('@tauri-apps/' + 'plugin-opener');", 'src/forbidden.ts'],
    ["<script lang=\"ts\">import { listen } from '@tauri-apps/api/event';</script>", 'src/Forbidden.svelte'],
    ["<script lang=\"ts\">const x = import('@tauri-apps/api/window');</script>", 'src/Forbidden.svelte']
];
for (const [code, filePath] of samples) {
    const [result] = await eslint.lintText(code, { filePath });
    assert(result.messages.some(m => ['no-restricted-imports','no-restricted-syntax'].includes(m.ruleId)), `${filePath}: import bypassed boundary`);
    assert(!result.messages.some(m => m.fatal), 'The rejection must be the boundary rule, not a parse error');
}
const [allowed] = await eslint.lintText("import { localCommand } from '$lib/data/platform';", { filePath: 'src/allowed.ts' });
assert.equal(allowed.errorCount, 0);
assert.equal(await eslint.isPathIgnored('src/lib/data/allowed.ts'), true);
const native = await eslint.lintText("import { invoke } from '@tauri-apps/api/core';", { filePath: 'src/lib/data/allowed.ts', warnIgnored: false });
assert(native.every(result => result.errorCount === 0));
console.log('PASS: data boundary rejects direct, dynamic, computed and Svelte Tauri/plugin imports; data layer is allowed');
