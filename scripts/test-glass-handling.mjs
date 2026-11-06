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
const { buildDetail, primaryState, emptyAnswer, waitText, fieldStep, answerBody, answerLabel, noBody } = await load('./detail', '../src/shells/glass/handling/detail.ts');
const { SendQueue, secondsLeft, stepFrom } = await load('./actions', '../src/shells/glass/handling/actions.ts');

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
    // G5: a key is never sent as mail (it would stay in the mail log).
    assert.deepEqual(primaryState(give, { ...a, input: 'k' }, false), { text: 'c9watch 還不能存 key', off: true, chat: false });
    assert.equal(answerBody(give, { ...a, input: 'k' }), null);
    assert.equal(give.reversible, false);
});

test('offline: nothing is kept for later, so the main button is off and says so', () => {
    const list = demoDetails();
    const a = emptyAnswer();
    assert.deepEqual(primaryState(byVerb(list, '處理'), a, true), { text: '斷線了，連上後再送', off: true, chat: false });
    assert.equal(primaryState(byVerb(list, '給'), a, true).off, true);
    for (const d of list) assert(!primaryState(d, { ...a, input: 'k', free: 'x', reject: 'x', text: 'x' }, true).text.includes('先存著'));
});

test('demo: answers go only to managed agents; the observed talk is ⌘O only', () => {
    const list = demoDetails();
    for (const v of ['審', '決定', '處理', '給']) assert.equal(byVerb(list, v).managed, true, v);
    const allow = byVerb(list, '允許');
    assert.equal(allow.managed, false);
    assert.equal(allow.canAnswer, false);
    assert.equal(answerBody(allow, emptyAnswer()), null);
});

test('answer bodies and labels', () => {
    const list = demoDetails();
    const a = emptyAnswer();
    const review = byVerb(list, '審'), decide = byVerb(list, '決定'), fix = byVerb(list, '處理'), give = byVerb(list, '給');
    assert.equal(answerBody(review, a), '驗收：MONITOR 改用新詞表');
    assert.equal(answerBody(review, { ...a, opt: 1 }), null);
    assert.equal(answerBody(review, { ...a, opt: 1, reject: ' 太長 ' }), '退回：MONITOR 改用新詞表\n原因：太長');
    assert.equal(answerBody(decide, a), '要不要改狀態色？\n我的回答：只有「需要你」上色，其他用灰階');
    assert.equal(answerBody(decide, { ...a, opt: 3, free: '先改 AGENTS' }), '先改 AGENTS');
    assert.equal(answerBody(fix, { ...a, opt: 2 }), '匯出 CSV 的測試環境\n我的回答：先跳過這一項');
    assert.equal(answerLabel(review, { ...a, opt: 1 }), '審 MONITOR 改用新詞表 · 退回');
    assert.equal(answerLabel(fix, a), '處理 匯出 CSV 的測試環境 · 測試改用 5433 port');
    assert.equal(noBody(give), '我沒有這把 key（STAGING_API_KEY）。請先跳過要它的步驟。');
    assert.equal(noBody(review), null);
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

test('key field: J / K step only when empty and no input method composes', () => {
    assert.equal(fieldStep({ code: 'KeyJ' }, ''), 1);
    assert.equal(fieldStep({ code: 'KeyK' }, ''), -1);
    assert.equal(fieldStep({ code: 'KeyJ' }, 'abc'), 0);
    // WebKit sends keyCode 229 for keys the input method takes, isComposing false.
    assert.equal(fieldStep({ code: 'KeyJ', keyCode: 229, isComposing: false }, ''), 0);
    assert.equal(fieldStep({ code: 'KeyK', isComposing: true }, ''), 0);
    assert.equal(fieldStep({ code: 'KeyJ', metaKey: true }, ''), 0);
    assert.equal(fieldStep({ code: 'KeyA' }, ''), 0);
    // ⌘Z in the empty field undoes the last send; with text it is typing undo.
    assert.equal(fieldStep({ code: 'KeyZ', metaKey: true }, ''), 'undo');
    assert.equal(fieldStep({ code: 'KeyZ', metaKey: true }, 'k'), 0);
    assert.equal(fieldStep({ code: 'KeyZ', metaKey: true, isComposing: true }, ''), 0);
});

/** Fake timers for SendQueue. */
function fakeTimers() {
    let t = 0, n = 0;
    const due = new Map();
    return {
        now: () => t,
        set: (fn, ms) => { const id = ++n; due.set(id, { at: t + ms, fn }); return id; },
        clear: id => { due.delete(id); },
        async advance(ms) {
            t += ms;
            for (const [id, x] of [...due].sort((a, b) => a[1].at - b[1].at)) if (x.at <= t) { due.delete(id); x.fn(); }
            for (let i = 0; i < 5; i++) await Promise.resolve();
        }
    };
}

test('send queue: nothing is sent before 10 s; undo inside 10 s sends nothing', async () => {
    const tm = fakeTimers(), events = [], sent = [];
    const q = new SendQueue(e => events.push(e.kind), tm);
    q.schedule('a', 'A', async () => { sent.push('a'); });
    await tm.advance(9_999);
    assert.deepEqual(sent, []);
    assert.equal(q.undo()?.itemId, 'a');
    await tm.advance(10_000);
    assert.deepEqual(sent, []);
    assert.deepEqual(events, ['undone']);
    assert.equal(q.list.length, 0);
});

test('send queue: timers never overwrite each other (handoff tests #2, #3)', async () => {
    const tm = fakeTimers(), sent = [];
    const q = new SendQueue(() => {}, tm);
    const a = q.schedule('a', 'A', async () => { sent.push('a'); });
    await tm.advance(4_000);
    q.schedule('b', 'B', async () => { sent.push('b'); });
    assert.deepEqual(q.list.map(p => p.itemId), ['a', 'b']);
    // ⌘Z takes the newest; the older one keeps its own deadline.
    assert.equal(q.undo()?.itemId, 'b');
    assert.equal(secondsLeft(q.list[0], tm.now()), 6);
    await tm.advance(6_000);
    assert.deepEqual(sent, ['a']);
    assert.equal(q.undo(a), null);
});

test('send queue: undo by id; a failure is reported once and not sent again; cancelAll sends nothing', async () => {
    const tm = fakeTimers(), events = [];
    let calls = 0;
    const q = new SendQueue(e => events.push([e.kind, e.pending.itemId, e.error]), tm);
    const a = q.schedule('a', 'A', async () => { calls++; throw { code: 'unavailable', msg: 'core is gone' }; });
    const b = q.schedule('b', 'B', async () => { calls++; });
    q.undo(a);
    assert.deepEqual(q.list.map(p => p.id), [b]);
    q.schedule('c', 'C', async () => { calls++; throw new Error('boom'); });
    await tm.advance(10_000);
    assert.deepEqual(events, [['undone', 'a', undefined], ['sent', 'b', undefined], ['failed', 'c', 'boom']]);
    assert.equal(calls, 2);
    await tm.advance(60_000);
    assert.equal(calls, 2);
    q.schedule('d', 'D', async () => { calls++; });
    assert.equal(q.has('d'), true);
    assert.equal(q.cancelAll().length, 1);
    await tm.advance(10_000);
    assert.equal(calls, 2);
});

test('stepFrom: J / K skip handled items; a receipt moves on from where it was', () => {
    const order = ['a', 'b', 'c', 'd'];
    assert.equal(stepFrom(order, new Set(), 'a', 1), 'b');
    assert.equal(stepFrom(order, new Set(['b']), 'a', 1), 'c');
    assert.equal(stepFrom(order, new Set(['b', 'c']), 'd', -1), 'a');
    assert.equal(stepFrom(order, new Set(['d']), 'c', 1), 'a');
    // 'b' is handled and shows its receipt: J goes to the item after it, not to the first.
    assert.equal(stepFrom(order, new Set(['a', 'b']), 'b', 1), 'c');
    assert.equal(stepFrom(order, new Set(['a', 'b', 'c', 'd']), 'b', 1), null);
    assert.equal(stepFrom(order, new Set(), null, 1), 'a');
    assert.equal(stepFrom(order, new Set(), null, -1), 'd');
});
