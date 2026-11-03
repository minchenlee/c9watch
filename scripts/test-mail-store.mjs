import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import ts from 'typescript';
import { test } from 'node:test';

/** Load a type-only-import TS module in a fresh context with fake timers. */
function load(path) {
    const exports = {};
    const timers = [];
    const source = readFileSync(new URL(path, import.meta.url), 'utf8');
    vm.runInNewContext(ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText, {
        exports, require() { throw new Error('module has no runtime dependencies'); }, Map, Set, JSON, Date, Error, Object, Promise,
        setTimeout(fn, ms) { timers.push({ fn, ms }); return timers.length; }, clearTimeout() {}
    });
    return { ...exports, timers };
}
const store = () => load('../src/lib/data/mail-store.ts');
const settle = async () => { for (let i = 0; i < 20; i++) await Promise.resolve(); };

// Rows in the exact camelCase shape core's MsgSummary serializes to.
const addr = (id, name) => ({ id, name, addr: `m1/${id}` });
const row = (msgId, createdAt, more = {}) => ({
    msgId, createdAt, stateAt: createdAt, state: 'queued', detail: null, kind: 'request', hop: 0, noticeId: null,
    replyTo: null, threadId: msgId, via: null, preview: `body ${msgId}`,
    from: { kind: 'human', addr: null, name: null }, to: addr('agt_b', 'bob'), ...more
});
const event = data => ({ v: 1, type: 'event', topic: 'msg.state', sub: '1', cursor: 'c', ts: data.at, data });
const denied = { code: 'not_allowed', msg: 'Method is not exposed to this window', retryable: false };

test('snapshot pages with before, buffers events during the read, then applies them in order', async () => {
    const mail = store(); const seen = []; const calls = []; let receive;
    const transport = {
        async request(method, p) {
            calls.push([method, p]);
            if (method === 'agent.list') return { agents: [{ id: 'agt_b', sessionKey: 'claudeCode:s1' }, { id: 'agt_x', sessionKey: null }] };
            if (method === 'msg.read') return { ...row('msg_new', 50, { state: 'delivered', stateAt: 60 }), body: 'full', attempts: 1, provenance: 'p', status: [] };
            assert.equal(method, 'msg.list');
            if (!p.before) {
                // An event lands while the snapshot is read; it must not be lost.
                receive(event({ msgId: 'msg_1', state: 'delivering', at: 20, attempts: 1, detail: null, from: 'local-unverified', to: 'agt_b', via: 'paste' }));
                return { items: [row('msg_2', 11)], next: 'msg_2' };
            }
            assert.equal(p.before, 'msg_2');
            return { items: [row('msg_1', 10)], next: null };
        },
        async subscribe(p, cb) { assert.deepEqual([...p.topics], ['msg.state']); assert.equal(p.since, undefined); receive = cb; return () => {}; }
    };
    const feed = mail.mailFeed(transport, s => seen.push(s));
    assert.equal(seen[0].status, 'loading');
    await settle();
    const last = seen.at(-1);
    assert.equal(last.status, 'ready');
    assert.equal(last.messages.get('msg_1').state, 'delivering');
    assert.equal(last.messages.get('msg_1').via, 'paste');
    assert.equal(last.sessionKeys.get('agt_b'), 'claudeCode:s1');
    assert.equal(last.sessionKeys.has('agt_x'), false);
    assert.deepEqual(calls.filter(c => c[0] === 'msg.list').map(c => c[1].limit), [200, 200]);

    // A stale event (older than the row's state) is ignored.
    receive(event({ msgId: 'msg_1', state: 'queued', at: 5, attempts: 0, detail: null, from: 'local-unverified', to: 'agt_b' }));
    assert.equal(seen.at(-1).messages.get('msg_1').state, 'delivering');
    // An unknown `queued` event carries the summary.
    receive(event({ msgId: 'msg_3', state: 'queued', at: 30, attempts: 0, detail: null, from: 'agent:agt_a', to: 'agt_b',
        sender: { kind: 'agent', addr: 'm1/agt_a', name: 'alice' }, recipient: addr('agt_b', 'bob'), kind: 'info', preview: 'hi', threadId: 'msg_3', replyTo: null, via: null }));
    assert.equal(seen.at(-1).messages.get('msg_3').from.name, 'alice');
    // An unknown later event is read with msg.read, without the body.
    receive(event({ msgId: 'msg_new', state: 'delivered', at: 60, attempts: 1, detail: 'prompt_hook', from: 'local-unverified', to: 'agt_b' }));
    await settle();
    const read = seen.at(-1).messages.get('msg_new');
    assert.equal(read.state, 'delivered');
    assert.equal('body' in read, false);
    feed.stop();
});

test('not_allowed from the bridge is terminal: unavailable, no retry, no list call', async () => {
    for (const phase of ['subscribe', 'list', 'stream']) {
        const mail = store(); const seen = []; let lists = 0; let receive; let stops = 0;
        const transport = {
            async request(method) { if (method === 'msg.list') { lists++; if (phase === 'list') throw denied; return { items: [row('m', 1)], next: null }; } return { agents: [] }; },
            async subscribe(_p, cb) { if (phase === 'subscribe') throw denied; receive = cb; return () => { stops++; }; }
        };
        mail.mailFeed(transport, s => seen.push(s));
        await settle();
        if (phase === 'stream') { receive({ v: 1, ok: false, error: denied }); assert.equal(stops, 1); }
        assert.equal(seen.at(-1).status, 'unavailable', phase);
        assert.equal(seen.at(-1).error, 'Method is not exposed to this window');
        assert.equal(mail.timers.length, 0, `${phase}: no retry`);
        if (phase === 'subscribe') assert.equal(lists, 0);
    }
    const mail = store();
    for (const code of ['not_allowed', 'unknown_method', 'upgrade_required']) assert.equal(mail.isUnavailableError({ code }), true);
    assert.equal(mail.isUnavailableError({ code: 'busy' }), false);
    assert.equal(mail.isUnavailableError(new Error('x')), false);
});

test('a transient failure keeps rows, retries, and an own send survives the refresh', async () => {
    const mail = store(); const seen = []; let n = 0; const receivers = [];
    const transport = {
        async request(method) {
            if (method === 'agent.list') throw new Error('agent.list is optional');
            n++;
            if (n === 2) throw Object.assign(new Error('offline'), { code: 'core_unavailable' });
            return { items: [row('msg_1', 10)], next: null };
        },
        async subscribe(_p, cb) { receivers.push(cb); return () => {}; }
    };
    let clock = 100;
    const feed = mail.mailFeed(transport, s => seen.push(s), () => clock);
    await settle();
    assert.equal(seen.at(-1).status, 'ready');
    receivers[0]({ v: 1, ok: false, error: { code: 'core_unavailable', msg: 'offline' } });
    assert.equal(seen.at(-1).status, 'error');
    assert.equal(seen.at(-1).messages.size, 1, 'rows stay on screen');
    assert.equal(mail.timers.length, 1);
    // Retry 1 fails at msg.list; retry 2 succeeds.
    mail.timers.shift().fn(); await settle();
    assert.equal(seen.at(-1).status, 'error');
    const sent = mail.rowFromSend({ msgId: 'msg_mine', cursor: 'c', replayed: false, state: 'queued', to: addr('agt_b', 'bob') }, 'x'.repeat(300), 150);
    assert.equal(sent.preview.length, 200);
    feed.upsert(sent);
    clock = 120;
    mail.timers.shift().fn(); await settle();
    assert.equal(seen.at(-1).status, 'ready');
    assert(seen.at(-1).messages.has('msg_mine'), 'own send added during the read stays');
    // A stale upsert does not roll a row back.
    feed.upsert({ ...sent, state: 'delivered', stateAt: 200 });
    feed.upsert({ ...sent, state: 'queued', stateAt: 150 });
    assert.equal(seen.at(-1).messages.get('msg_mine').state, 'delivered');
    feed.stop();
});

test('threads: one per agent, agent mail in both threads, attention and labels', () => {
    const mail = store();
    const a2b = row('m2', 20, { from: { kind: 'agent', addr: 'm1/agt_a', name: 'alice' }, state: 'uncertain', detail: 'paste not seen' });
    const held = row('m3', 30, { to: addr('agt_a', 'alice'), state: 'delivering', detail: 'held: permission' });
    const sys = row('m4', 40, { from: { kind: 'engine', addr: null, name: null }, kind: 'system', noticeId: 'm2', state: 'delivered' });
    const mine = row('m1', 10);
    const threads = mail.threadsOf([mine, a2b, held, sys]);
    assert.deepEqual(Array.from(threads, t => t.agentId), ['agt_b', 'agt_a']);
    const bob = threads[0];
    assert.deepEqual(Array.from(bob.messages, m => m.msgId), ['m1', 'm2', 'm4']);
    assert.equal(bob.attention, 1);
    assert.equal(bob.queued, 1);
    const alice = threads[1];
    assert.deepEqual(Array.from(alice.messages, m => m.msgId), ['m2', 'm3']);
    assert.equal(alice.attention, 1, 'only mail TO alice counts for her');
    const own = new Set(['m1']);
    assert.equal(mail.sourceOf(mine, own), 'you');
    assert.equal(mail.sourceOf(row('m9', 1), own), 'terminal');
    assert.equal(mail.sourceOf(a2b, own), 'agent');
    assert.equal(mail.sourceOf(sys, own), 'engine');
    assert.equal(mail.stateLabel(held), 'HELD');
    assert.equal(mail.isHeld({ state: 'delivered', detail: 'held: x' }), false);
    assert.equal(mail.stateLabel({ state: 'delivering', detail: null }), 'SENDING');
    assert.equal(mail.stateLabel({ state: 'cancelled', detail: null }), 'CANCELLED');
    assert.equal(mail.routeLabel('socket'), 'PEER');
    assert.equal(mail.routeLabel('paste'), 'PASTE');
    assert.equal(mail.routeLabel(null), null);
    assert.equal(mail.agentIdOf('m1/agt_a'), 'agt_a');
});

test('demo mailbox drives the same feed: list, send walks states, cancel only queued', async () => {
    const demo = load('../src/lib/data/demo/mail.ts');
    const mail = store(); const seen = [];
    const transport = { async request(m, p) { return demo.demoMailRequest(m, p); }, async subscribe(_p, cb) { return demo.demoMailSubscribe(data => cb(event(data))); } };
    const feed = mail.mailFeed(transport, s => seen.push(s));
    await settle();
    const snap = seen.at(-1);
    assert.equal(snap.status, 'ready');
    assert.equal(snap.sessionKeys.get('agt_demo_pm'), 'claudeCode:demo-1');
    const states = new Set([...snap.messages.values()].map(m => mail.stateLabel(m)));
    for (const s of ['QUEUED', 'HELD', 'UNCERTAIN', 'FAILED', '✓ DELIVERED']) assert(states.has(s), s);
    const sent = demo.demoMailRequest('msg.send', { to: 'reviewer', body: 'hello' });
    assert.equal(seen.at(-1).messages.get(sent.msgId).state, 'queued', 'the queued event alone makes the row');
    for (const t of demo.timers.splice(0)) t.fn();
    assert.equal(seen.at(-1).messages.get(sent.msgId).state, 'delivered');
    assert.throws(() => demo.demoMailRequest('msg.cancel', { msgId: sent.msgId }), e => e.code === 'not_cancellable');
    demo.demoMailRequest('msg.cancel', { msgId: 'msg_demo_08' });
    assert.equal(seen.at(-1).messages.get('msg_demo_08').state, 'cancelled');
    assert.equal(demo.demoMailRequest('msg.read', { msgId: 'msg_demo_01' }).body.startsWith('Plan the OAuth2'), true);
    feed.stop();
});
