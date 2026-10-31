import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { C9pClient, C9P_API_MINOR, C9P_PUBLIC_METHODS, C9P_PUBLIC_TOPICS } from '../dist/index.js';
import { sdkTypes } from '../../../scripts/gen-c9p-sdk.mjs';

const tick = () => new Promise((r) => setImmediate(r));
function hello(overrides = {}) {
  return { apiMajor: 1, apiMinor: 6, methods: Object.fromEntries(C9P_PUBLIC_METHODS.map((m) => [m, { since: 1, rev: 1, stability: 'preview' }])),
    topics: ['session.view'], breakingSince: [], cursor: 'm:e:0', capabilities: [], ...overrides };
}
class Fake {
  sent = [];
  open(onFrame, onClose) { this.receive = onFrame; this.disconnect = onClose; return Promise.resolve(); }
  send(frame) {
    this.sent.push(frame);
    if (frame.method === 'hello') this.answer(frame, this.hello ?? hello());
    else if (frame.method === 'events.subscribe') {
      this.answer(frame, { cursor: frame.params.since ?? 'm:e:0' });
      this.sub = frame.id;
    } else if (frame.method === 'events.unsubscribe') this.answer(frame, { ended: true });
    else if (frame.method === 'ping') this.answer(frame, { ts: 7 });
  }
  answer(frame, result) { this.receive({ v: 1, id: frame.id, ok: true, result }); }
  event(cursor, data = { sessionKey: 'claudeCode:a' }) {
    this.receive({ v: 1, type: 'event', sub: this.sub, topic: 'session.view', cursor, ts: 1, data });
  }
  close() {}
}
test('generated types are public and current', () => {
  assert.equal(readFileSync(new URL('../src/types.ts', import.meta.url), 'utf8'), sdkTypes());
  for (const m of C9P_PUBLIC_METHODS) assert(!m.startsWith('hook.') && m !== 'task.done' && m !== 'agent.handoff');
  const types = sdkTypes();
  assert(!types.includes('resumeCount:'));
  assert(!types.includes('C9pHookParams'));
  const statusType = types.split('export type C9pSessionStatus =')[1].split(';')[0];
  assert.match(statusType, /\|\s*string/);
});
test('hello sends build minor; unknown and breaking methods never reach transport', async () => {
  const t = new Fake(); t.hello = hello({ breakingSince: ['session.views'] });
  const c = new C9pClient(() => t); await c.connect();
  assert.equal(t.sent[0].params.apiMinor, C9P_API_MINOR);
  await assert.rejects(c.request('session.views', {}), /breaking_preview/);
  await assert.rejects(c.request('hook.stop', {}), /not_allowed/);
  delete c.hello.methods['cost.get'];
  await assert.rejects(c.request('cost.get', {}), /unsupported/);
  assert.equal(t.sent.length, 1); c.close();
});
test('multiplexes responses, rejects pending calls on close, never retries writes', async () => {
  const t = new Fake(); const c = new C9pClient(() => t); await c.connect();
  const a = c.request('agent.retire', { agentId: 'a' });
  assert.deepEqual(await c.request('ping', {}), { ts: 7 });
  t.disconnect(new Error('connection lost'));
  await assert.rejects(a, /connection lost/);
  assert.equal(t.sent.filter((f) => f.method === 'agent.retire').length, 1); c.close();
});
test('reconnect resumes after the last completed callback, not an in-flight one', async () => {
  const transports = [new Fake(), new Fake()]; let index = 0;
  const c = new C9pClient(() => transports[index++]); await c.connect();
  const seen = []; let release;
  const sub = await c.subscribe({ topics: ['session.view'] }, async (e) => {
    seen.push(e.cursor);
    if (e.cursor === 'm:e:1') await new Promise((r) => { release = r; });
  }, (e) => assert.fail(e.message));
  transports[0].event('m:e:1'); await tick();
  assert.equal(sub.cursor, 'm:e:0');
  transports[0].disconnect(new Error('offline'));
  const reconnect = c.reconnect(); await tick();
  assert.equal(index, 1);
  release(); await reconnect;
  assert.equal(transports[1].sent.find((f) => f.method === 'events.subscribe').params.since, 'm:e:1');
  transports[1].event('m:e:2'); await tick();
  assert.equal(sub.cursor, 'm:e:2'); assert.deepEqual(seen, ['m:e:1', 'm:e:2']);
  await sub.unsubscribe(); assert(transports[1].sent.some((f) => f.method === 'events.unsubscribe')); c.close();
});
test('stream cursor_expired is exposed and not silently reset', async () => {
  const t = new Fake(); const c = new C9pClient(() => t); await c.connect();
  const errors = [];
  const sub = await c.subscribe({ since: 'm:e:0' }, () => {}, (e) => errors.push(e));
  t.receive({ v: 1, id: t.sub, ok: false, error: { code: 'cursor_expired', msg: 'pruned' } });
  t.event('m:e:4'); await tick();
  assert.equal(errors[0].code, 'cursor_expired'); assert.equal(sub.cursor, 'm:e:0'); c.close();
});
test('callback failure preserves cursor and bounds slow consumers', async () => {
  const t = new Fake(); const c = new C9pClient(() => t, { maxQueuedEvents: 1 }); await c.connect();
  const errors = [];
  const sub = await c.subscribe({}, () => { throw new Error('consumer failed'); }, (e) => errors.push(e));
  t.event('m:e:1'); await tick();
  assert.equal(sub.cursor, 'm:e:0'); assert.match(errors[0].message, /consumer failed/);
  c.close();
  const t2 = new Fake(); const c2 = new C9pClient(() => t2, { maxQueuedEvents: 1 }); await c2.connect();
  await c2.subscribe({}, () => {}, (e) => errors.push(e));
  t2.event('m:e:1'); t2.event('m:e:2'); await tick();
  assert.match(errors.at(-1).message, /fell behind/); assert.equal(c2.hello, undefined); c2.close();
});
test('request timeout rejects and major mismatch is refused', async () => {
  const t = new Fake(); const c = new C9pClient(() => t, { timeoutMs: 10 }); await c.connect();
  await assert.rejects(c.request('agent.retire', { agentId: 'x' }), /timeout/); c.close();
  const t2 = new Fake(); t2.hello = hello({ apiMajor: 2 });
  await assert.rejects(new C9pClient(() => t2).connect(), /upgrade_required/);
});

test('subscriptions expand only public advertised topics and drop private frames', async () => {
  const t = new Fake(); t.hello = hello({ topics: [...C9P_PUBLIC_TOPICS, 'legacy.inbox'] });
  const c = new C9pClient(() => t); await c.connect();
  const seen = [];
  const sub = await c.subscribe({}, (e) => seen.push(e), (e) => assert.fail(e.message));
  assert.deepEqual(t.sent.find((f) => f.method === 'events.subscribe').params.topics, C9P_PUBLIC_TOPICS);
  await assert.rejects(c.subscribe({ topics: ['legacy.inbox'] }, () => {}, () => {}), /not_allowed/);
  t.receive({ v: 1, type: 'event', sub: t.sub, topic: 'legacy.inbox', cursor: 'm:e:1', data: { secret: 'private' } });
  await tick(); assert.equal(seen.length, 0); await sub.unsubscribe(); c.close();
});
test('a close in the hello response cannot leave a falsely connected client', async () => {
  const t = new Fake();
  t.send = (frame) => { t.answer(frame, hello()); t.disconnect(new Error('closed during hello')); };
  const c = new C9pClient(() => t);
  await assert.rejects(c.connect(), /Disconnected during hello/);
  assert.equal(c.hello, undefined); c.close();
});

test('failed reconnect retains active subscriptions for the next attempt', async () => {
  const first = new Fake(), failed = new Fake(), last = new Fake();
  failed.open = () => Promise.reject(new Error('still offline'));
  const transports = [first, failed, last]; let index = 0;
  const c = new C9pClient(() => transports[index++]); await c.connect();
  const sub = await c.subscribe({}, () => {}, (e) => assert.fail(e.message));
  first.event('m:e:1'); await tick(); first.disconnect(new Error('offline'));
  await assert.rejects(c.reconnect(), /still offline/);
  assert.equal(sub.cursor, 'm:e:1');
  await c.reconnect();
  assert.equal(last.sent.find((f) => f.method === 'events.subscribe').params.since, 'm:e:1');
  await sub.unsubscribe(); c.close();
});
test('close cancels a reconnect waiting for an application callback', async () => {
  const t = new Fake(); let opened = 0, release;
  const c = new C9pClient(() => { opened++; return t; }); await c.connect();
  await c.subscribe({}, () => new Promise((r) => { release = r; }), () => {});
  t.event('m:e:1'); await tick();
  const reconnect = c.reconnect();
  c.close(); release();
  await assert.rejects(reconnect, /cancelled/);
  assert.equal(opened, 1); assert.equal(c.hello, undefined);
});
