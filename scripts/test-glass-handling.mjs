import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import ts from 'typescript';

// The handling panel's pure part (src/shells/glass/handling/detail.ts), on
// the glass demo fixture and on live-shaped rows. Same loader as
// test-glass-model.mjs: each file is transpiled, imports point at loaded modules.
const urls = {};
async function load(spec, path) {
    let out = ts.transpileModule(readFileSync(new URL(path, import.meta.url), 'utf8'), {
        compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022 }
    }).outputText;
    for (const [s, url] of Object.entries(urls)) out = out.replaceAll(`from '${s}'`, `from '${url}'`);
    assert(!/from '(\.|\$lib)/.test(out), `${path} has an unresolved import`);
    urls[spec] = 'data:text/javascript;base64,' + Buffer.from(out).toString('base64');
    return import(urls[spec]);
}
const M = '../src/shells/glass/model/';
await load('./stale', M + 'stale.ts');
await load('./workspaces', M + 'workspaces.ts');
await load('./rooms', M + 'rooms.ts');
await load('$lib/data/mail-store', '../src/lib/data/mail-store.ts');
await load('./needs', M + 'needs.ts');
const { deriveGlassModel } = await load('./model', M + 'index.ts');
const fixture = await load('$lib/data/demo/glass', '../src/lib/data/demo/glass.ts');
const gaps = await load('../model/gaps-demo', M + 'gaps-demo.ts');
urls['./gaps-demo'] = urls['../model/gaps-demo'];
const { buildDetail, primaryState, emptyAnswer, waitText, CANNOT_ANSWER } = await load('./detail', '../src/shells/glass/handling/detail.ts');

const NOW = 1_800_000_000_000;
const GAPS = { workspaces: gaps.DEMO_WORKSPACES, goals: gaps.DEMO_GOALS, needs: gaps.DEMO_NEEDS };
const demoModel = () => deriveGlassModel({ sessions: fixture.glassDemoSessionViews(), agents: fixture.glassDemoAgents(), gaps: GAPS, now: Date.now() });
const demoDetails = () => { const m = demoModel(); return m.needs.map(n => buildDetail(n, m.workspaces, gaps.DEMO_NEED_DETAILS)); };
const byVerb = (list, verb) => list.find(d => d.verb === verb);

test('demo: one detail per verb, located by workspace and row', () => {
    const list = demoDetails();
    assert.deepEqual(list.map(d => d.verb).sort(), ['允許', '決定', '審', '給', '處理'].sort());
    assert.equal(byVerb(list, '審').where, 'C9WATCH › 狀態用詞統一');
    assert.equal(byVerb(list, '允許').where, 'JUMPMARK › 對談 · main');
    assert.equal(byVerb(list, '給').who, 'w-login');
});

test('D9 = C: only 允許 has the 「以後直接做」 toggle', () => {
    for (const d of demoDetails()) assert.equal(d.rule !== null, d.verb === '允許', d.verb);
});

test('允許 cannot be answered yet: the main button opens the chat', () => {
    const d = byVerb(demoDetails(), '允許');
    assert.equal(d.canAnswer, false);
    assert.equal(d.no, null);
    assert.match(d.note, /⌘O/);
    assert.deepEqual(primaryState(d, emptyAnswer(), false), { text: '在終端機回答', off: false, chat: true });
    // Offline does not change it: ⌘O is local.
    assert.equal(primaryState(d, emptyAnswer(), true).chat, true);
    assert.deepEqual(d.facts.map(f => f.confidence), ['確定', '估計', '不確定']);
});

test('the main button says what ⌘↵ does; typing never changes the option', () => {
    const list = demoDetails();
    const review = byVerb(list, '審'), decide = byVerb(list, '決定'), give = byVerb(list, '給');
    const a = emptyAnswer();
    assert.equal(primaryState(review, a, false).text, '驗收');
    assert.deepEqual(primaryState(review, { ...a, opt: 1 }, false), { text: '先寫退回原因', off: true, chat: false });
    assert.equal(primaryState(review, { ...a, opt: 1, reject: '太長' }, false).text, '退回並送出');
    assert.equal(primaryState(review, { ...a, reject: '太長' }, false).text, '驗收');
    assert.equal(primaryState(decide, a, false).text, '送出：只有「需要你」上色');
    assert.equal(primaryState(decide, { ...a, opt: 3 }, false).text, '先寫你的回答');
    assert.equal(primaryState(decide, { ...a, opt: 3, free: 'ok' }, false).text, '送出我寫的回答');
    assert.deepEqual(primaryState(give, a, false), { text: '先貼上 key', off: true, chat: false });
    assert.equal(primaryState(give, { ...a, input: 'k' }, false).text, '存入 key（不能復原）');
    assert.equal(give.reversible, false);
});

test('offline: the main button keeps the answer for later, but an empty answer stays off', () => {
    const list = demoDetails();
    const a = emptyAnswer();
    assert.equal(primaryState(byVerb(list, '處理'), a, true).text, '先存著，連上後送出');
    assert.equal(primaryState(byVerb(list, '給'), a, true).off, true);
    assert.equal(primaryState(byVerb(list, '給'), { ...a, input: 'k' }, true).text, '先存著，連上後送出');
});

test('live: no demo text; an observed session can only be answered in its terminal', () => {
    const m = deriveGlassModel({ sessions: fixture.glassDemoSessionViews(), agents: fixture.glassDemoAgents(), gaps: null, now: Date.now() });
    const list = m.needs.map(n => buildDetail(n, m.workspaces, null));
    // Without the gaps file only the c9p facts are left: 允許 and the question.
    assert.deepEqual(list.map(d => d.verb).sort(), ['允許', '決定'].sort());
    const demoText = JSON.stringify(gaps.DEMO_NEED_DETAILS.map(d => d.why));
    for (const d of list) {
        assert.equal(d.shots, null);
        assert.equal(d.options, null);
        assert(!d.why || !demoText.includes(d.why), `demo why in live: ${d.why}`);
    }
    const allow = byVerb(list, '允許');
    assert.equal(allow.canAnswer, false);
    assert.equal(allow.code, '$ npm run build && npm run test:e2e\n  在 /Users/demo/code/jumpmark');
    assert.equal(allow.facts.length, 1);
});

test('live: a managed agent gets a text box; an attention reason gets plain words', () => {
    const path = '/Users/x/repo';
    const agent = { id: 'agt_1', cwd: path, name: 'w-a', role: 'worker', parentId: null, sessionKey: null, status: 'working', state: 'working',
        goneAt: null, lastSeen: NOW - 60_000, createdAt: NOW - 3_600_000, mailPaused: false, generation: 1, source: 'engine', control: 'pty' };
    const attention = [{ event: { id: 'agt_1', reason: 'auto_mode_denied', generation: 1 }, at: NOW - 120_000 }];
    const m = deriveGlassModel({ sessions: [], agents: [agent], attention, now: NOW });
    const [d] = m.needs.map(n => buildDetail(n, m.workspaces, null));
    assert.equal(d.verb, '處理');
    assert.equal(d.title, '自動模式擋下了一個動作');
    assert.equal(d.canAnswer, true);
    assert.equal(d.text.label, '給 w-a 的新指示');
    assert.equal(primaryState(d, emptyAnswer(), false).off, true);
    assert.equal(primaryState(d, { ...emptyAnswer(), text: '換個做法' }, false).text, '送出');
});

test('wait text', () => {
    assert.equal(waitText(NOW - 30_000, NOW), '不到 1 分');
    assert.equal(waitText(NOW - 3 * 60_000, NOW), '3 分');
    assert.equal(waitText(NOW - 65 * 60_000, NOW), '1 小時 5 分');
    assert.equal(waitText(NOW - 120 * 60_000, NOW), '2 小時');
});
