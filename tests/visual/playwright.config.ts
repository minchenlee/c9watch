import { defineConfig, devices } from '@playwright/test';

const PORT = 4179;

/**
 * Classic visual harness. WebKit only, headless only.
 *
 * C9_VISUAL_SNAPSHOT_DIR moves the snapshot folder (used to compare two
 * local runs without touching tests/visual/classic/__screenshots__).
 */
const snapshotDir = process.env.C9_VISUAL_SNAPSHOT_DIR ?? '{testDir}/__screenshots__';

export default defineConfig({
	testDir: './classic',
	outputDir: './.cache/test-results',
	globalSetup: './harness/global-setup.ts',
	snapshotPathTemplate: `${snapshotDir}/{arg}{ext}`,
	fullyParallel: false,
	workers: 1,
	retries: 0,
	forbidOnly: !!process.env.CI,
	reporter: process.env.CI ? [['list'], ['html', { open: 'never', outputFolder: './.cache/report' }]] : 'list',
	expect: {
		toHaveScreenshot: {
			maxDiffPixels: 0,
			threshold: 0,
			animations: 'disabled',
			caret: 'hide',
			scale: 'css'
		}
	},
	use: {
		...devices['Desktop Safari'],
		baseURL: `http://127.0.0.1:${PORT}`,
		headless: true,
		viewport: { width: 900, height: 600 },
		deviceScaleFactor: 1,
		locale: 'en-US',
		timezoneId: 'UTC',
		colorScheme: 'dark',
		reducedMotion: 'reduce',
		trace: 'retain-on-failure'
	},
	projects: [{ name: 'webkit', use: { browserName: 'webkit' } }],
	webServer: {
		// The static build, served the way Tauri loads it. No Tauri, no window.
		command: `npx vite build && npx vite preview --host 127.0.0.1 --port ${PORT} --strictPort`,
		cwd: '../..',
		url: `http://127.0.0.1:${PORT}`,
		reuseExistingServer: false,
		timeout: 180_000,
		stdout: 'ignore',
		stderr: 'pipe'
	}
});
