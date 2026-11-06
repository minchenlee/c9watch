import { test, expect, type Page } from '@playwright/test';

// Handoff §3: the 7 interaction bugs, one test each (build plan §G5). Runs on
// `vite dev` in the browser (no Tauri) on the glass demo fixture, so every
// request goes to `demoTransport` and nothing reaches a real core.
// The clock is fake and paused: a test moves it with `runFor`.
const STORAGE = { 'ui.shell': 'glass', demoMode: 'true', 'c9watch.demoGlass': '1' };
const T0 = Date.parse('2026-11-06T09:00:00Z');

async function open(page: Page) {
	await page.clock.install({ time: T0 - 10_000 });
	await page.clock.pauseAt(T0);
	await page.addInitScript((storage) => {
		if (sessionStorage.getItem('c9-test-seeded')) return;
		sessionStorage.setItem('c9-test-seeded', '1');
		localStorage.clear();
		for (const [k, v] of Object.entries(storage)) localStorage.setItem(k, v);
	}, STORAGE);
	await page.goto('/#/needs');
	await expect(page.locator('.queue .qi')).toHaveCount(5);
}
const panel = (page: Page) => page.locator('.handling');
const title = (page: Page) => panel(page).locator('.ask h2');
const pick = (page: Page, verb: string) => page.locator('.queue .qi', { has: page.locator('.verb', { hasText: verb }) }).click();
const toast = (page: Page, label: string) => page.locator('[data-undo]', { hasText: label });
/** A keydown as WebKit sends it under a Chinese input method. */
function imeKey(page: Page, code: string, key: string, composing = false) {
	return page.evaluate(([code, key, composing]) => {
		const e = new KeyboardEvent('keydown', { code, key, bubbles: true, cancelable: true, composed: true, isComposing: composing === 'y' });
		(document.activeElement ?? document.body).dispatchEvent(e);
	}, [code, key, composing ? 'y' : 'n']);
}

test.describe('handoff §3: handling panel interactions', () => {
	test('#1 a send moves to the 「下一件」 the panel showed, not to the first item', async ({ page }) => {
		await open(page);
		await pick(page, '決定');
		const shown = (await panel(page).locator('.nx').innerText()).replace(/^下一件：/, '');
		const first = await page.locator('.queue .qi').first().locator('.t').innerText();
		expect(shown).not.toBe(first);
		await page.keyboard.press('Meta+Enter');
		await expect(title(page)).toHaveText(shown);
		await expect(page.locator('.queue .qi.on .t')).toHaveText(shown);
		await expect(toast(page, '要不要改狀態色')).toBeVisible();
	});

	test('#2 an irreversible action within 10 s keeps the earlier undo until its 10 s end', async ({ page }) => {
		await open(page);
		await pick(page, '審');
		await page.keyboard.press('Meta+Enter');
		const undo = toast(page, 'MONITOR 改用新詞表');
		await expect(undo).toContainText('10 秒後送出');
		// Stop is the action that cannot be undone: it ends in a receipt.
		await pick(page, '處理');
		await panel(page).locator('[data-stop]').click();
		await panel(page).locator('[data-stop-confirm]').click();
		await expect(panel(page).locator('.card h2')).toHaveText('✓ 已停止 w-export');
		await page.clock.runFor(8_000);
		await expect(undo).toContainText('2 秒後送出');
		await expect(undo.getByRole('button', { name: /復原/ })).toBeEnabled();
		// ⌘Z on the receipt page still undoes the earlier send.
		await page.keyboard.press('Meta+KeyZ');
		await expect(undo).toHaveCount(0);
		await expect(title(page)).toHaveText('審MONITOR 改用新詞表');
		await expect(page.locator('[data-notice]')).toContainText('復原了');
	});

	test('#3 a new toast or notice never hides an undo toast', async ({ page }) => {
		await open(page);
		await pick(page, '審');
		await page.keyboard.press('Meta+Enter');
		await page.clock.runFor(3_000);
		await pick(page, '決定');
		await page.keyboard.press('Meta+Enter');
		const a = toast(page, 'MONITOR 改用新詞表'), b = toast(page, '要不要改狀態色');
		await expect(a).toContainText('7 秒後送出');
		await expect(b).toContainText('10 秒後送出');
		// Undo the newer one: a notice appears; the older toast stays.
		await page.keyboard.press('Meta+KeyZ');
		await expect(b).toHaveCount(0);
		await expect(page.locator('[data-notice]')).toContainText('復原了');
		await expect(a).toBeVisible();
		await page.clock.runFor(7_000);
		await expect(a).toHaveCount(0);
		await expect(page.locator('[data-done]', { hasText: 'MONITOR 改用新詞表' })).toContainText('送出了');
	});

	test('#4 J skips handled items', async ({ page }) => {
		await open(page);
		await pick(page, '審');
		await page.keyboard.press('Meta+Enter');
		await expect(page.locator('.queue .qi')).toHaveCount(4);
		const seen = new Set<string>();
		for (let i = 0; i < 8; i++) {
			await page.keyboard.press('KeyJ');
			seen.add(await title(page).innerText());
		}
		expect(seen.size).toBe(4);
		expect([...seen].some(t => t.includes('MONITOR 改用新詞表'))).toBe(false);
	});

	test('#5 Esc while typing keeps the draft', async ({ page }) => {
		await open(page);
		await pick(page, '決定');
		await page.keyboard.press('Digit4');
		const box = panel(page).locator('textarea');
		await expect(box).toBeFocused();
		await page.keyboard.type('先改 AGENTS，其他');
		await page.keyboard.press('Escape');
		await expect(box).not.toBeFocused();
		await expect(box).toHaveValue('先改 AGENTS，其他');
		// Change item and come back.
		await page.keyboard.press('KeyJ');
		await page.keyboard.press('KeyK');
		await expect(panel(page).locator('textarea')).toHaveValue('先改 AGENTS，其他');
		// Leave the page and come back: the draft is still there.
		await page.locator('.glass-side a', { hasText: '今天' }).click();
		await expect(page.locator('.glass-root')).toHaveAttribute('data-page', 'today');
		await page.locator('.glass-side [data-nav="needs"]').click();
		await pick(page, '決定');
		await expect(panel(page).locator('textarea')).toHaveValue('先改 AGENTS，其他');
	});

	test('#6 J / K / 1–4 work with a Chinese input method on; a composing key is left alone', async ({ page }) => {
		await open(page);
		await pick(page, '決定');
		const n = panel(page).locator('.prog .n');
		const at = await n.innerText();
		// Zhuyin on, not composing: `key` is a zhuyin letter, `code` is the key.
		await imeKey(page, 'Digit2', 'ㄉ');
		await expect(panel(page).locator('.opt.on')).toContainText('不改顏色');
		await imeKey(page, 'KeyJ', 'ㄨ');
		await expect(n).not.toHaveText(at);
		await imeKey(page, 'KeyK', 'ㄜ');
		await expect(n).toHaveText(at);
		// Composing: the key belongs to the input method.
		await imeKey(page, 'KeyJ', 'ㄨ', true);
		await imeKey(page, 'Digit1', 'ㄅ', true);
		await expect(n).toHaveText(at);
		await expect(panel(page).locator('.opt.on')).toContainText('不改顏色');
	});

	test('#7 給: the cursor is in the key field; J / K change item while it is empty, type when it is not', async ({ page }) => {
		await open(page);
		// 給 waits longest, so it is first. The page opened on it without a
		// pick, so the field does not take the focus yet (it would eat the
		// shell keys); reach it with J / K, as a person would.
		await expect(panel(page)).toHaveAttribute('data-verb', '給');
		await expect(panel(page).locator('input[type="password"]')).not.toBeFocused();
		await page.keyboard.press('KeyJ');
		await page.keyboard.press('KeyK');
		await expect(panel(page)).toHaveAttribute('data-verb', '給');
		const key = panel(page).locator('input[type="password"]');
		await expect(key).toBeFocused();
		const at = await panel(page).locator('.prog .n').innerText();
		await page.keyboard.press('KeyJ');
		await expect(panel(page)).not.toHaveAttribute('data-verb', '給');
		await page.keyboard.press('KeyK');
		await expect(panel(page)).toHaveAttribute('data-verb', '給');
		await expect(key).toBeFocused();
		await page.keyboard.type('sk-1');
		await page.keyboard.press('KeyJ');
		await page.keyboard.press('KeyK');
		await expect(key).toHaveValue('sk-1jk');
		await expect(panel(page).locator('.prog .n')).toHaveText(at);
		// A key is never sent as mail.
		await expect(panel(page).locator('[data-primary]')).toBeDisabled();
		await expect(panel(page).locator('[data-primary]')).toContainText('c9watch 還不能存 key');
	});
});
