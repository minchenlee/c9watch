import { test, expect, type Page } from '@playwright/test';
import { fileURLToPath } from 'node:url';
import { openClassic, settle, expectNoUnhandledCommands } from '../harness/classic';

// G10b: native glass and Reduce Transparency. Preview only, no baseline.
// No Tauri here: a page background stands in for the desktop behind the
// window. The real window shows the macOS `sidebar` material (blurred and
// darkened), so these shots show the worst case, not the real look.
const out = (name: string) => fileURLToPath(new URL(`../.cache/glass-preview/${name}.png`, import.meta.url));
const glass = (mode: string) => ({ 'ui.shell': 'glass', demoMode: 'true', 'c9watch.demoGlass': '1', 'ui.glass': mode });
const VIBRANCY = { vibrancy: true, reduceTransparency: false };
const WALLPAPERS = {
	white: '#ffffff',
	photo: 'linear-gradient(135deg, #f7c873 0%, #e8603c 30%, #3a7bd5 60%, #9be15d 100%)'
};

async function wallpaper(page: Page, bg: string) {
	await page.addStyleTag({ content: `html.glass-native { background: ${bg} !important; }` });
}
const htmlNative = (page: Page) => page.evaluate(() => document.documentElement.classList.contains('glass-native'));
const bodyBg = (page: Page) => page.evaluate(() => getComputedStyle(document.body).backgroundColor);

test.describe('glass G10b native glass (not a baseline)', () => {
	test('Classic main and popover keep an opaque black body in a transparent window', async ({ page }) => {
		for (const path of ['/', '/popover']) {
			await openClassic(page, { path, window: path === '/' ? 'main' : 'popover', nativeGlass: VIBRANCY });
			await expectNoUnhandledCommands(page);
			expect(await htmlNative(page), `${path}: html.glass-native`).toBe(false);
			expect(await bodyBg(page), `${path}: body background`).toBe('rgb(0, 0, 0)');
		}
	});

	test('glass without vibrancy (vite dev, not macOS) keeps the opaque body', async ({ page }) => {
		await openClassic(page, { path: '/#/settings', storage: glass('subtle') });
		expect(await htmlNative(page)).toBe(false);
		expect(await bodyBg(page)).toBe('rgb(0, 0, 0)');
	});

	test('glass with vibrancy: transparent body; switching to Classic restores it', async ({ page }) => {
		await openClassic(page, { path: '/#/settings', storage: glass('subtle'), nativeGlass: VIBRANCY });
		await expectNoUnhandledCommands(page);
		expect(await htmlNative(page)).toBe(true);
		expect(await bodyBg(page)).toBe('rgba(0, 0, 0, 0)');
		await expect(page.locator('.glass-root')).toHaveAttribute('data-glass', 'subtle');
		await page.locator('[data-shell="classic"]').click();
		await page.locator('[data-confirm="classic"]').click();
		await settle(page);
		await expect(page.locator('.glass-root')).toHaveCount(0);
		expect(await htmlNative(page)).toBe(false);
		expect(await bodyBg(page)).toBe('rgb(0, 0, 0)');
	});

	for (const mode of ['off', 'subtle', 'full'] as const) {
		for (const [name, bg] of Object.entries(WALLPAPERS)) {
			test(`Glass ${mode} on a ${name} desktop`, async ({ page }) => {
				await page.setViewportSize({ width: 1100, height: 700 });
				await openClassic(page, { path: '/#/ws/c9watch', storage: glass(mode), nativeGlass: VIBRANCY });
				await wallpaper(page, bg);
				await expect(page.locator('.glass-root')).toHaveAttribute('data-glass', mode);
				const tint = await page.evaluate(() => getComputedStyle(document.querySelector('.glass-root')!).backgroundColor);
				if (mode === 'off') expect(tint, 'Off is opaque').not.toMatch(/\/ 0\.|, 0\.\d+\)$/);
				else expect(tint, `${mode} lets the desktop through`).toMatch(/\/ 0\.\d+\)|, 0\.\d+\)$/);
				await page.mouse.move(0, 0);
				await settle(page);
				await page.screenshot({ path: out(`g10b-${mode}-${name}`) });
			});
		}
	}

	test('Reduce Transparency forces Off over Full, and the next focus reads it again', async ({ page }) => {
		await page.setViewportSize({ width: 1100, height: 900 });
		await openClassic(page, {
			path: '/#/settings',
			storage: glass('full'),
			nativeGlass: { vibrancy: true, reduceTransparency: true }
		});
		await wallpaper(page, WALLPAPERS.photo);
		const root = page.locator('.glass-root');
		await expect(root).toHaveAttribute('data-glass', 'off');
		// The pref is not changed: Full stays selected, and the note says why.
		await expect(page.locator('[data-glass-btn="full"]')).toHaveAttribute('aria-checked', 'true');
		await expect(page.locator('[data-reduce-transparency]')).toContainText('減少透明度');
		await page.mouse.move(0, 0);
		await settle(page);
		await page.screenshot({ path: out('g10b-reduce-transparency') });
		expect(await page.evaluate(() => localStorage.getItem('ui.glass'))).toBe('full');

		// The owner turns Reduce Transparency off in System Settings and comes back.
		await page.evaluate(() => {
			window.__C9_VISUAL__!.nativeGlass = { vibrancy: true, reduceTransparency: false };
			window.dispatchEvent(new Event('focus'));
		});
		await settle(page);
		await expect(root).toHaveAttribute('data-glass', 'full');
		await expect(page.locator('[data-reduce-transparency]')).toHaveCount(0);
	});
});
