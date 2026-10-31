import { test } from '@playwright/test';
import { openClassic, openTab, settle, snap } from '../harness/classic';

test.describe('history tab', () => {
	test('flat list', async ({ page }) => {
		await openClassic(page);
		await openTab(page, 'HISTORY');
		await snap(page, 'history');
	});

	test('grouped by project', async ({ page }) => {
		await openClassic(page, { storage: { historyGroup: 'true' } });
		await openTab(page, 'HISTORY');
		await snap(page, 'history-grouped');
	});

	test('history overlay', async ({ page }) => {
		await openClassic(page);
		await openTab(page, 'HISTORY');
		await page.locator('.session-row', { hasText: 'Saved title' }).first().waitFor();
		await page.locator('.session-row').filter({ hasText: 'web-app' }).filter({ hasText: 'Add OAuth2 login' }).last().click();
		await settle(page);
		await snap(page, 'history-overlay');
	});

	test('narrow window', async ({ page }) => {
		await page.setViewportSize({ width: 600, height: 700 });
		await openClassic(page);
		await openTab(page, 'HISTORY');
		await snap(page, 'history-narrow');
	});
});

test.describe('cost tab', () => {
	test('daily, USD', async ({ page }) => {
		await openClassic(page);
		await openTab(page, 'COST');
		await snap(page, 'cost-daily');
	});

	test('daily, tall window shows projects', async ({ page }) => {
		await page.setViewportSize({ width: 900, height: 1500 });
		await openClassic(page);
		await openTab(page, 'COST');
		await snap(page, 'cost-daily-tall');
	});

	test('tokens', async ({ page }) => {
		await openClassic(page, { storage: { 'c9watch.costMode': 'tokens' } });
		await openTab(page, 'COST');
		await snap(page, 'cost-tokens');
	});

	test('narrow window', async ({ page }) => {
		await page.setViewportSize({ width: 600, height: 700 });
		await openClassic(page);
		await openTab(page, 'COST');
		await snap(page, 'cost-narrow');
	});
});

test.describe('memory tab', () => {
	test('first project', async ({ page }) => {
		await openClassic(page);
		await openTab(page, 'MEMORY');
		await snap(page, 'memory');
	});

	test('file expanded', async ({ page }) => {
		await openClassic(page);
		await openTab(page, 'MEMORY');
		await page.getByRole('button', { name: /auth\.md/ }).click();
		await settle(page);
		await snap(page, 'memory-file-open');
	});

	test('narrow window', async ({ page }) => {
		await page.setViewportSize({ width: 600, height: 700 });
		await openClassic(page);
		await openTab(page, 'MEMORY');
		await snap(page, 'memory-narrow');
	});
});

test.describe('settings tab', () => {
	for (const section of ['Notifications', 'Usage', 'Integration', 'Remote access', 'About']) {
		test(section, async ({ page }) => {
			await openClassic(page);
			await openTab(page, 'SETTINGS');
			await page.locator('.settings-nav button', { hasText: section }).click();
			await settle(page);
			await snap(page, `settings-${section.toLowerCase().replace(/\s+/g, '-')}`);
		});
	}
});
