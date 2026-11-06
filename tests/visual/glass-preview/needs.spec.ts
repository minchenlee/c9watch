import { test, expect, type Page } from '@playwright/test';
import { fileURLToPath } from 'node:url';
import { openClassic, settle, expectNoUnhandledCommands } from '../harness/classic';

// G4–G5: the 需要你 page and the one handling panel. Preview only, no baseline.
// The 7 handoff interaction tests are in tests/glass/needs.spec.ts.
const glass = { 'ui.shell': 'glass', demoMode: 'true', 'c9watch.demoGlass': '1' };
const out = (name: string) => fileURLToPath(new URL(`../.cache/glass-preview/${name}.png`, import.meta.url));

async function shot(page: Page, name: string) {
	await expectNoUnhandledCommands(page);
	await page.mouse.move(0, 0);
	await settle(page);
	await page.screenshot({ path: out(name) });
}
async function openNeeds(page: Page, storage: Record<string, string> = glass, sessions?: 'empty') {
	await page.setViewportSize({ width: 1280, height: 860 });
	await openClassic(page, { path: '/#/needs', storage, ...(sessions ? { sessions } : {}) });
	await expect(page.locator('.glass-root')).toHaveAttribute('data-page', 'needs');
}
/** Live mode on the Classic visual fixture. Its rows predate `sessionKey`,
 * which every c9p `SessionView` has; this adds it. `failAgentList` rejects
 * `agent.list`, as when the core is gone. `managed` makes the question
 * session (pid 41003) a managed engine agent and lets `msg.send` through
 * (the shared IPC mock refuses mail). Runs before the IPC mock. */
const MANAGED = { id: 'agt_01VISUALMANAGED', sessionKey: 'claudeCode:0b6c1f0e-1111-4a2b-9c3d-000000000003' };
async function live(page: Page, failAgentList = false, managed = false) {
	await page.addInitScript(([fail, managed, M]) => {
		const internals: Record<string, unknown> = {};
		let inner: ((cmd: string, args: any, o?: unknown) => unknown) | undefined;
		const calls: { cmd: string; args: any }[] = ((window as any).__C9_CALLS__ = []);
		const call = async (cmd: string, args: any, o?: unknown) => {
			calls.push({ cmd, args });
			if (fail && cmd === 'c9p_request' && args?.method === 'agent.list') throw { code: 'unavailable', msg: 'core is gone', retryable: true, hint: '' };
			if (managed && cmd === 'c9p_request' && args?.method === 'agent.list')
				return { agents: [{ id: M.id, sessionKey: M.sessionKey, name: 'w-cache', role: 'worker', parentId: null, cwd: '/Users/visual/projects/api-server', source: 'engine', control: 'pty', mode: 'managed', state: 'ready', status: 'needs_attention', generation: 1, goneAt: null, lastSeen: Date.now(), createdAt: Date.now() - 3_600_000, mailPaused: false }] };
			if (managed && cmd === 'c9p_request' && args?.method === 'msg.send')
				return { cursor: 'visual:send:1', msgId: 'msg_01VISUAL', paused: false, replayed: false, state: 'queued', to: { addr: `m/${args.params.to}`, id: args.params.to, name: 'w-cache' } };
			const r: any = await inner!(cmd, args, o);
			if (cmd === 'c9p_request' && args?.method === 'session.views' && r?.sessions)
				return { ...r, sessions: r.sessions.map((s: any) => ({ sessionKey: `${s.provider}:${s.id}`, ...s })) };
			return r;
		};
		Object.defineProperty(internals, 'invoke', { configurable: true, get: () => call, set: (fn) => { inner = fn; } });
		(window as any).__TAURI_INTERNALS__ = internals;
	}, [failAgentList, managed, MANAGED] as const);
}
const calls = (page: Page, method: string) => page.evaluate((m) => (window as any).__C9_CALLS__.filter((c: any) => c.cmd === 'c9p_request' && c.args?.method === m).map((c: any) => c.args.params), method);
const MUTATING = ['msg.send', 'session.stop', 'msg.cancel', 'mail.pause', 'mail.resume'];
const panel = (page: Page) => page.locator('.handling');
const pick = (page: Page, verb: string) => page.locator('.queue .qi', { has: page.locator('.verb', { hasText: verb }) }).click();

// Text that exists only in the demo fixture or the demo gaps file.
const FIXTURE_TEXT = ['MONITOR 改用新詞表', '要不要改狀態色', '匯出 CSV', 'staging', 'STAGING_API_KEY', 'npm run test:e2e', '5432',
	'會開瀏覽器跑測試', 'w-agents', 'w-login', 'lead-terms', 'C9WATCH', 'FLOCKBOOK', 'JUMPMARK', '跨專案', '狀態用詞統一'];

test.describe('glass 需要你 (G4 preview)', () => {
	test('demo queue: 5 items, sorted by wait, first one open', async ({ page }) => {
		await openNeeds(page);
		await expect(page.locator('.queue .qi')).toHaveCount(5);
		await expect(page.locator('.glass-side [data-nav="needs"] .need-n')).toHaveText('5');
		await expect(page.locator('.queue .rule')).toHaveText('等越久越前面。');
		await expect(page.locator('.queue .qi.on')).toHaveCount(1);
		await expect(panel(page).locator('.prog .n')).toHaveText('第 1 件，共 5 件');
	});

	test('審: before / after, diff summary, 驗收 / 退回', async ({ page }) => {
		await openNeeds(page);
		await pick(page, '審');
		const p = panel(page);
		await expect(p).toHaveAttribute('data-verb', '審');
		await expect(p.locator('figcaption')).toContainText(['改前', '改後 · verifier 通過 · 3 個檔案 +42 −18']);
		await expect(p.locator('.opt')).toHaveCount(2);
		// G5: a managed agent; the main button says what ⌘↵ does.
		await expect(p.locator('[data-primary]')).toContainText('驗收');
		await expect(p.locator('[data-primary]')).toBeEnabled();
		await expect(p.locator('.note')).toContainText('示範模式：不會真的送出或停止。');
		await expect(p.locator('.grant')).toHaveCount(0); // D9 = C: only 允許 has the toggle
		await page.keyboard.press('Digit2');
		await expect(p.locator('textarea')).toBeFocused();
		await expect(p.locator('[data-primary]')).toBeDisabled();
		await page.keyboard.press('Escape');
		await page.keyboard.press('Digit1');
		await shot(page, 'g4-verb-review');
	});

	test('決定: two shots, 4 options, 4 = 自己寫 opens a box', async ({ page }) => {
		await openNeeds(page);
		await pick(page, '決定');
		const p = panel(page);
		await expect(p.locator('.shot')).toHaveCount(2);
		await expect(p.locator('.opt')).toHaveCount(4);
		await expect(p.locator('.opt.on')).toContainText('只有「需要你」上色');
		await expect(p.locator('[data-primary]')).toContainText('送出：只有「需要你」上色');
		await expect(p.locator('.grant')).toHaveCount(0);
		await shot(page, 'g4-verb-decide');
		await page.keyboard.press('Digit4');
		await expect(p.locator('textarea')).toBeFocused();
		await expect(p.locator('[data-primary]')).toBeDisabled();
	});

	test('處理: log and 3 options', async ({ page }) => {
		await openNeeds(page);
		await pick(page, '處理');
		const p = panel(page);
		await expect(p.locator('.code')).toContainText('address already in use');
		await expect(p.locator('.opt')).toHaveCount(3);
		await expect(p.locator('.grant')).toHaveCount(0);
		await shot(page, 'g4-verb-fix');
	});

	test('給: the key field has the focus; empty key keeps the button off; J still moves', async ({ page }) => {
		await openNeeds(page);
		await pick(page, '給');
		const p = panel(page);
		const key = p.locator('input[type="password"]');
		await expect(key).toBeFocused();
		await expect(p.locator('[data-primary]')).toBeDisabled();
		await expect(p.locator('[data-primary]')).toContainText('先貼上 key');
		// The 「不」 answer can be sent; a key cannot (G5).
		await expect(p.getByRole('button', { name: '我沒有這把 key' })).toBeEnabled();
		await expect(p.locator('.grant')).toHaveCount(0);
		await shot(page, 'g4-verb-give');
		// Handoff test #7: an empty field does not eat J.
		const before = await p.locator('.prog .n').textContent();
		await page.keyboard.press('KeyJ');
		await expect(p.locator('.prog .n')).not.toHaveText(before!);
	});

	test('允許: ⌘O 在終端機回答, cannot answer yet, facts with source, the only toggle', async ({ page }) => {
		await openNeeds(page);
		await pick(page, '允許');
		const p = panel(page);
		await expect(p.locator('[data-cannot]')).toHaveText('c9watch 還不能直接回答');
		await expect(p.locator('[data-primary]')).toContainText('在終端機回答');
		await expect(p.locator('[data-primary] kbd')).toHaveText('⌘O');
		await expect(p.locator('[data-primary]')).toBeEnabled();
		await expect(p.locator('.f')).toHaveCount(3);
		await expect(p.locator('.conf')).toHaveText(['確定', '估計', '不確定']);
		// Fixture only: the toggle is a design preview that saves nothing.
		await expect(p.locator('.grant .label')).toHaveText('另外 · 設計預覽，不會儲存');
		await expect(p.locator('.grant')).not.toContainText('撤銷');
		await expect(p.locator('.grant [role="switch"]')).not.toBeChecked();
		await expect(p.getByRole('button', { name: '拒絕' })).toHaveCount(0);
		await shot(page, 'g4-verb-allow');
	});

	test('給: an input-method key (keyCode 229) in the empty field is not taken', async ({ page }) => {
		await openNeeds(page);
		await pick(page, '給');
		const p = panel(page);
		const key = p.locator('input[type="password"]');
		await expect(key).toBeFocused();
		const prevented = await key.evaluate((el) => {
			const e = new KeyboardEvent('keydown', { code: 'KeyJ', key: 'Process', bubbles: true, cancelable: true });
			Object.defineProperty(e, 'keyCode', { get: () => 229 });
			el.dispatchEvent(e);
			return e.defaultPrevented;
		});
		expect(prevented).toBe(false);
		await expect(p.locator('.prog .n')).toHaveText('第 1 件，共 5 件');
	});

	test('J / K walk the queue and the position follows', async ({ page }) => {
		await openNeeds(page);
		const n = panel(page).locator('.prog .n');
		await page.keyboard.press('KeyJ');
		await expect(n).toHaveText('第 2 件，共 5 件');
		await page.keyboard.press('KeyK');
		await page.keyboard.press('KeyK');
		await expect(n).toHaveText('第 5 件，共 5 件');
	});

	test('disconnected: banner on top; the main button promises no queue', async ({ page }) => {
		await openNeeds(page, { ...glass, 'c9watch.demoGlassOffline': '1' });
		await pick(page, '處理');
		const p = panel(page);
		await expect(p.locator('[data-offline]')).toContainText('和 agent 斷線了 · 2 分前');
		await expect(p.getByRole('button', { name: '重新連線' })).toBeVisible();
		// Nothing is kept for later, so nothing promises it.
		await expect(p.locator('[data-primary]')).toContainText('斷線了，連上後再送');
		await expect(p.locator('[data-primary]')).toBeDisabled();
		await expect(p.locator('[data-stop]')).toBeDisabled();
		await expect(page.locator('.glass-root')).not.toContainText('先存著');
		await page.keyboard.press('Meta+Enter');
		await expect(page.locator('[data-undo]')).toHaveCount(0);
		await shot(page, 'g4-disconnected');
	});

	test('0 items: 「0 · 沒有事」 is gray and the page says so', async ({ page }) => {
		await openNeeds(page, { 'ui.shell': 'glass' }, 'empty');
		await expect(page.locator('.glass-side [data-nav="needs"] .need-n')).toHaveText('0 · 沒有事');
		await expect(page.locator('.glass-side [data-nav="needs"] .need-n')).not.toHaveClass(/\bon\b/);
		await expect(page.locator('.glass-side [data-nav="needs"] .need-dot')).not.toHaveClass(/\bon\b/);
		await expect(page.locator('.queue .qi')).toHaveCount(0);
		await expect(page.locator('.none h1')).toHaveText('沒有需要你的事');
		await shot(page, 'g4-empty');
	});

	test('live mode: real items only, no fixture text anywhere', async ({ page }) => {
		await live(page);
		await openNeeds(page, { 'ui.shell': 'glass' });
		// The Classic visual fixture: one Bash approval, one AskUserQuestion.
		await expect(page.locator('.queue .qi')).toHaveCount(2);
		await expect(page.locator('.queue .verb')).toHaveText(['決定', '允許']);
		for (const i of [0, 1]) {
			await page.locator('.queue .qi').nth(i).click();
			const text = await page.locator('.glass-root').innerText();
			for (const t of FIXTURE_TEXT) expect(text, `fixture text "${t}" in live mode`).not.toContain(t);
			// Observed sessions: c9watch cannot answer, ⌘O only.
			await expect(panel(page).locator('[data-cannot]')).toBeVisible();
		}
		await page.locator('.queue .qi', { hasText: '允許' }).click();
		// Live: no 「以後直接允許」 toggle until core can answer and keep rules.
		await expect(panel(page).locator('.grant')).toHaveCount(0);
		await shot(page, 'g4-live');
		// 「在終端機回答」 opens the real session (pid 41001) and sends nothing.
		await panel(page).locator('[data-primary]').click();
		await expect.poll(() => page.evaluate(() => (window as any).__C9_CALLS__.filter((c: any) => c.cmd === 'open_session').map((c: any) => c.args.pid))).toEqual([41001]);
		await page.keyboard.press('Meta+KeyO');
		await expect.poll(() => page.evaluate(() => (window as any).__C9_CALLS__.filter((c: any) => c.cmd === 'open_session').length)).toBe(2);
	});

	test('live mode: a feed error shows the disconnect banner', async ({ page }) => {
		await live(page, true);
		await openNeeds(page, { 'ui.shell': 'glass' });
		await expect(panel(page).locator('[data-offline]')).toContainText('和 agent 斷線了 · 剛剛');
	});
	test('G5 demo: the undo toast and the receipt; no mutating call reaches the IPC', async ({ page }) => {
		await live(page); // only to record the IPC calls; demoMode stays on
		await openNeeds(page);
		await pick(page, '審');
		await page.keyboard.press('Meta+Enter');
		await pick(page, '決定');
		await page.keyboard.press('Meta+Enter');
		await page.clock.runFor(3_000);
		await expect(page.locator('[data-undo]')).toHaveCount(2);
		await expect(page.locator('[data-leave-rule]')).toContainText('離開這一頁或關掉視窗就不送');
		await shot(page, 'g5-undo-toast');
		await pick(page, '處理');
		await panel(page).locator('[data-stop]').click();
		await expect(panel(page).locator('[data-confirm-stop]')).toContainText('不能復原');
		await panel(page).locator('[data-stop-confirm]').click();
		const r = panel(page);
		await expect(r.locator('.card h2')).toHaveText('✓ 已停止 w-export');
		await expect(r.locator('.kv')).toContainText('沒有真的停止任何程式');
		await expect(r).toContainText('收據頁不接受 ⌘↵，免得連按跳過。');
		await shot(page, 'g5-receipt');
		// The receipt does not take ⌘↵; J goes to the 「下一件」 it names.
		const nextText = (await r.locator('.nx').innerText()).replace(/^下一件：/, '');
		await page.keyboard.press('Meta+Enter');
		await expect(r.locator('.card h2')).toHaveText('✓ 已停止 w-export');
		await page.keyboard.press('KeyJ');
		await expect(r.locator('.ask h2')).toHaveText(nextText);
		await page.clock.runFor(10_000);
		await expect(page.locator('[data-undo]')).toHaveCount(0);
		// Demo answers come from demoTransport: no c9p request of these reached Tauri.
		for (const m of MUTATING) expect(await calls(page, m), m).toEqual([]);
	});

	test('G5 live: msg.send to a managed agent only after 10 s; undo sends nothing', async ({ page }) => {
		await live(page, false, true);
		await openNeeds(page, { 'ui.shell': 'glass' });
		const item = page.locator('.queue .qi', { hasText: 'rate limiter' });
		await item.click();
		const p = panel(page);
		await expect(p.locator('[data-cannot]')).toHaveCount(0);
		await expect(p.locator('.note')).not.toContainText('示範');
		await p.locator('textarea').fill('Use Redis.');
		await page.keyboard.press('Meta+Enter');
		await expect(page.locator('[data-undo]')).toContainText('10 秒後送出');
		await page.clock.runFor(9_000);
		expect(await calls(page, 'msg.send')).toEqual([]);
		await page.locator('[data-undo] button').click();
		await page.clock.runFor(10_000);
		expect(await calls(page, 'msg.send')).toEqual([]);
		// The draft came back with the item.
		await expect(p.locator('textarea')).toHaveValue('Use Redis.');
		await page.keyboard.press('Meta+Enter');
		await page.clock.runFor(10_000);
		await expect.poll(() => calls(page, 'msg.send')).toEqual([{ to: MANAGED.id, body: 'Use Redis.' }]);
		await expect(page.locator('[data-done]')).toContainText('送出了');
		const text = await page.locator('.glass-root').innerText();
		for (const t of FIXTURE_TEXT) expect(text, `fixture text "${t}" in live mode`).not.toContain(t);
	});

	test('G5 live: leaving the page before 10 s sends nothing; the draft stays', async ({ page }) => {
		await live(page, false, true);
		await openNeeds(page, { 'ui.shell': 'glass' });
		await page.locator('.queue .qi', { hasText: 'rate limiter' }).click();
		await panel(page).locator('textarea').fill('Use memory.');
		await page.keyboard.press('Meta+Enter');
		await expect(page.locator('[data-leave-rule]')).toBeVisible();
		await page.locator('.glass-side a', { hasText: '今天' }).click();
		await expect(page.locator('.glass-root')).toHaveAttribute('data-page', 'today');
		await page.clock.runFor(30_000);
		expect(await calls(page, 'msg.send')).toEqual([]);
		await page.locator('.glass-side [data-nav="needs"]').click();
		await page.locator('.queue .qi', { hasText: 'rate limiter' }).click();
		await expect(panel(page).locator('textarea')).toHaveValue('Use memory.');
	});

	test('G5 live: stop a managed agent (session.stop, receipt); an observed session gets ⌘O only', async ({ page }) => {
		await live(page, false, true);
		await openNeeds(page, { 'ui.shell': 'glass' });
		await page.locator('.queue .qi', { hasText: '允許' }).click();
		await expect(panel(page).locator('[data-stop]')).toHaveCount(0);
		await expect(panel(page).locator('textarea')).toHaveCount(0);
		await expect(panel(page).locator('[data-cannot]')).toHaveText('c9watch 還不能直接回答');
		await page.locator('.queue .qi', { hasText: 'rate limiter' }).click();
		await panel(page).locator('[data-stop]').click();
		await panel(page).locator('[data-stop-confirm]').click();
		await expect(panel(page).locator('.card h2')).toHaveText('✓ 已停止 w-cache');
		await expect(panel(page).locator('.kv')).toContainText('pid 41003');
		await expect(panel(page).locator('.kv')).not.toContainText('示範');
		await expect.poll(() => calls(page, 'session.stop')).toEqual([{ sessionKey: MANAGED.sessionKey, pid: 41003 }]);
		expect(await calls(page, 'msg.send')).toEqual([]);
	});
});
