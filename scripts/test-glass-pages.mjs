import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import ts from 'typescript';

// Glass G10a pure helpers: 用量 period math (usage-math.ts) and the 今天
// sections (today-model.ts). Same loader as test-glass-ui.mjs.
const urls = {};
const dataUrl = src => 'data:text/javascript;base64,' + Buffer.from(src).toString('base64');
async function load(spec, path) {
    let out = ts.transpileModule(readFileSync(new URL(path, import.meta.url), 'utf8'), {
        compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022 }
    }).outputText;
    for (const [s, url] of Object.entries(urls)) out = out.replaceAll(`from '${s}'`, `from '${url}'`);
    assert(!/from '(\.|\$lib)/.test(out), `${path} has an unresolved import`);
    urls[spec] = dataUrl(out);
    return import(urls[spec]);
}
urls['$lib/data/client'] = dataUrl('export function request() { throw new Error("no transport in tests"); }');
const G = '../src/shells/glass/';
const { workspaceFor } = await load('../model/workspaces', G + 'model/workspaces.ts');
await load('./workspace-extras', G + 'pages/workspace-extras.ts');
const U = await load('./usage-math', G + 'pages/usage-math.ts');
const T = await load('./today-model', G + 'pages/today-model.ts');
const fixture = await load('$lib/data/demo/glass', '../src/lib/data/demo/glass.ts');
const gaps = await load('../model/gaps-demo', G + 'model/gaps-demo.ts');

const rec = (date, project, model, tokens, cost, costAvailable = true) => ({
    cachedInputTokens: 0, cost, costAvailable, date, inputTokens: tokens, model, outputTokens: 0, project, projectName: project.split('/').pop(),
    provider: 'claude', reasoningOutputTokens: 0, sessionId: `${date}-${model}`, sessionName: 'x', timestamp: `${date}T10:00:00Z`, totalTokens: tokens
});
const costOf = records => {
    const days = new Map();
    for (const r of records) days.set(r.date, [...(days.get(r.date) ?? []), r]);
    return { dailyCosts: [...days].map(([date, sessions]) => ({ date, cost: 0, sessions })), modelCosts: [], projectCosts: [], totalCost: 0, totalTokens: 0, unpricedTokens: 0 };
};

test('periods: today, 7 / 30 days back, this month, last month', () => {
    assert.deepEqual(U.PERIODS.map(p => p.label), ['今天', '7 天', '30 天', '本月', '上個月']);
    const r = p => { const x = U.rangeFor(p, '2026-03-10'); return [x.from, x.to, x.days.length]; };
    assert.deepEqual(r('today'), ['2026-03-10', '2026-03-10', 1]);
    assert.deepEqual(r('7d'), ['2026-03-04', '2026-03-10', 7]);
    assert.deepEqual(r('30d'), ['2026-02-09', '2026-03-10', 30]);
    assert.deepEqual(r('month'), ['2026-03-01', '2026-03-10', 10]);
    assert.deepEqual(r('lastMonth'), ['2026-02-01', '2026-02-28', 28]);
    // Year turn, leap year, a DST day (US 2026-03-08) all count whole days.
    assert.deepEqual(U.rangeFor('lastMonth', '2026-01-15').days.length, 31);
    assert.equal(U.rangeFor('lastMonth', '2026-01-15').from, '2025-12-01');
    assert.equal(U.rangeFor('lastMonth', '2028-03-01').to, '2028-02-29');
    assert.equal(U.rangeFor('7d', '2026-01-03').from, '2025-12-28');
    assert.deepEqual(U.rangeFor('7d', '2026-03-10').days, ['2026-03-04', '2026-03-05', '2026-03-06', '2026-03-07', '2026-03-08', '2026-03-09', '2026-03-10']);
});

test('實付: the monthly fee spread over the days of each month', () => {
    assert.equal(U.paidFor(U.rangeFor('lastMonth', '2026-03-10'), 200), 200);
    assert.equal(U.paidFor(U.rangeFor('today', '2026-02-10'), 280), 10);
    assert.equal(U.paidFor(U.rangeFor('month', '2026-04-15'), 300), 150);
    // 30 days that span February (28 days) and March (31 days).
    const span = U.paidFor(U.rangeFor('30d', '2026-03-10'), 100);
    assert.ok(Math.abs(span - (20 * 100 / 28 + 10 * 100 / 31)) < 1e-9);
    assert.equal(U.paidFor(U.rangeFor('month', '2026-03-10'), null), null);
    assert.equal(U.paidFor(U.rangeFor('month', '2026-03-10'), -1), null);
});

test('summary: only records in the period; rows by workspace and model; 分攤 by token share', () => {
    const cost = costOf([
        rec('2026-03-10', '/r/app', 'opus', 300, 3),
        rec('2026-03-09', '/r/app/.worktrees/x', 'sonnet', 100, 1),
        rec('2026-03-09', '/r/web', 'opus', 600, 6),
        rec('2026-02-27', '/r/web', 'opus', 5000, 50)
    ]);
    const s = U.summarize(cost, U.rangeFor('7d', '2026-03-10'), 70, null);
    assert.equal(s.tokens, 1000);
    assert.equal(s.apiCost, 10);
    assert.equal(s.pricing, 'all');
    assert.equal(s.paid, 70 * 7 / 31);
    assert.deepEqual(s.byWorkspace.map(r => [r.label, r.tokens]), [['web', 600], ['app', 400]]);
    assert.equal(s.byWorkspace[1].workspaceId, workspaceFor('/r/app', null).id, 'worktree folds into its repo');
    assert.ok(Math.abs(s.byWorkspace[0].paid - s.paid * 0.6) < 1e-9);
    assert.deepEqual(s.byModel.map(r => [r.label, r.tokens]), [['opus', 900], ['sonnet', 100]]);
    assert.deepEqual(s.daily.map(d => d.tokens), [0, 0, 0, 0, 0, 700, 300]);
    const last = U.summarize(cost, U.rangeFor('lastMonth', '2026-03-10'), null, null);
    assert.equal(last.tokens, 5000);
    assert.equal(last.paid, null);
    assert.equal(last.byWorkspace[0].paid, null);
});

test('unpriced records: no $0, a partial total says so', () => {
    const cost = costOf([rec('2026-03-10', '/r/app', 'cursor-auto', 400, 0, false), rec('2026-03-10', '/r/web', 'opus', 100, 2)]);
    const s = U.summarize(cost, U.rangeFor('today', '2026-03-10'), null, null);
    assert.equal(s.pricing, 'some');
    assert.equal(s.unpricedTokens, 400);
    const usd = n => `$${n.toFixed(2)}`;
    const app = s.byWorkspace.find(r => r.label === 'app');
    assert.equal(U.apiPriceText(app.apiCost, app.pricing, usd), '沒有價格');
    assert.equal(U.apiPriceText(s.apiCost, s.pricing, usd), '$2.00（部分）');
    const none = U.summarize(costOf([rec('2026-03-10', '/r/app', 'cursor-auto', 400, 0, false)]), U.rangeFor('today', '2026-03-10'), null, null);
    assert.equal(none.pricing, 'none');
    assert.equal(U.apiPriceText(none.apiCost, none.pricing, usd), '沒有價格');
});

test('fixture: every period has usage; demo names only with the demo list', () => {
    const today = new Date().toISOString().slice(0, 10);
    const local = (() => { const d = new Date(); return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`; })();
    const cost = fixture.glassDemoCost();
    for (const p of U.PERIODS) assert.ok(U.summarize(cost, U.rangeFor(p.id, local), null, gaps.DEMO_WORKSPACES).tokens > 0, `${p.id} has usage`);
    const named = U.summarize(cost, U.rangeFor('30d', local), null, gaps.DEMO_WORKSPACES).byWorkspace.map(r => r.label);
    assert.ok(named.includes('C9WATCH'));
    const live = U.summarize(cost, U.rangeFor('30d', local), null, null).byWorkspace.map(r => r.label);
    for (const n of ['C9WATCH', 'FLOCKBOOK', 'JUMPMARK', '跨專案']) assert.ok(!live.includes(n), `${n} without the demo list`);
    assert.ok(cost.dailyCosts.flatMap(d => d.sessions).some(r => !r.costAvailable), 'fixture has an unpriced record');
    assert.ok(U.hoursIn(U.rangeFor('7d', today), today, gaps.DEMO_AGENT_HOURS) > 0);
    assert.equal(U.hoursIn(U.rangeFor('today', '2026-03-10'), '2026-03-10', [{ days: 0, hours: 2 }, { days: 1, hours: 5 }]), 2);
});

test('quota labels and the 90% line', () => {
    assert.equal(U.windowLabel('5-hour'), '5 小時');
    assert.equal(U.windowLabel('Weekly'), '每週');
    assert.equal(U.windowLabel('Included usage'), 'Included usage');
    assert.equal(U.QUOTA_WARN, 90);
});

// ---- 今天 ------------------------------------------------------------------

const NOW = Date.parse('2026-03-10T15:00:00');
const MIDNIGHT = T.localMidnight(NOW);
const member = (key, over = {}) => ({
    key, agentId: null, sessionKey: `claudeCode:${key}`, name: key, role: 'talk', parentId: null, path: '/r/app', status: 'working', live: true,
    mailPaused: false, seenAt: NOW, agent: null, session: { id: key, startedAtMs: NOW - 3_600_000 }, workspaceId: 'ws_app', lastChange: NOW, stale: false, ...over
});
const modelOf = members => ({
    workspaces: [{ id: 'ws_app', root: '/r/app', name: 'app', verifier: null, rows: members.map(m => ({ id: `row-${m.key}`, workspaceId: 'ws_app', kind: 'talk', title: m.name, goalId: null, lead: null, members: [m], cap: null, freeSeats: null, live: true })) }],
    needs: [], changes: new Map()
});
const hist = (id, t) => ({ customTitle: `done ${id}`, display: '', project: '/r/app', projectName: 'app', provider: 'claudeCode', sessionId: id, timestamp: t });

test('today live: 今天完成 from history since midnight, no 被退回, stale is 需要留意, working is 還在進行', () => {
    const model = modelOf([member('a'), member('b', { stale: true, lastChange: NOW - 25 * 60_000 }), member('c', { status: 'waiting' })]);
    const history = [hist('old', MIDNIGHT - 1), hist('x', MIDNIGHT + 1000), hist('y', NOW - 60_000), hist('a', NOW - 1000)];
    const away = { 'claudeCode:a': { commandsRun: 2, filesEdited: 3, partial: false, sinceMs: MIDNIGHT, subagentsCompleted: 0, toolCalls: 9 } };
    const s = T.todaySections({ model, history, away, midnight: MIDNIGHT, now: NOW, demo: null });
    assert.equal(s.doneTitle, '今天完成');
    assert.deepEqual(s.done.map(i => i.title), ['done y', 'done x'], 'newest first; no yesterday, no live session');
    assert.deepEqual(s.rejected, []);
    assert.deepEqual(s.attention.map(i => i.title), ['b']);
    assert.match(s.attention[0].meta, /^沒回應 · 25 分鐘沒有動靜/);
    assert.deepEqual(s.active.map(r => [r.title, r.working]), [['app › a', 1]]);
    assert.equal(T.awayText(s.active[0].away), '今天：改了 3 個檔案 · 跑了 2 個指令');
    assert.equal(T.awayText({ ...s.active[0].away, partial: true }), '今天至少：改了 3 個檔案 · 跑了 2 個指令');
    assert.deepEqual(T.awayRequests(model, MIDNIGHT), [{ sessionKey: 'claudeCode:a', sinceMs: NOW - 3_600_000 }]);
    const early = modelOf([member('a', { session: { id: 'a', startedAtMs: MIDNIGHT - 5000 } })]);
    assert.deepEqual(T.awayRequests(early, MIDNIGHT), [{ sessionKey: 'claudeCode:a', sinceMs: MIDNIGHT }]);
});

test('today fixture: 驗收完 and 被退回 from the gaps file only', () => {
    const model = modelOf([member('a')]);
    const s = T.todaySections({ model, history: [hist('x', NOW - 1000)], away: {}, midnight: MIDNIGHT, now: NOW, demo: gaps.DEMO_TODAY });
    assert.equal(s.doneTitle, '驗收完');
    assert.equal(s.done.length, gaps.DEMO_TODAY.accepted.length);
    assert.deepEqual(s.rejected.map(i => i.title), ['HISTORY 畫面']);
    const live = T.todaySections({ model, history: null, away: {}, midnight: MIDNIGHT, now: NOW, demo: null });
    assert.deepEqual([live.done, live.rejected], [[], []]);
});
