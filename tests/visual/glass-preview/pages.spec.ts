import { test, expect, type Page } from '@playwright/test';
import { fileURLToPath } from 'node:url';
import { openClassic, settle, expectNoUnhandledCommands } from '../harness/classic';

// G10a: 今天, 用量 and 設定. Preview only, no baseline.
const glass = { 'ui.shell': 'glass', demoMode: 'true', 'c9watch.demoGlass': '1' };
const live = { 'ui.shell': 'glass' };
const out = (name: string) => fileURLToPath(new URL(`../.cache/glass-preview/${name}.png`, import.meta.url));

async function ready(page: Page) {
	await expectNoUnhandledCommands(page);
	await page.mouse.move(0, 0);
	await settle(page);
}

/** Text that exists only in the glass fixture or the demo gaps file. */
const FIXTURE_TEXT = [
	'C9WATCH', 'FLOCKBOOK', 'JUMPMARK', '跨專案', 'lead-terms', 'w-monitor', 'w-history', '狀態用詞統一', '匯出 CSV', '驗收完', '被退回',
	'HISTORY 畫面', 'MONITOR 畫面', '欄位對應', 'cursor-auto', 'npm run lint', '設計預覽', 'SENTRY_DSN', '/Users/demo', '上次用到'
];
async function expectNoFixtureText(page: Page) {
	const text = await page.locator('.glass-root').innerText();
	for (const t of FIXTURE_TEXT) expect(text, `fixture text "${t}" in live mode`).not.toContain(t);
}
const card = (page: Page, id: string) => page.locator(`[data-card="${id}"]`);

test.describe('glass 今天 / 用量 / 設定 (not a baseline)', () => {
	test('今天: one needs line, then 驗收完 → 被退回 → 需要留意 → 還在進行', async ({ page }) => {
		await page.setViewportSize({ width: 1100, height: 1100 });
		await openClassic(page, { path: '/#/today', storage: glass });
		const t = page.locator('[data-today]');
		await expect(t.locator('[data-needs-line]')).toHaveText(/查看 5 件需要你 →/);
		await expect(t.locator('[data-needs-line]')).toHaveAttribute('href', '#/needs');
		await expect(t.locator('section .psec')).toHaveText([/^驗收完\s*6$/, /^被退回\s*1$/, /^需要留意\s*1$/, /^還在進行\s*\d+$/]);
		await expect(t.locator('[data-section="attention"] .t')).toHaveText(['w-history']);
		await expect(t.locator('[data-section="attention"] .d')).toContainText('沒回應 · 24 分鐘沒有動靜');
		await expect(t.locator('[data-section="active"] .t').first()).toHaveText('C9WATCH › 引擎權限重構');
		await expect(t.locator('[data-section="active"] .away').first()).toContainText(/^今天：改了 \d+ 個檔案/);
		// The 「符合你的規則…」 section stays hidden (Gap R); no handling on this page.
		await expect(t.getByText(/規則/)).toHaveCount(0);
		await expect(t.getByRole('button')).toHaveCount(0);
		await ready(page);
		await page.screenshot({ path: out('g10-today'), fullPage: true });
	});

	test('用量: period switch changes range, cards and bars; unpriced is never $0', async ({ page }) => {
		await page.setViewportSize({ width: 1100, height: 1500 });
		await openClassic(page, { path: '/#/usage', storage: { ...glass, 'glass.monthlyFee': '310' } });
		const u = page.locator('[data-usage]');
		await expect(u).toHaveAttribute('data-period', 'month');
		await expect(u.locator('[data-period-btn]')).toHaveText(['今天', '7 天', '30 天', '本月', '上個月']);
		// FIXED_NOW is 2026-03-10 (UTC): 本月 = 10 of 31 days of 310.
		await expect(card(page, 'paid').locator('.num')).toHaveText('$100.00');
		await expect(u.locator('.bars .bar')).toHaveCount(10);
		await ready(page);
		await page.screenshot({ path: out('g10-usage'), fullPage: true });

		await u.locator('[data-period-btn="7d"]').click();
		await expect(u).toHaveAttribute('data-period', '7d');
		await expect(u.locator('.sub')).toHaveText(/^3\/4 – 3\/10/);
		await expect(card(page, 'paid').locator('.num')).toHaveText('$70.00');
		await expect(u.locator('.bars .bar')).toHaveCount(7);
		// A Cursor record has no price: its row says so, the total says 部分.
		await expect(u.locator('[data-table="model"] tr', { hasText: 'cursor-auto' }).locator('td').nth(3)).toHaveText('沒有價格');
		await expect(card(page, 'api').locator('.meta')).toContainText('只算有價格的部分');
		await expect(u.getByText('$0.00')).toHaveCount(0);

		await u.locator('[data-period-btn="lastMonth"]').click();
		await expect(u.locator('.sub')).toHaveText(/^2\/1 – 2\/28/);
		await expect(card(page, 'paid').locator('.num')).toHaveText('$310.00');
		await expect(u.locator('.bars .bar')).toHaveCount(28);

		await u.locator('[data-period-btn="today"]').click();
		await expect(card(page, 'paid').locator('.num')).toHaveText('$10.00');
		await expect(u.locator('.bars')).toHaveCount(0);
		await expect(card(page, 'hours').locator('.num')).toHaveText('9.5 小時');
		await ready(page);
		await page.screenshot({ path: out('g10-usage-today') });
	});

	test('用量: quota windows from usage.subscriptions', async ({ page }) => {
		await page.setViewportSize({ width: 1100, height: 1500 });
		await openClassic(page, { path: '/#/usage', storage: glass });
		const q = page.locator('[data-section="quota"]');
		await expect(q.locator('[data-plan="claudeCode"] [data-window]')).toHaveCount(2);
		await expect(q.locator('[data-plan="claudeCode"] .wl')).toHaveText(['5 小時', '每週']);
		await expect(card(page, 'paid')).toContainText('還沒填月費');
	});

	for (const [name, answer, reason] of [
		['a plan without windows shows its message', { usage: [{ provider: 'claudeCode', name: 'Claude Code', plan: null, windows: [], updatedAt: null, message: '找不到 Claude Code 的登入資料' }] }, '找不到 Claude Code 的登入資料'],
		['no plan at all says so', { usage: [] }, '沒有讀到任何訂閱方案的額度。'],
		['a failed read shows the error', { error: '額度服務沒有回應' }, '讀不到額度：額度服務沒有回應']
	] as const) {
		test(`用量 quota unavailable: ${name}`, async ({ page }) => {
			// Wraps the IPC mock (installed later) so usage.subscriptions answers this case.
			await page.addInitScript((answer) => {
				const internals: Record<string, unknown> = {};
				let inner: ((cmd: string, args: any, o?: unknown) => unknown) | undefined;
				Object.defineProperty(internals, 'invoke', {
					configurable: true,
					get: () => (cmd: string, args: any, o?: unknown) => {
						if (cmd === 'c9p_request' && args?.method === 'usage.subscriptions') {
							return 'error' in answer ? Promise.reject({ code: 'internal', msg: answer.error }) : Promise.resolve(answer);
						}
						return inner!(cmd, args, o);
					},
					set: (fn) => { inner = fn; }
				});
				(window as any).__TAURI_INTERNALS__ = internals;
			}, answer as any);
			await openClassic(page, { path: '/#/usage', storage: live });
			await expect(page.locator('[data-quota-reason]')).toHaveText(reason);
			if (name.startsWith('a plan')) { await ready(page); await page.locator('[data-section="quota"]').screenshot({ path: out('g10-usage-quota-reason') }); }
		});
	}

	test('設定: shell, glass mode, fee, extensions, rules preview (fixture)', async ({ page }) => {
		await page.setViewportSize({ width: 1100, height: 1300 });
		await openClassic(page, { path: '/#/settings', storage: glass });
		const s = page.locator('[data-settings]');
		await expect(s.locator('[data-shell="glass"]')).toHaveAttribute('aria-checked', 'true');
		await expect(s.locator('[data-preview]')).toHaveText('設計預覽，不會儲存');
		await expect(s.locator('.rule')).toHaveCount(1);
		await expect(s.locator('.rule button')).toBeDisabled();
		await expect(s.locator('[data-rearrange]')).toBeDisabled();
		await expect(s.locator('[data-table="extensions"]')).toHaveCount(0);
		await expect(s.getByText('沒有安裝 extension')).toBeVisible();
		await ready(page);
		await page.screenshot({ path: out('g10-settings'), fullPage: true });

		await s.locator('[data-glass-btn="off"]').click();
		await expect(page.locator('.glass-root')).toHaveAttribute('data-glass', 'off');
		expect(await page.evaluate(() => localStorage.getItem('ui.glass'))).toBe('off');
		await s.locator('[data-glass-btn="full"]').click();
		await expect(page.locator('.glass-root')).toHaveAttribute('data-glass', 'full');
		expect(await page.evaluate(() => localStorage.getItem('ui.glass'))).toBe('full');
	});

	test('設定: monthly fee saves on Enter, not on an IME Enter; bad input is refused', async ({ page }) => {
		await openClassic(page, { path: '/#/settings', storage: glass });
		const fee = page.locator('[data-fee]');
		await fee.fill('abc');
		await fee.press('Enter');
		await expect(page.locator('#s-fee-note')).toHaveText('請填 0 以上的數字，最多 2 位小數');
		expect(await page.evaluate(() => localStorage.getItem('glass.monthlyFee'))).toBeNull();
		await fee.fill('200');
		await fee.evaluate(el => el.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true, isComposing: true })));
		expect(await page.evaluate(() => localStorage.getItem('glass.monthlyFee'))).toBeNull();
		await fee.press('Enter');
		expect(await page.evaluate(() => localStorage.getItem('glass.monthlyFee'))).toBe('200');
		await page.locator('.glass-side a', { hasText: '用量' }).click();
		await expect(card(page, 'paid').locator('.num')).toHaveText(/^\$\d+\.\d\d$/);
	});

	test('設定: switching to Classic asks first, then unmounts glass and stores ui.shell', async ({ page }) => {
		await openClassic(page, { path: '/#/settings', storage: glass });
		await page.locator('[data-shell="classic"]').click();
		await expect(page.locator('[data-confirm-classic]')).toContainText('還沒有切回 Glass 的開關');
		expect(await page.evaluate(() => localStorage.getItem('ui.shell'))).toBe('glass');
		await page.getByRole('button', { name: '留在 Glass' }).click();
		await expect(page.locator('[data-confirm-classic]')).toHaveCount(0);
		await page.locator('[data-shell="classic"]').click();
		await page.locator('[data-confirm="classic"]').click();
		await expect(page.locator('.glass-root')).toHaveCount(0);
		expect(await page.evaluate(() => localStorage.getItem('ui.shell'))).toBe('classic');
	});

	test('live mode: the three pages show no fixture text and no fixture-only parts', async ({ page }) => {
		await page.setViewportSize({ width: 1100, height: 1400 });
		for (const storage of [live, { 'ui.shell': 'glass', demoMode: 'true' }]) {
			await openClassic(page, { path: '/#/today', storage });
			await expect(page.locator('[data-today] section .psec').first()).toHaveText(/^今天完成/);
			await expect(page.locator('[data-section="rejected"]')).toHaveCount(0);
			await expectNoFixtureText(page);
			if (storage === live) { await ready(page); await page.screenshot({ path: out('g10-today-live'), fullPage: true }); }

			await page.goto('/#/usage');
			await expect(page.locator('[data-usage] .tiles')).toBeVisible();
			await expect(card(page, 'hours')).toContainText('c9watch 還沒有記錄 agent 的工作時間');
			await expectNoFixtureText(page);
			if (storage === live) { await ready(page); await page.screenshot({ path: out('g10-usage-live'), fullPage: true }); }

			await page.goto('/#/settings');
			await expect(page.locator('[data-rules-empty]')).toHaveText('還沒有規則');
			await expect(page.locator('[data-preview]')).toHaveCount(0);
			await expectNoFixtureText(page);
			if (storage === live) { await ready(page); await page.screenshot({ path: out('g10-settings-live'), fullPage: true }); }
		}
	});

	test('600px window: no sideways scroll on the three pages', async ({ page }) => {
		await page.setViewportSize({ width: 600, height: 900 });
		for (const hash of ['#/today', '#/usage', '#/settings']) {
			await openClassic(page, { path: `/${hash}`, storage: glass });
			await expect(page.locator('.glass-page h1')).toBeVisible();
			const overflow = await page.evaluate(() => {
				const main = document.querySelector('.glass-page') as HTMLElement;
				return { doc: document.documentElement.scrollWidth - innerWidth, main: main.scrollWidth - main.clientWidth };
			});
			expect(overflow, hash).toEqual({ doc: 0, main: 0 });
		}
	});
});
