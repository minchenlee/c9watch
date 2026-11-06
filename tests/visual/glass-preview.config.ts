import { defineConfig } from '@playwright/test';
import classic from './playwright.config';

/**
 * GLASS preview for owner and lead review. NOT a baseline suite: it compares
 * nothing and writes PNGs to tests/visual/.cache/glass-preview/ (git-ignored).
 * Run: npx playwright test -c tests/visual/glass-preview.config.ts
 */
export default defineConfig({ ...classic, testDir: './glass-preview', outputDir: './.cache/glass-preview-results' });
