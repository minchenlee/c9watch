import { test, expect } from '@playwright/test';
import { fileURLToPath } from 'node:url';
import { openClassic, settle, expectNoUnhandledCommands } from '../harness/classic';

// Glass shell on the glass demo fixture. Classic baselines never set these keys.
const glass = { storage: { 'ui.shell': 'glass', demoMode: 'true', 'c9watch.demoGlass': '1' } };
const out = (name: string) => fileURLToPath(new URL(`../.cache/glass-preview/${name}.png`, import.meta.url));

async function shot(page: import('@playwright/test').Page, name: string) {
	await expectNoUnhandledCommands(page);
	await page.mouse.move(0, 0);
	await settle(page);
	await page.screenshot({ path: out(name) });
}

test.describe('glass preview (not a baseline)', () => {
	test('empty glass frame on the demo fixture', async ({ page }) => {
		await page.setViewportSize({ width: 1100, height: 700 });
		await openClassic(page, glass);
		const root = page.locator('.glass-root');
		await expect(root).toHaveAttribute('data-page', 'needs');
		// 22 agents, 19 live sessions (3 retired), 3 annotations.
		await expect(root).toHaveAttribute('data-agents', '22');
		await expect(root).toHaveAttribute('data-sessions', '19');
		await expect(root).toHaveAttribute('data-annotations', '3');
		await shot(page, 'glass-frame');
	});

	test('hash opens a page directly', async ({ page }) => {
		await openClassic(page, { ...glass, path: '/#/ws/c9watch' });
		await expect(page.locator('.glass-root')).toHaveAttribute('data-page', 'ws/c9watch');
		await page.locator('.glass-side a', { hasText: 'Settings' }).click();
		await expect(page.locator('.glass-root')).toHaveAttribute('data-page', 'settings');
	});

	test('popover stays Classic in glass mode', async ({ page }) => {
		await openClassic(page, { path: '/popover', window: 'popover', storage: { 'ui.shell': 'glass' } });
		await expect(page.locator('.glass-root')).toHaveCount(0);
		await shot(page, 'popover-in-glass-mode');
	});
});
