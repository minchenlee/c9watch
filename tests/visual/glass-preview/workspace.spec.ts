import { test, expect, type Page } from '@playwright/test';
import { fileURLToPath } from 'node:url';
import { openClassic, settle, expectNoUnhandledCommands } from '../harness/classic';
import { workspaceId } from '../../../src/shells/glass/model/workspaces';

// G3: workspace page and the glass classic card. Preview only, no baseline.
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
	'C9WATCH', 'FLOCKBOOK', 'JUMPMARK', '跨專案', 'lead-terms', 'w-monitor', 'w-export', '狀態用詞統一', '匯出 CSV',
	'欄位對應', '圖片上傳壓縮', 'pnpm', 'SENTRY_DSN', 'staging', '/Users/demo', 'MONITOR 改用新詞表', '上次用到'
];
async function expectNoFixtureText(page: Page) {
	const text = await page.locator('.glass-root').innerText();
	for (const t of FIXTURE_TEXT) expect(text, `fixture text "${t}" in live mode`).not.toContain(t);
}

test.describe('glass workspace page (not a baseline)', () => {
	test('workspace page: rows of classic cards, done, usage, memory and rules', async ({ page }) => {
		await page.setViewportSize({ width: 1100, height: 1400 });
		await openClassic(page, { path: '/#/ws/flockbook', storage: glass });
		const ws = page.locator('.ws-page');
		await expect(ws.locator('h1')).toHaveText('FLOCKBOOK');
		// Rows from the G2 model: the goal row (demo name) and the talk row.
		await expect(ws.locator('.row-h .name')).toHaveText(['匯出 CSV', '對談 · feed-cache']);
		await expect(ws.locator('.card')).toHaveCount(4);
		await expect(ws.locator('.card[data-tone="needs"]')).toHaveCount(1);
		await expect(ws.locator('.card[data-member$="w-export"] .pixel-status')).toHaveText('BLOCKED');
		await expect(ws.locator('.card[data-member$="w-columns"] .pixel-status')).toHaveText('DONE');
		await expect(ws.locator('.card[data-member$="w-export"] .chip')).toHaveText(/重試\s*2/);
		await expect(ws.locator('.pixel-bar')).toHaveAttribute('data-needs', '1');
		await expect(ws.locator('.li .t')).toHaveText(['匯出 CSV › 欄位對應', '對談 · feed 快取修正', '登入頁改版', '圖片上傳壓縮']);
		await expect(ws.locator('.tile .num').first()).toHaveText('0.4M');
		await expect(ws.locator('.rule')).toHaveCount(2);
		await expect(ws.locator('.file summary .t')).toHaveText('MEMORY.md');
		await ready(page);
		await page.screenshot({ path: out('g3-workspace-flockbook') });
	});

	test('C9WATCH: two goal rows and a talk row; buttons only on hover except needs cards', async ({ page }) => {
		await page.setViewportSize({ width: 1100, height: 900 });
		await openClassic(page, { path: '/#/ws/c9watch', storage: glass });
		const ws = page.locator('.ws-page');
		await expect(ws.locator('.row-h .name')).toHaveText(['狀態用詞統一', '引擎權限重構', '對談 · spike']);
		await expect(ws.locator('.card[data-tone="needs"]')).toHaveCount(2);
		const plain = ws.locator('.card[data-member$="w-authz"] .ac');
		await expect(plain).toHaveCSS('opacity', '0');
		await ws.locator('.card[data-member$="w-authz"]').hover();
		await expect(plain).toHaveCSS('opacity', '1');
		await expect(ws.locator('.card[data-member$="w-monitor"] .ac')).toHaveCSS('opacity', '1');
		await ready(page);
		await page.screenshot({ path: out('g3-workspace-c9watch') });
	});

	test('needs-attention cards: review and approval', async ({ page }) => {
		await page.setViewportSize({ width: 1100, height: 900 });
		await openClassic(page, { path: '/#/ws/c9watch', storage: glass });
		const review = page.locator('.card[data-member$="w-monitor"]');
		await expect(review.locator('.pixel-status')).toHaveText('REVIEW');
		await expect(review.locator('.pv')).toHaveText('MONITOR 改用新詞表');
		await expect(review.getByRole('link', { name: '在需要你處理' })).toHaveAttribute('href', '#/needs');
		await ready(page);
		await review.screenshot({ path: out('g3-card-needs-review') });

		await page.goto('/#/ws/jumpmark');
		const allow = page.locator('.card[data-member$="talk-main"]');
		await expect(allow.locator('.pixel-status')).toHaveText('APPROVAL');
		await expect(allow.locator('.tool')).toHaveText('Bash: npm run build && npm run test:e2e');
		// No approve / reject on the card (G5 handling panel).
		await expect(allow.getByRole('button', { name: /approve|reject|允許|拒絕/i })).toHaveCount(0);
		await ready(page);
		await allow.screenshot({ path: out('g3-card-needs-approval') });
	});

	test('600px window: one card column, no sideways scroll', async ({ page }) => {
		await page.setViewportSize({ width: 600, height: 900 });
		await openClassic(page, { path: '/#/ws/c9watch', storage: glass });
		await expect(page.locator('.ws-page .card').first()).toBeVisible();
		const overflow = await page.evaluate(() => {
			const main = document.querySelector('.glass-page') as HTMLElement;
			return { doc: document.documentElement.scrollWidth - innerWidth, main: main.scrollWidth - main.clientWidth };
		});
		expect(overflow).toEqual({ doc: 0, main: 0 });
		const xs = await page.locator('.ws-page .row').first().locator('.card').evaluateAll(cs => [...new Set(cs.map(c => Math.round(c.getBoundingClientRect().left)))]);
		expect(xs).toHaveLength(1);
		await ready(page);
		await page.screenshot({ path: out('g3-workspace-600') });
	});

	test('live mode: a live workspace shows live cards and no fixture text', async ({ page }) => {
		await page.setViewportSize({ width: 1100, height: 1200 });
		const id = workspaceId('/Users/visual/projects/web-app');
		await openClassic(page, { path: `/#/ws/${id}`, storage: live });
		const ws = page.locator('.ws-page');
		await expect(ws.locator('h1')).toHaveText('web-app');
		await expect(ws.locator('.card').first()).toBeVisible();
		await expect(ws.locator('.rule')).toHaveCount(0);
		await expectNoFixtureText(page);
		await ready(page);
		await page.screenshot({ path: out('g3-workspace-live') });
	});

	test('live and plain demo mode: fixture routes show no fixture text', async ({ page }) => {
		for (const storage of [live, { 'ui.shell': 'glass', demoMode: 'true' }]) {
			for (const wsId of ['c9watch', 'flockbook', 'jumpmark', 'cross']) {
				await openClassic(page, { path: `/#/ws/${wsId}`, storage });
				await expect(page.locator('.ws-page h1')).toHaveText(wsId);
				await expect(page.locator('.ws-page .state-msg')).toHaveText('這個 workspace 現在沒有 agent');
				await expectNoFixtureText(page);
			}
		}
	});
});
