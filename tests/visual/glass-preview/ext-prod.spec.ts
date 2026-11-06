import { test, expect } from '@playwright/test';
import { openClassic, expectNoUnhandledCommands } from '../harness/classic';

// G6 in a production build (this suite serves `vite build`): the dev-only
// hello extension and the `/c9ext/*` dev endpoint never appear, and the
// missing `ext_list` command (G7) leaves the side bar and the route clean.
const glass = { storage: { 'ui.shell': 'glass', demoMode: 'true', 'c9watch.demoGlass': '1' } };

test('production build: no dev extension, no dev endpoint, clean fallback', async ({ page }) => {
	const asked: string[] = [];
	page.on('request', r => { if (new URL(r.url()).pathname.startsWith('/c9ext/')) asked.push(r.url()); });
	await openClassic(page, { ...glass, path: '/#/ext/hello/main' });
	await expect(page.locator('.glass-root')).toHaveAttribute('data-page', 'ext/hello/main');
	await expect(page.locator('.ext-page .state-msg')).toHaveText('沒有這個 extension 頁面');
	await expect(page.locator('[data-hello]')).toHaveCount(0);
	await expect(page.locator('.glass-side [data-slot="extensions"] a')).toHaveCount(0);
	expect(asked).toEqual([]);
	await expectNoUnhandledCommands(page);
	// The frame still works.
	await page.locator('.glass-side a', { hasText: '設定' }).click();
	await expect(page.locator('.glass-root')).toHaveAttribute('data-page', 'settings');
});
