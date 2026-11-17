import { defineConfig } from '@playwright/test';
import classic from './playwright.config';

/**
 * GLASS preview for owner and lead review. NOT a baseline suite: it compares
 * nothing and writes PNGs to tests/visual/.cache/glass-preview/ (git-ignored).
 * Glass is off in a plain build (D43), so the server is built with VITE_C9_GLASS=1.
 * Run: npx playwright test -c tests/visual/glass-preview.config.ts
 */
// The web server (`vite build`) inherits this process's environment.
process.env.VITE_C9_GLASS = '1';
export default defineConfig({ ...classic, testDir: './glass-preview', outputDir: './.cache/glass-preview-results' });
