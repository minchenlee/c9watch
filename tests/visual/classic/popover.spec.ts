import { test } from '@playwright/test';
import { openClassic, snap } from '../harness/classic';

test.describe('menu bar popover', () => {
	test.use({ viewport: { width: 320, height: 400 } });

	test('live sessions', async ({ page }) => {
		await openClassic(page, { path: '/popover', window: 'popover' });
		await snap(page, 'popover');
	});

	test('no sessions', async ({ page }) => {
		await openClassic(page, { path: '/popover', window: 'popover', sessions: 'empty' });
		await snap(page, 'popover-empty');
	});

	test('demo mode', async ({ page }) => {
		await openClassic(page, { path: '/popover', window: 'popover', storage: { demoMode: 'true' } });
		await snap(page, 'popover-demo');
	});
});
