import { defineConfig } from '@playwright/test';
import classic from './playwright.config';

/**
 * MAIL preview for owner review. NOT a baseline suite: it compares nothing
 * and writes PNGs to tests/visual/.cache/mail-preview/ (git-ignored).
 * Run: npx playwright test -c tests/visual/mail-preview.config.ts
 */
export default defineConfig({ ...classic, testDir: './mail-preview', outputDir: './.cache/mail-preview-results' });
