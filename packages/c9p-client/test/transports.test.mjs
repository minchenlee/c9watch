import test from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:net';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createHmac, generateKeyPairSync, sign } from 'node:crypto';
import { UnixTransport } from '../dist/transport-unix.js';
import { WsTransport, pair } from '../dist/transport-ws.js';

const tick = () => new Promise((r) => setImmediate(r));
test('unix buffers split UTF-8 and multiple frames, enforces byte limit', async (t) => {
  const dir = await mkdtemp(join(tmpdir(), 'c9p-sdk-')); const path = join(dir, 'e.sock');
  let peer;
  const server = createServer((s) => { peer = s; });
  await new Promise((r) => server.listen(path, r));
  t.after(async () => { peer?.destroy(); await new Promise((r) => server.close(r)); await rm(dir, { recursive: true }); });
  const frames = [], errors = [];
  const transport = new UnixTransport(path, 64);
  await transport.open((f) => frames.push(f), (e) => errors.push(e));
  t.after(() => transport.close());
  const bytes = Buffer.from('{"text":"中文"}\n{"n":2}\n');
  peer.write(bytes.subarray(0, 10)); await tick(); peer.write(bytes.subarray(10));
  for (let i = 0; i < 20 && frames.length < 2; i++) await new Promise((r) => setTimeout(r, 5));
  assert.deepEqual(frames, [{ text: '中文' }, { n: 2 }]);
  peer.write('x'.repeat(65));
  for (let i = 0; i < 20 && errors.length === 0; i++) await new Promise((r) => setTimeout(r, 5));
  assert.match(errors[0].message, /too large/);
});
class FakeWs {
  listeners = new Map(); readyState = 1; bufferedAmount = 0; sent = [];
  addEventListener(type, cb) { this.listeners.set(type, [...(this.listeners.get(type) ?? []), cb]); }
  emit(type, event) { for (const cb of this.listeners.get(type) ?? []) cb(event); }
  message(frame) { this.emit('message', { data: JSON.stringify(frame) }); }
  send(text) { this.sent.push(JSON.parse(text)); this.onSend?.(JSON.parse(text)); }
  close() { this.readyState = 3; }
}
const keys = generateKeyPairSync('ed25519');
const serverKey = keys.publicKey.export({ format: 'der', type: 'spki' }).subarray(-32).toString('hex');
const credentials = { clientId: 'c', token: 'secret-token', machineId: 'm', serverKey };
test('WS proves pinned Ed25519 identity and HMAC audience before c9p', async () => {
  const ws = new FakeWs();
  ws.onSend = (f) => {
    if (!f.auth) return;
    const { clientNonce, mac } = f.auth;
    assert.equal(mac, createHmac('sha256', credentials.token).update('nonce' + clientNonce + 'm').digest('hex'));
    assert(!JSON.stringify(f).includes(credentials.token));
    ws.message({ sig: sign(null, Buffer.from('nonce' + clientNonce), keys.privateKey).toString('hex') });
  };
  const transport = new WsTransport('ws://localhost:9210/c9p', credentials, { webSocket: () => ws });
  assert.throws(() => transport.send({ method: 'hello' }), /not authenticated/);
  const frames = [], errors = [];
  const opened = transport.open((f) => frames.push(f), (e) => errors.push(e));
  ws.message({ challenge: 'nonce' }); await opened;
  transport.send({ v: 1, id: '1', method: 'hello' });
  ws.message({ v: 1, id: '1', ok: true, result: {} }); await tick();
  assert.equal(frames.length, 1); assert.equal(errors.length, 0); transport.close();
});
test('WS rejects impersonation, binary frames, and handshake timeout', async () => {
  const ws = new FakeWs(); ws.onSend = () => ws.message({ sig: '00'.repeat(64) });
  const transport = new WsTransport('ws://localhost/c9p', credentials, { webSocket: () => ws });
  const opened = transport.open(() => assert.fail('no c9p data'), () => {});
  ws.message({ challenge: 'nonce' });
  await assert.rejects(opened, /pinned server key/);
  assert.equal(ws.sent.length, 1);
  const ws2 = new FakeWs();
  const opened2 = new WsTransport('ws://localhost/c9p', credentials, { webSocket: () => ws2 }).open(() => {}, () => {});
  ws2.emit('message', { data: new Uint8Array([1]) }); await assert.rejects(opened2, /text frames/);
  await assert.rejects(new WsTransport('ws://localhost/c9p', credentials, { webSocket: () => new FakeWs(), timeoutMs: 5 }).open(() => {}, () => {}), /timed out/);
});
test('pairing sends only the code and validates returned credentials', async () => {
  const ws = new FakeWs(); ws.onSend = (f) => { assert.deepEqual(f, { pair: { code: 'ABC123' } }); ws.message(credentials); };
  const paired = pair('ws://localhost/c9p', 'ABC123', { webSocket: () => ws });
  ws.message({ challenge: 'nonce' }); assert.deepEqual(await paired, credentials);
  await assert.rejects(pair('ws://localhost/c9p?token=no', 'ABC'), /without credentials or query/);
});
