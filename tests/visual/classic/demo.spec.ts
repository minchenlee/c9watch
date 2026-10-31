import { test } from '@playwright/test';
import { openClassic, openTab, settle, snap } from '../harness/classic';

// Demo mode (isDemoMode, src/lib/demo/). The paused clock keeps its status
// simulation from firing, so the demo data stays in its first state.
const demo = { storage: { demoMode: 'true' } };

test.describe('demo mode', () => {
	test('monitor', async ({ page }) => {
		await page.setViewportSize({ width: 900, height: 1500 });
		await openClassic(page, demo);
		await snap(page, 'demo-monitor');
	});

	test('monitor, default window', async ({ page }) => {
		await openClassic(page, demo);
		await snap(page, 'demo-monitor-window');
	});

	test('expanded card overlay', async ({ page }) => {
		await page.setViewportSize({ width: 900, height: 1500 });
		await openClassic(page, demo);
		await page.locator('.session-card').first().click();
		await page.setViewportSize({ width: 900, height: 600 });
		await settle(page);
		await snap(page, 'demo-overlay');
	});

	for (const tab of ['HISTORY', 'COST'] as const) {
		test(tab.toLowerCase(), async ({ page }) => {
			await openClassic(page, demo);
			await openTab(page, tab);
			await snap(page, `demo-${tab.toLowerCase()}`);
		});
	}

	test('narrow window', async ({ page }) => {
		await page.setViewportSize({ width: 600, height: 700 });
		await openClassic(page, demo);
		await snap(page, 'demo-narrow');
	});
});
