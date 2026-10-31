import { test, type Page } from '@playwright/test';
import { openClassic, settle, snap } from '../harness/classic';

const card = (page: Page, title: string) => page.locator('.session-card', { hasText: title });

test.describe('monitor tab', () => {
	test('grouped by project', async ({ page }) => {
		await openClassic(page);
		await snap(page, 'monitor-project');
	});

	test('grouped by project, tall window shows every card', async ({ page }) => {
		await page.setViewportSize({ width: 900, height: 1500 });
		await openClassic(page);
		await snap(page, 'monitor-project-tall');
	});

	test('grouped by status', async ({ page }) => {
		await page.setViewportSize({ width: 900, height: 1500 });
		await openClassic(page, { storage: { sessionViewMode: 'all' } });
		await snap(page, 'monitor-all');
	});

	test('dense board', async ({ page }) => {
		await openClassic(page, { storage: { sessionViewMode: 'dense' } });
		await snap(page, 'monitor-dense');
	});

	test('dense board, one line per session', async ({ page }) => {
		await openClassic(page, { storage: { sessionViewMode: 'dense', sessionViewCompact: 'true' } });
		await snap(page, 'monitor-dense-compact');
	});

	test('compact cards', async ({ page }) => {
		await page.setViewportSize({ width: 900, height: 1100 });
		await openClassic(page, { storage: { sessionViewCompact: 'true' } });
		await snap(page, 'monitor-compact');
	});

	test('no sessions', async ({ page }) => {
		await openClassic(page, { sessions: 'empty' });
		await snap(page, 'monitor-empty');
	});

	test('narrow window', async ({ page }) => {
		await page.setViewportSize({ width: 600, height: 700 });
		await openClassic(page);
		await snap(page, 'monitor-narrow');
	});

	test('light system color scheme', async ({ page }) => {
		await page.emulateMedia({ colorScheme: 'light' });
		await openClassic(page);
		await snap(page, 'monitor-light-scheme');
	});

	test('in-app toast after a failed stop', async ({ page }) => {
		await openClassic(page, { stopError: 'The session process changed. Refresh and try again.' });
		await card(page, 'Rate limiter cache backend').getByRole('button', { name: 'STOP' }).click();
		await settle(page);
		await snap(page, 'monitor-toast');
	});
});

test.describe('expanded card overlay', () => {
	test('long conversation with subagents and tasks', async ({ page }) => {
		await openClassic(page);
		await page.setViewportSize({ width: 900, height: 1500 });
		await card(page, 'Dashboard table perf').click();
		await settle(page);
		await page.setViewportSize({ width: 900, height: 600 });
		await settle(page);
		await snap(page, 'overlay-long-conversation');
	});

	test('approval session', async ({ page }) => {
		await openClassic(page);
		await card(page, 'OAuth2 login with Google and GitHub').click();
		await settle(page);
		await snap(page, 'overlay-approval');
	});

	test('narrow window', async ({ page }) => {
		await openClassic(page);
		await page.setViewportSize({ width: 900, height: 1500 });
		await card(page, 'Dashboard table perf').click();
		await page.setViewportSize({ width: 600, height: 700 });
		await settle(page);
		await snap(page, 'overlay-narrow');
	});
});
