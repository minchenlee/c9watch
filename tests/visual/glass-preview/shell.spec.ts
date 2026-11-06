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

/** Records every Tauri invoke. Runs before the IPC mock bundle, so the mock's
 * `invoke` goes through the setter and is wrapped. */
async function recordInvokes(page: import('@playwright/test').Page) {
	await page.addInitScript(() => {
		const calls: { cmd: string; args: any }[] = ((window as any).__C9_CALLS__ = []);
		let inner: ((cmd: string, args: any, o?: unknown) => unknown) | undefined;
		const internals: Record<string, unknown> = {};
		Object.defineProperty(internals, 'invoke', {
			configurable: true,
			get: () => (cmd: string, args: any, o?: unknown) => { calls.push({ cmd, args }); return inner!(cmd, args, o); },
			set: (fn) => { inner = fn; }
		});
		(window as any).__TAURI_INTERNALS__ = internals;
	});
}
const sessionFeeds = (page: import('@playwright/test').Page) => page.evaluate(() =>
	((window as any).__C9_CALLS__ as { cmd: string; args: any }[]).filter(c =>
		c.cmd === 'get_sessions' ||
		(c.cmd === 'c9p_subscribe' && c.args?.params?.topics?.includes('session.view')) ||
		(c.cmd === 'c9p_request' && c.args?.method === 'session.views')).map(c => c.cmd));

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

	test('side bar: needs count, lead and talk rows, no office entry', async ({ page }) => {
		await page.setViewportSize({ width: 1100, height: 700 });
		await openClassic(page, glass);
		const side = page.locator('.glass-side');
		await expect(side.locator('[data-nav="needs"] .need-n')).toHaveText('5');
		await expect(side.getByText('lead-terms', { exact: true })).toBeVisible();
		await expect(side.getByText('對談 · main', { exact: true })).toBeVisible();
		await expect(side.getByText('辦公室平面圖')).toHaveCount(0);
		await expect(side.locator('[data-slot="extensions"]')).toBeEmpty();
		await shot(page, 'g1-sidebar-open');
	});

	test('[ collapses the side bar to a "需要你 · N" button and back', async ({ page }) => {
		await page.setViewportSize({ width: 1100, height: 700 });
		await openClassic(page, glass);
		await page.keyboard.press('BracketLeft');
		await expect(page.locator('.glass-side')).toHaveCount(0);
		await expect(page.locator('.reopen')).toHaveText(/需要你 · 5/);
		await shot(page, 'g1-sidebar-collapsed');
		await page.locator('.reopen').click();
		await expect(page.locator('.glass-side')).toBeVisible();
	});

	test('⌘N shows the spawn hint; Esc closes it', async ({ page }) => {
		await page.setViewportSize({ width: 1100, height: 700 });
		await openClassic(page, glass);
		await page.keyboard.press('Meta+KeyN');
		await expect(page.locator('.hint')).toContainText('c9watch spawn');
		await shot(page, 'g1-delegate-hint');
		await page.keyboard.press('Escape');
		await expect(page.locator('.hint')).toHaveCount(0);
	});

	test('Glass Off: solid surfaces', async ({ page }) => {
		await page.setViewportSize({ width: 1100, height: 700 });
		await openClassic(page, { storage: { ...glass.storage, 'ui.glass': 'off' } });
		await expect(page.locator('.glass-root')).toHaveAttribute('data-glass', 'off');
		await shot(page, 'g1-glass-off');
	});

	test('hash opens a page directly', async ({ page }) => {
		await openClassic(page, { ...glass, path: '/#/ws/c9watch' });
		await expect(page.locator('.glass-root')).toHaveAttribute('data-page', 'ws/c9watch');
		await page.locator('.glass-side a', { hasText: '設定' }).click();
		await expect(page.locator('.glass-root')).toHaveAttribute('data-page', 'settings');
	});

	test('persisted glass starts one session feed and no Classic feed', async ({ page }) => {
		await recordInvokes(page);
		await openClassic(page, { storage: { 'ui.shell': 'glass' } });
		await expect(page.locator('.glass-root')).toHaveAttribute('data-sessions', /[1-9]/);
		await expectNoUnhandledCommands(page);
		// Glass: one session.views snapshot and one session.view stream.
		// Classic would add get_sessions and a second stream.
		expect(await sessionFeeds(page)).toEqual(['c9p_request', 'c9p_subscribe']);
	});

	test('live mode shows no demo workspaces', async ({ page }) => {
		await openClassic(page, { storage: { 'ui.shell': 'glass' } });
		const side = page.locator('.glass-side');
		await expect(side.locator('.state-msg')).toHaveText('還沒有 workspace');
		for (const name of ['C9WATCH', 'FLOCKBOOK', 'JUMPMARK', '跨專案']) await expect(side.getByText(name, { exact: true })).toHaveCount(0);
		await shot(page, 'glass-frame-live');
	});

	test('demo mode without demoGlass shows no demo workspaces', async ({ page }) => {
		await openClassic(page, { storage: { 'ui.shell': 'glass', demoMode: 'true' } });
		await expect(page.locator('.glass-side .state-msg')).toBeVisible();
		await expect(page.locator('.glass-side').getByText('C9WATCH', { exact: true })).toHaveCount(0);
	});

	test('popover stays Classic in glass mode', async ({ page }) => {
		await openClassic(page, { path: '/popover', window: 'popover', storage: { 'ui.shell': 'glass' } });
		await expect(page.locator('.glass-root')).toHaveCount(0);
		await shot(page, 'popover-in-glass-mode');
	});
});
