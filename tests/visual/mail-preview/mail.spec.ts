import { test, expect } from '@playwright/test';
import { fileURLToPath } from 'node:url';
import { openClassic, settle, expectNoUnhandledCommands } from '../harness/classic';

// Demo mode with demo mail opted in. Classic demo baselines keep it off.
const demo = { storage: { demoMode: 'true', 'c9watch.demoMail': '1' } };
const out = (name: string) => fileURLToPath(new URL(`../.cache/mail-preview/${name}.png`, import.meta.url));

async function shot(page: import('@playwright/test').Page, name: string) {
	await expectNoUnhandledCommands(page);
	await page.mouse.move(0, 0);
	await settle(page);
	await page.screenshot({ path: out(name) });
}

test.describe('mail preview (not a baseline)', () => {
	test('monitor card mail line', async ({ page }) => {
		await page.setViewportSize({ width: 900, height: 1100 });
		await openClassic(page, demo);
		await expect(page.locator('.mail-line').first()).toBeVisible();
		await shot(page, 'mail-card-line');
	});

	test('mail tab thread with every state', async ({ page }) => {
		await page.setViewportSize({ width: 1100, height: 900 });
		await openClassic(page, demo);
		await page.keyboard.press('ControlOrMeta+6');
		await settle(page);
		await expect(page.locator('.thread')).toBeVisible();
		await shot(page, 'mail-tab-pm');
		await page.locator('.agent-item', { hasText: 'worker-api' }).click();
		await settle(page);
		await shot(page, 'mail-tab-worker');
	});

	test('mail tab narrow window', async ({ page }) => {
		await page.setViewportSize({ width: 600, height: 800 });
		await openClassic(page, demo);
		await page.keyboard.press('ControlOrMeta+6');
		await settle(page);
		await shot(page, 'mail-tab-narrow');
	});
});
