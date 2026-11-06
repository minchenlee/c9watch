import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import ts from 'typescript';

// Glass G3 pure helpers: card status, pixel bar split, annotation chips and
// the workspace page filters. Same loader as test-glass-model.mjs.
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
// workspace-extras reads through the data client; the filters never call it.
urls['$lib/data/client'] = dataUrl('export function request() { throw new Error("no transport in tests"); }');
const G = '../src/shells/glass/';
const { workspaceId } = await load('../model/workspaces', G + 'model/workspaces.ts');
const F = await load('./format', G + 'ui/format.ts');
const X = await load('./workspace-extras', G + 'pages/workspace-extras.ts');
const fixture = await load('$lib/data/demo/glass', '../src/lib/data/demo/glass.ts');
const gaps = await load('./gaps-demo', G + 'model/gaps-demo.ts');

test('card status: only a needs-queue item or a detected wait is orange', () => {
    const m = (status, stale = false) => ({ status, stale });
    assert.deepEqual(F.cardStatus(m('waiting'), '審'), { tone: 'needs', label: 'REVIEW' });
    assert.deepEqual(F.cardStatus(m('needs'), '允許'), { tone: 'needs', label: 'APPROVAL' });
    assert.deepEqual(F.cardStatus(m('needs'), null), { tone: 'needs', label: 'NEEDS YOU' });
    assert.deepEqual(F.cardStatus(m('working'), null), { tone: 'working', label: 'WORKING' });
    assert.deepEqual(F.cardStatus(m('working', true), null), { tone: 'stale', label: 'NO RESPONSE' });
    assert.deepEqual(F.cardStatus(m('waiting'), null), { tone: 'waiting', label: 'WAITING' });
    assert.deepEqual(F.cardStatus(m('done'), null), { tone: 'done', label: 'DONE' });
    assert.deepEqual(F.cardStatus(m('unknown'), null), { tone: 'idle', label: 'IDLE' });
    for (const v of ['允許', '決定', '審', '給', '處理']) assert.match(F.cardStatus(m('waiting'), v).label, /^[A-Z ]+$/, 'pixel status is Latin');
});

test('pixel bar: proportional split fills every block, largest remainder', () => {
    assert.deepEqual(F.allocateBlocks([1, 1, 1], 10), [4, 3, 3]);
    assert.deepEqual(F.allocateBlocks([2, 0, 1], 9), [6, 0, 3]);
    assert.deepEqual(F.allocateBlocks([0, 0, 0], 10), [0, 0, 0]);
    assert.deepEqual(F.allocateBlocks([5, 1, 0], 0), [0, 0, 0]);
    for (const n of [1, 7, 33, 64]) assert.equal(F.allocateBlocks([3, 2, 7], n).reduce((a, b) => a + b, 0), n);
    assert.equal(F.blockColumns(0), 1);
    assert.equal(F.blockColumns(598), 60);
});

test('time and numbers', () => {
    const now = 1_800_000_000_000;
    assert.equal(F.timeAgo(now - 30_000, now), 'now');
    assert.equal(F.timeAgo(now - 4 * 60_000, now), '4m');
    assert.equal(F.timeAgo(now - 3 * 3600_000, now), '3h');
    assert.equal(F.timeAgo(now - 3 * 86400_000, now), '3d');
    assert.equal(F.formatTokens(400_000), '0.4M');
    assert.equal(F.formatTokens(7_400_000), '7.4M');
    assert.equal(F.formatTokens(4200), '4.2K');
    assert.equal(F.formatTokens(0), '0');
    assert.equal(F.formatDuration(95_000), '1m 35s');
    assert.equal(F.formatDuration(3_600_000), '1h');
});

test('annotation chips: one session, ordered by key, rendered by type/format/unit', () => {
    const now = 1_800_000_000_000;
    const a = (key, value, more = {}) => ({ target: { kind: 'session', sessionKey: 'k1' }, key, value, writer: 'local-unverified', sensitivity: 'metadata', createdAt: 0, updatedAt: 0, expiresAt: null, ...more });
    const rows = [a('zz/b', { type: 'bool', v: true }), a('ci/job', { type: 'duration', ms: 95_000 }), { ...a('ci/x', { type: 'number', v: 1 }), target: { kind: 'session', sessionKey: 'other' } }];
    assert.deepEqual(F.annotationsFor(rows, 'k1').map(r => r.key), ['ci/job', 'zz/b']);
    assert.deepEqual(F.annotationsFor(rows, null), []);
    assert.equal(F.annotationLabel(a('ci/job', null)), 'job');
    assert.equal(F.annotationLabel(a('ci/job', null, { label: 'cargo test' })), 'cargo test');
    assert.equal(F.annotationValue(a('k/v', { type: 'duration', ms: 95_000 }), now), '1m 35s');
    assert.equal(F.annotationValue(a('k/v', { type: 'bool', v: false }), now), '否');
    assert.equal(F.annotationValue(a('k/v', { type: 'string', v: '12/12' }), now), '12/12');
    assert.equal(F.annotationValue(a('k/v', { type: 'money', amount: '1.20', currency: 'USD' }), now), '1.20 USD');
    assert.equal(F.annotationValue(a('k/v', { type: 'number', v: 2 }, { unit: 'x' }), now), '2 x');
    assert.equal(F.annotationValue(a('k/v', { type: 'number', v: 1234.4 }, { format: 'integer' }), now), '1,234');
    assert.equal(F.annotationValue(a('k/v', { type: 'number', v: 42 }, { format: 'percent' }), now), '42%');
    assert.equal(F.annotationValue(a('k/v', { type: 'number', v: 2048 }, { format: 'bytes' }), now), '2.0 KB');
    assert.equal(F.annotationValue(a('k/v', { type: 'number', v: now - 120_000 }, { format: 'relative-time' }), now), '2m');
    assert.equal(F.annotationValue(a('k/v', { type: 'number', v: 1.23456 }, { format: 'sparkline' }), now), '1.23', 'unknown format: plain value');
    assert.equal(F.annotationValue(a('k/v', null, { redacted: true }), now), '隱藏');
});

test('workspace filters: history, spend and memory follow the repo root (worktrees fold in)', () => {
    const root = '/r/app';
    const h = (project, t, more = {}) => ({ project, timestamp: t, sessionId: String(t), provider: 'claudeCode', customTitle: null, display: 'd', projectName: '', ...more });
    const hist = [h('/r/app', 1), h('/r/app/.worktrees/x', 3), h('/r/other', 2), h('/r/app2', 4)];
    assert.deepEqual(X.historyFor(hist, root).map(e => e.timestamp), [3, 1]);
    assert.deepEqual(X.historyFor(hist, root, new Set(['3'])).map(e => e.timestamp), [1], 'a live session is not done');
    assert.deepEqual(X.codeParts('`npm run lint`（一字不差）直接允許'), [{ code: true, text: 'npm run lint' }, { code: false, text: '（一字不差）直接允許' }]);
    assert.equal(X.historyTitle(h('/r', 1, { customTitle: null, aiTitle: 'AI', display: 'D' })), 'AI');

    const rec = (project, date, tokens, cost, model = 'm1') => ({ project, date, totalTokens: tokens, cost, costAvailable: true, model, inputTokens: 0, outputTokens: 0, cachedInputTokens: 0, reasoningOutputTokens: 0 });
    const cost = { projectCosts: [
        { project: '/r/app', sessions: [rec('/r/app', '2026-11-06', 100, 1), rec('/r/app', '2026-11-02', 300, 3, 'm2'), rec('/r/app', '2026-10-30', 1000, 10)] },
        { project: '/r/app/.worktrees/x', sessions: [rec('/r/app/.worktrees/x', '2026-11-06', 50, 0.5)] },
        { project: '/r/other', sessions: [rec('/r/other', '2026-11-05', 550, 5)] }
    ] };
    const s = X.spendFor(cost, root, '2026-11-06');
    assert.equal(s.today, 150);
    assert.equal(s.month, 450);
    assert.equal(s.total, 1450);
    assert.equal(s.monthShare, 45);
    assert.deepEqual(s.byModel.map(m => [m.model, m.tokens]), [['m1', 1150], ['m2', 300]]);
    assert.equal(X.spendFor(cost, '/r/none', '2026-11-06'), null);
    assert.equal(X.spendFor(null, root, '2026-11-06'), null);

    const mem = [{ projectPath: '/r/app', memoryDirPath: '/m/app', files: [{ filename: 'MEMORY.md', content: 'x' }] }, { projectPath: '/r/other', memoryDirPath: '/m/o', files: [{ filename: 'MEMORY.md', content: 'y' }] }];
    assert.deepEqual(X.memoryFor(mem, root).map(f => f.dir), ['/m/app']);
});

test('fixture: every demo workspace has history, spend and memory in its own root; rules match R7 §A.6', () => {
    const today = X.localDay(Date.now());
    const hist = fixture.glassDemoHistory(), cost = fixture.glassDemoCost(), mem = fixture.glassDemoMemory();
    for (const w of gaps.DEMO_WORKSPACES) {
        assert(X.historyFor(hist, w.root).length > 0, `${w.id} history`);
        assert(X.spendFor(cost, w.root, today)?.today > 0, `${w.id} spend today`);
    }
    assert.equal(X.memoryFor(mem, fixture.GLASS_DEMO_ROOTS.flockbook).length, 1);
    const ws = id => gaps.DEMO_WORKSPACES.find(w => w.id === id);
    assert.deepEqual(X.rulesFor(gaps.DEMO_RULES, ws('flockbook'), gaps.DEMO_GOALS).map(r => r.kind), ['金鑰', '指示']);
    assert.deepEqual(X.rulesFor(gaps.DEMO_RULES, ws('jumpmark'), gaps.DEMO_GOALS).map(r => r.kind), ['授權']);
    assert.deepEqual(X.rulesFor(gaps.DEMO_RULES, ws('c9watch'), gaps.DEMO_GOALS).map(r => r.kind), ['界線']);
    assert.deepEqual(X.rulesFor(gaps.DEMO_RULES, ws('cross'), gaps.DEMO_GOALS), []);
    assert.match(workspaceId('/r/app'), /^ws_[0-9a-f]{8}$/);
});
