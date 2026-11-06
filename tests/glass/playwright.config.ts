import { defineConfig, devices } from '@playwright/test';
import { fileURLToPath } from 'node:url';

// Glass interaction tests that need the Vite dev server: the extension
// host (G6) loads extensions from the dev-only `/c9ext/*` endpoint, and
// the `dev: true` hello extension shows only in a dev build. Its own port,
// so it never meets the visual suites on 4179.
// Run: npx playwright test -c tests/glass/playwright.config.ts
const PORT = 4181;
const engineHome = fileURLToPath(new URL('./fixtures/engine-home', import.meta.url));

export default defineConfig({
	testDir: '.',
	outputDir: './.cache/test-results',
	fullyParallel: false,
	workers: 1,
	retries: 0,
	forbidOnly: !!process.env.CI,
	reporter: 'list',
	use: {
		...devices['Desktop Safari'],
		baseURL: `http://127.0.0.1:${PORT}`,
		headless: true,
		viewport: { width: 1100, height: 700 },
		deviceScaleFactor: 1,
		locale: 'en-US',
		timezoneId: 'UTC',
		colorScheme: 'dark',
		reducedMotion: 'reduce',
		trace: 'retain-on-failure'
	},
	projects: [{ name: 'webkit', use: { browserName: 'webkit' } }],
	webServer: {
		command: `npx vite dev --host 127.0.0.1 --port ${PORT} --strictPort`,
		cwd: '../..',
		// The fixture extensions (a throwing one, a silent one, bad manifests,
		// a shadowed copy of hello) sit in the third-party root.
		env: { C9WATCH_ENGINE_HOME: engineHome },
		url: `http://127.0.0.1:${PORT}`,
		reuseExistingServer: false,
		timeout: 180_000,
		stdout: 'ignore',
		stderr: 'pipe'
	}
});
