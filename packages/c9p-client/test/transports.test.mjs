import test from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:net';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createPrivateKey, createPublicKey, generateKeyPairSync, sign, verify, webcrypto } from 'node:crypto';
import { UnixTransport } from '../dist/transport-unix.js';
import { WsTransport, pair, gatewayMessage } from '../dist/transport-ws.js';

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
// Same seeds and bytes as src-tauri/src/gateway/pubkey.rs cross_language_vectors.
const seeded = (seed) => createPrivateKey({ key: Buffer.concat([Buffer.from('302e020100300506032b657004220420', 'hex'), Buffer.alloc(32, seed)]), format: 'der', type: 'pkcs8' });
const rawPub = (key) => createPublicKey(key).export({ format: 'der', type: 'spki' }).subarray(-32);
const machine = seeded(7);
const serverKey = rawPub(machine).toString('hex');
const clientId = 'cli_01jbz3k6x8q2m9v4r7t5w0y1zc';
const importSeed = (seed) => webcrypto.subtle.importKey('pkcs8', seeded(seed).export({ format: 'der', type: 'pkcs8' }), 'Ed25519', false, ['sign']);
const credentials = async () => ({ clientId, machineId: serverKey, serverKey, privateKey: await importSeed(9) });
const msg = (label, s, c, ...f) => Buffer.from(gatewayMessage(label, Buffer.from(s, 'hex'), Buffer.from(c, 'hex'), ...f));
const S = 'b2'.repeat(32);

test('message bytes and signatures match the Rust vectors', async () => {
  const m = msg('c9p-gateway auth-client v1', S, 'c3'.repeat(32), serverKey, clientId);
  assert.equal(m.toString('hex'), Buffer.from('c9p-gateway auth-client v1').toString('hex') + '00' + S + 'c3'.repeat(32) + '40' + Buffer.from(serverKey).toString('hex') + '1e' + Buffer.from(clientId).toString('hex'));
  assert.equal(serverKey, 'ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c');
  assert.equal(rawPub(seeded(9)).toString('hex'), 'fd1724385aa0c75b64fb78cd602fa1d991fdebf76b13c58ed702eac835e9f618');
  assert.equal(sign(null, m, seeded(9)).toString('hex'), 'c8434983a6da58624add2bdc334f4f3f1f306c5a4177da50e8d1df4352e033d587bb5d3a26027cc0690fe4bcdf66276bc41a9c0474111fd6f7f168424a73e901');
  assert.equal(sign(null, msg('c9p-gateway auth-server v1', S, 'c3'.repeat(32), serverKey, clientId), machine).toString('hex'), 'd7558ccadd82cef22380a242aaa3f97b882f9deff62fbc47d0ff75eafa5fe321fc83dd5fd6b348980d39afb32447f104426e5f2926542f002a30cae58ecc5b0d');
});
test('WS signs with the client key and verifies the pinned server key before c9p', async () => {
  const ws = new FakeWs();
  const creds = await credentials();
  ws.onSend = (f) => {
    if (!f.auth) return;
    assert.deepEqual(Object.keys(f.auth).sort(), ['clientId', 'clientNonce', 'sig']);
    const { clientNonce, sig } = f.auth;
    assert.match(clientNonce, /^[0-9a-f]{64}$/);
    assert(verify(null, msg('c9p-gateway auth-client v1', S, clientNonce, serverKey, clientId), rawPubKey(seeded(9)), Buffer.from(sig, 'hex')));
    ws.message({ sig: sign(null, msg('c9p-gateway auth-server v1', S, clientNonce, serverKey, clientId), machine).toString('hex') });
  };
  const transport = new WsTransport('ws://localhost:9210/c9p', creds, { webSocket: () => ws });
  assert.throws(() => transport.send({ method: 'hello' }), /not authenticated/);
  const frames = [], errors = [];
  const opened = transport.open((f) => frames.push(f), (e) => errors.push(e));
  ws.message({ challenge: S }); await opened;
  transport.send({ v: 1, id: '1', method: 'hello' });
  ws.message({ v: 1, id: '1', ok: true, result: {} }); await tick();
  assert.equal(frames.length, 1); assert.equal(errors.length, 0); transport.close();
});
const rawPubKey = (key) => createPublicKey(key);
test('WS rejects impersonation, malformed nonces, binary frames, and handshake timeout', async () => {
  const creds = await credentials();
  const ws = new FakeWs(); ws.onSend = () => ws.message({ sig: '00'.repeat(64) });
  const transport = new WsTransport('ws://localhost/c9p', creds, { webSocket: () => ws });
  const opened = transport.open(() => assert.fail('no c9p data'), () => {});
  ws.message({ challenge: S });
  await assert.rejects(opened, /pinned server key/);
  assert.equal(ws.sent.length, 1);
  // A server answering with a proof for another step is refused.
  const ws1 = new FakeWs();
  ws1.onSend = (f) => ws1.message({ sig: sign(null, msg('c9p-gateway auth-client v1', S, f.auth.clientNonce, serverKey, clientId), machine).toString('hex') });
  const reflected = new WsTransport('ws://localhost/c9p', creds, { webSocket: () => ws1 }).open(() => {}, () => {});
  ws1.message({ challenge: S }); await assert.rejects(reflected, /pinned server key/);
  for (const challenge of ['nonce', S.toUpperCase(), S.slice(2), S + '00', 7]) {
    const bad = new FakeWs();
    const p = new WsTransport('ws://localhost/c9p', creds, { webSocket: () => bad }).open(() => {}, () => {});
    bad.message({ challenge }); await assert.rejects(p, /Invalid gateway challenge/);
    assert.equal(bad.sent.length, 0);
  }
  const ws2 = new FakeWs();
  const opened2 = new WsTransport('ws://localhost/c9p', creds, { webSocket: () => ws2 }).open(() => {}, () => {});
  ws2.emit('message', { data: new Uint8Array([1]) }); await assert.rejects(opened2, /text frames/);
  await assert.rejects(new WsTransport('ws://localhost/c9p', creds, { webSocket: () => new FakeWs(), timeoutMs: 5 }).open(() => {}, () => {}), /timed out/);
});
test('pairing registers a fresh public key with proof of possession, never a secret', async () => {
  const ws = new FakeWs();
  let registered;
  ws.onSend = (f) => {
    assert.deepEqual(Object.keys(f.pair).sort(), ['clientNonce', 'code', 'publicKey', 'sig']);
    assert.equal(f.pair.code, 'ABC123');
    const pub = createPublicKey({ key: Buffer.concat([Buffer.from('302a300506032b6570032100', 'hex'), Buffer.from(f.pair.publicKey, 'hex')]), format: 'der', type: 'spki' });
    assert(verify(null, msg('c9p-gateway pair v1', S, f.pair.clientNonce, Buffer.from(f.pair.publicKey, 'hex')), pub, Buffer.from(f.pair.sig, 'hex')));
    registered = f.pair;
    const sig = sign(null, msg('c9p-gateway paired v1', S, f.pair.clientNonce, Buffer.from(f.pair.publicKey, 'hex'), serverKey, clientId), machine).toString('hex');
    ws.message({ paired: { clientId, serverKey, machineId: serverKey, sig } });
  };
  const paired = pair('ws://localhost/c9p', 'ABC123', { webSocket: () => ws });
  ws.message({ challenge: S });
  const creds = await paired;
  assert.deepEqual({ ...creds, privateKey: undefined }, { clientId, serverKey, machineId: serverKey, privateKey: undefined });
  assert.equal(creds.privateKey.extractable, false);
  assert.equal(creds.privateKey.type, 'private');
  assert(!JSON.stringify(registered).includes('token'));
  await assert.rejects(pair('ws://localhost/c9p?token=no', 'ABC'), /without credentials or query/);
  // A reply signed by a different key than the one it returns is refused.
  const forged = new FakeWs();
  forged.onSend = () => forged.message({ paired: { clientId, serverKey, machineId: serverKey, sig: '00'.repeat(64) } });
  const p = pair('ws://localhost/c9p', 'ABC123', { webSocket: () => forged });
  forged.message({ challenge: S }); await assert.rejects(p, /not signed/);
  // A bearer-style reply is not accepted.
  const old = new FakeWs();
  old.onSend = () => old.message({ clientId, token: 'secret', serverKey, machineId: serverKey });
  const q = pair('ws://localhost/c9p', 'ABC123', { webSocket: () => old });
  old.message({ challenge: S }); await assert.rejects(q, /rejected or incomplete/);
});
