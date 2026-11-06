import { test, expect, type Page } from '@playwright/test';
import { fileURLToPath } from 'node:url';

// Extension host v1 (G6) on `vite dev`, in the browser (no Tauri), on the
// glass demo fixture. Playwright CSS locators pierce open shadow roots, so
// `[data-hello=...]` finds hello's elements inside its shadow root.
const STORAGE = { 'ui.shell': 'glass', demoMode: 'true', 'c9watch.demoGlass': '1' };
const out = (name: string) => fileURLToPath(new URL(`./.cache/${name}.png`, import.meta.url));

/** Seeds localStorage once per test, so a reload keeps what the page wrote. */
async function open(page: Page, hash: string) {
	await page.addInitScript((storage) => {
		if (sessionStorage.getItem('c9-test-seeded')) return;
		sessionStorage.setItem('c9-test-seeded', '1');
		localStorage.clear();
		for (const [k, v] of Object.entries(storage)) localStorage.setItem(k, v);
	}, STORAGE);
	await page.goto(`/${hash}`);
	await expect(page.locator('.glass-root')).toBeVisible();
}
const hello = (page: Page, name: string) => page.locator(`[data-hello="${name}"]`);
const frameWorks = async (page: Page) => {
	await page.locator('.glass-side a', { hasText: '設定' }).click();
	await expect(page.locator('.glass-root')).toHaveAttribute('data-page', 'settings');
};

test.describe('extension host v1', () => {
	test('hello mounts in a shadow root and reads c9p data', async ({ page }) => {
		await open(page, '#/ext/hello/main');
		await expect(page.locator('.ext-mount')).toHaveAttribute('data-ext-state', 'ready');
		await expect(hello(page, 'sessions')).toHaveText('19');
		const inShadow = await page.locator('.ext-box[data-c9ext-host="hello"]').evaluate(el => !!el.shadowRoot?.querySelector('[data-hello="sessions"]'));
		expect(inShadow).toBe(true);
		await expect(page.locator('.ext-page h1')).toHaveText('Hello');
		await page.mouse.move(0, 0);
		await page.screenshot({ path: out('g6-hello') });
	});

	test('side bar: first-party wins, broken and too-new extensions are not listed', async ({ page }) => {
		await open(page, '#/needs');
		const slot = page.locator('.glass-side [data-slot="extensions"]');
		await expect(slot.locator('a')).toHaveText(['Hello', 'Silent', 'Throws']);
		await expect(slot.locator('[data-ext="hello"]')).toHaveAttribute('data-source', 'first');
		await expect(slot.getByText('Shadowed hello')).toHaveCount(0);
		await slot.locator('[data-ext="hello"]').click();
		await expect(page.locator('.glass-root')).toHaveAttribute('data-page', 'ext/hello/main');
		await expect(hello(page, 'sessions')).toHaveText('19');
		await expect(page.locator('[data-shadowed]')).toHaveCount(0);
		await expect(slot.locator('[data-ext="hello"]')).toHaveAttribute('aria-current', 'page');
	});

	test('a route to a refused extension says why', async ({ page }) => {
		await open(page, '#/ext/bad-format/main');
		await expect(page.locator('.ext-page .state-msg')).toHaveText('manifest 版本不支援');
		await open(page, '#/ext/later/main');
		await expect(page.locator('.ext-page .state-msg')).toHaveText('需要較新的 c9watch（99.0.0 以上）');
		await open(page, '#/ext/nope/main');
		await expect(page.locator('.ext-page .state-msg')).toHaveText('沒有這個 extension 頁面');
	});

	test('theme tokens reach the extension (CSS and host.theme)', async ({ page }) => {
		await open(page, '#/ext/hello/main');
		await expect(hello(page, 'token')).toHaveText('#ececee');
		await expect(hello(page, 'glass')).toHaveText('subtle');
		// CSS custom properties inherit into the shadow root.
		const color = await hello(page, 'sessions').evaluate(el => getComputedStyle(el).color);
		expect(color).toBe('rgb(236, 236, 238)');
	});

	test('prefs survive a reload', async ({ page }) => {
		await open(page, '#/ext/hello/main');
		await expect(hello(page, 'clicks')).toHaveText('0');
		await hello(page, 'click').click();
		await hello(page, 'click').click();
		await expect(hello(page, 'clicks')).toHaveText('2');
		expect(await page.evaluate(() => localStorage.getItem('c9ext.hello.prefs'))).toBe('{"clicks":2}');
		await page.reload();
		await expect(hello(page, 'clicks')).toHaveText('2');
	});

	test('intent navigate changes the page; coming back resumes the same instance', async ({ page }) => {
		await open(page, '#/ext/hello/main');
		await hello(page, 'click').click();
		await hello(page, 'go').click();
		await expect(page.locator('.glass-root')).toHaveAttribute('data-page', 'needs');
		await expect(page.locator('.ext-page')).toBeHidden();
		await expect(hello(page, 'state')).toHaveText('suspended');
		await page.locator('.glass-side [data-ext="hello"]').click();
		await expect(hello(page, 'state')).toHaveText('running');
		// No reload and no new mount: the in-memory count is still there.
		await expect(hello(page, 'clicks')).toHaveText('1');
	});

	test('J / K are handled by the host, also when hello has focus', async ({ page }) => {
		await open(page, '#/ext/hello/main');
		await expect(hello(page, 'sessions')).toHaveText('19');
		await expect(hello(page, 'needs')).toHaveText('—');
		// Focus inside the shadow root: hello forwards the key to the host,
		// which steps context.needsItemId through the needs queue.
		await hello(page, 'click').focus();
		await page.keyboard.press('KeyJ');
		const first = await hello(page, 'needs').textContent();
		expect(first).not.toBe('—');
		await page.keyboard.press('KeyJ');
		const second = await hello(page, 'needs').textContent();
		expect(second).not.toBe(first);
		await page.keyboard.press('KeyK');
		await expect(hello(page, 'needs')).toHaveText(first!);
		// Typing in the extension's own field is typing, not J / K.
		await hello(page, 'input').focus();
		await page.keyboard.type('jk');
		await expect(hello(page, 'input')).toHaveValue('jk');
		await expect(hello(page, 'needs')).toHaveText(first!);
		// Outside the extension the host still has J.
		await page.locator('.ext-page h1').click();
		await page.keyboard.press('KeyJ');
		await expect(hello(page, 'needs')).toHaveText(second!);
	});

	test('a throwing extension leaves the host frame working', async ({ page }) => {
		const errors: string[] = [];
		page.on('pageerror', e => errors.push(e.message));
		await open(page, '#/ext/throws/main');
		await expect(page.locator('.ext-mount')).toHaveAttribute('data-ext-state', 'stopped');
		await expect(page.locator('.ext-mount [role="alert"]')).toContainText('extension 停止了');
		await expect(page.locator('.ext-mount [role="alert"]')).toContainText('fixture: mount failed on purpose');
		await page.screenshot({ path: out('g6-stopped') });
		// The shell keys and the side bar still work.
		await page.keyboard.press('BracketLeft');
		await expect(page.locator('.glass-side')).toHaveCount(0);
		await page.locator('.reopen').click();
		await frameWorks(page);
		expect(errors, 'the error stays inside the host').toEqual([]);
	});

	test('an extension that never calls ready() shows 「extension 沒有回應」 after 5 s', async ({ page }) => {
		await open(page, '#/ext/silent/main');
		await expect(page.locator('.ext-mount')).toHaveAttribute('data-ext-state', 'loading');
		await expect(page.locator('.ext-mount [role="status"]')).toHaveText('extension 沒有回應', { timeout: 8000 });
		await frameWorks(page);
	});
});
