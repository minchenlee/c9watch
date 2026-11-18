import { defineConfig, devices } from '@playwright/test';

// Glass interaction tests on the Vite dev server. Its own port, so it
// never meets the visual suites on 4179.
// Run: npx playwright test -c tests/glass/playwright.config.ts
const PORT = 4181;

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
		url: `http://127.0.0.1:${PORT}`,
		reuseExistingServer: false,
		timeout: 180_000,
		stdout: 'ignore',
		stderr: 'pipe'
	}
});
