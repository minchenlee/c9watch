import { createRequire } from 'node:module';
const require = createRequire(import.meta.url);
const ts = require('@typescript-eslint/parser');
const svelte = require('svelte-eslint-parser');
const rules = {
    'no-restricted-imports': ['error', {
        patterns: [{ group: ['@tauri-apps/api/*', '@tauri-apps/plugin-*', '@choochmeque/tauri-plugin-*'], message: 'Shells use src/lib/data/platform.ts or the shared c9p data client.' }]
    }],
    'no-restricted-syntax': ['error',
        { selector: 'ImportExpression[source.type!="Literal"]', message: 'Computed imports can bypass the data-layer boundary.' },
        { selector: 'ImportExpression[source.value=/^@tauri-apps\\//]', message: 'Tauri imports belong in src/lib/data/.' },
        { selector: 'ImportExpression[source.value=/^@choochmeque\\/tauri-plugin-/]', message: 'Tauri plugins belong in src/lib/data/.' }
    ]
};
export default [
    { ignores: ['src/lib/data/**'] },
    { files: ['src/**/*.ts'], languageOptions: { parser: ts }, rules },
    { files: ['src/**/*.svelte'], languageOptions: { parser: svelte, parserOptions: { parser: ts } }, rules }
];
