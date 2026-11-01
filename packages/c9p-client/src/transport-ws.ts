import { object, type Transport } from './transport.js';

/** A paired gateway client. `privateKey` is this client's own Ed25519 key;
 * `pair()` creates it non-extractable. Keep the object (for example in
 * IndexedDB); core stores only the public half. */
export interface Credentials {
  clientId: string;
  privateKey: CryptoKey;
  /** Pinned raw Ed25519 public key (hex). */
  serverKey: string;
  machineId: string;
}
export interface WebSocketLike {
  readyState: number;
  bufferedAmount: number;
  addEventListener(type: string, listener: (event: any) => void): void;
  send(data: string): void;
  close(code?: number, reason?: string): void;
}
export type WebSocketFactory = (url: string) => WebSocketLike;
export interface WsOptions {
  webSocket?: WebSocketFactory;
  crypto?: Crypto;
  timeoutMs?: number;
  maxFrame?: number;
}
const utf8 = (s: string) => new TextEncoder().encode(s);
const hex = (bytes: Uint8Array) => Array.from(bytes, (x) => x.toString(16).padStart(2, '0')).join('');
/** Exactly `length` bytes of lowercase hex; anything else is refused. */
function unhex(s: unknown, length: number, what: string): Uint8Array {
  if (typeof s !== 'string' || s.length !== length * 2 || !/^[0-9a-f]*$/.test(s)) throw new Error(`Invalid gateway ${what}`);
  return Uint8Array.from(s.match(/../g)!, (x) => parseInt(x, 16));
}
const CLIENT_ID = /^cli_[0-9a-hjkmnp-tv-z]{26}$/;
/** `label || 0x00 || serverNonce || clientNonce || (len8 || field)*`; see gateway.md. */
export function gatewayMessage(label: string, serverNonce: Uint8Array, clientNonce: Uint8Array, ...fields: (Uint8Array | string)[]): Uint8Array {
  const parts = [utf8(label), new Uint8Array([0]), serverNonce, clientNonce];
  for (const f of fields) {
    const bytes = typeof f === 'string' ? utf8(f) : f;
    if (bytes.byteLength > 255) throw new Error('Gateway field too long');
    parts.push(new Uint8Array([bytes.byteLength]), bytes);
  }
  const out = new Uint8Array(parts.reduce((n, p) => n + p.byteLength, 0));
  let at = 0;
  for (const p of parts) { out.set(p, at); at += p.byteLength; }
  return out;
}
function cryptoApi(options: WsOptions): Crypto {
  const api = options.crypto ?? globalThis.crypto;
  if (!api?.subtle) throw new Error('WebCrypto required; serve the example on HTTPS or localhost');
  return api;
}
function socket(url: string, options: WsOptions): WebSocketLike {
  const parsed = new URL(url);
  if (!['ws:', 'wss:'].includes(parsed.protocol) || parsed.pathname !== '/c9p' || parsed.username || parsed.password || parsed.search || parsed.hash) {
    throw new Error('Use a ws(s)://host:port/c9p URL without credentials or query');
  }
  return options.webSocket ? options.webSocket(url) : new WebSocket(url);
}

async function verifyServer(api: Crypto, serverKey: string, sig: unknown, message: Uint8Array): Promise<boolean> {
  const key = await api.subtle.importKey('raw', unhex(serverKey, 32, 'server key'), 'Ed25519', false, ['verify']);
  return api.subtle.verify('Ed25519', key, unhex(sig, 64, 'signature'), message);
}
async function signClient(api: Crypto, key: CryptoKey, message: Uint8Array): Promise<string> {
  return hex(new Uint8Array(await api.subtle.sign('Ed25519', key, message)));
}

/** Gateway /c9p. No c9p frame is sent until the pinned server proves its key.
 * The client proves its own key; no shared secret crosses the wire or is stored
 * by core. Wire encoding: lowercase hex nonces, keys and signatures. Needs M-b.
 */
export class WsTransport implements Transport {
  private ws?: WebSocketLike;
  private authenticated = false;
  constructor(readonly url: string, readonly credentials: Credentials, readonly options: WsOptions = {}) {}
  open(onFrame: (v: unknown) => void, onClose: (e: Error) => void): Promise<void> {
    if (this.ws) return Promise.reject(new Error('Transport already open'));
    return new Promise((resolve, reject) => {
      const api = cryptoApi(this.options);
      const ws = this.ws = socket(this.url, this.options);
      const { clientId, machineId, serverKey, privateKey } = this.credentials;
      let ended = false;
      let phase: 'challenge' | 'signature' | 'ready' = 'challenge';
      let serverNonce = new Uint8Array();
      const clientNonce = api.getRandomValues(new Uint8Array(32));
      const timer = setTimeout(() => fail(new Error('Gateway authentication timed out')), this.options.timeoutMs ?? 5000);
      const fail = (e: Error) => {
        if (ended) return;
        ended = true; clearTimeout(timer); this.authenticated = false;
        ws.close(); reject(e); onClose(e);
      };
      let queue = Promise.resolve();
      ws.addEventListener('message', (event: { data: unknown }) => {
        queue = queue.then(async () => {
          if (ended) return;
          if (typeof event.data !== 'string') throw new Error('Gateway requires text frames');
          const size = utf8(event.data).byteLength;
          if (size > (phase === 'ready' ? (this.options.maxFrame ?? 1 << 20) : 4096)) throw new Error('Gateway frame too large');
          const frame = object(JSON.parse(event.data));
          if (phase === 'challenge') {
            serverNonce = unhex(frame.challenge, 32, 'challenge');
            if (!CLIENT_ID.test(clientId)) throw new Error('Invalid gateway client id');
            unhex(machineId, 32, 'machine id');
            const sig = await signClient(api, privateKey, gatewayMessage('c9p-gateway auth-client v1', serverNonce, clientNonce, machineId, clientId));
            if (ended) return;
            phase = 'signature';
            ws.send(JSON.stringify({ auth: { clientId, clientNonce: hex(clientNonce), sig } }));
          } else if (phase === 'signature') {
            const message = gatewayMessage('c9p-gateway auth-server v1', serverNonce, clientNonce, machineId, clientId);
            if (!await verifyServer(api, serverKey, frame.sig, message)) throw new Error('Gateway identity does not match the pinned server key');
            if (ended) return;
            phase = 'ready'; this.authenticated = true; clearTimeout(timer); resolve();
          } else onFrame(frame);
        }).catch((e: unknown) => fail(e instanceof Error ? e : new Error(String(e))));
      });
      ws.addEventListener('error', () => fail(new Error('Gateway connection failed')));
      ws.addEventListener('close', (event: { code?: number }) => fail(new Error(`Gateway closed (${event.code ?? 'unknown'})`)));
    });
  }
  send(frame: unknown): void {
    if (!this.authenticated || !this.ws) throw new Error('Gateway is not authenticated');
    const text = JSON.stringify(frame);
    if (utf8(text).byteLength > 65536) throw new Error('Gateway request exceeds 64 KiB');
    if (this.ws.bufferedAmount > 262144) throw new Error('Gateway write queue full');
    this.ws.send(text);
  }
  close(): void { this.authenticated = false; this.ws?.close(); }
}

/** Redeem a code on /c9p, then reconnect with WsTransport. Needs M-b.
 * Makes a fresh non-extractable Ed25519 key and registers its public half.
 * Pairing is trust on first use: verify the returned server key out of band.
 * Codes must never be placed in URLs, logs or source files.
 */
export function pair(url: string, code: string, options: WsOptions = {}): Promise<Credentials> {
  if (!code.trim()) return Promise.reject(new Error('Pairing code required'));
  return new Promise((resolve, reject) => {
    let api: Crypto;
    let ws: WebSocketLike;
    try { api = cryptoApi(options); ws = socket(url, options); } catch (e) { reject(e); return; }
    let ended = false;
    let sent = false;
    const finish = (error?: Error, result?: Credentials) => {
      if (ended) return;
      ended = true; clearTimeout(timer); ws.close();
      if (error) reject(error); else resolve(result!);
    };
    const timer = setTimeout(() => finish(new Error('Pairing timed out')), options.timeoutMs ?? 5000);
    const clientNonce = api.getRandomValues(new Uint8Array(32));
    let serverNonce = new Uint8Array();
    let keys: CryptoKeyPair | undefined;
    let publicKey = new Uint8Array();
    let queue = Promise.resolve();
    ws.addEventListener('message', (event: { data: unknown }) => {
      queue = queue.then(async () => {
        if (ended) return;
        if (typeof event.data !== 'string' || utf8(event.data).byteLength > 4096) throw new Error('Invalid pairing frame');
        const frame = object(JSON.parse(event.data));
        if (!sent) {
          serverNonce = unhex(frame.challenge, 32, 'challenge');
          keys = await api.subtle.generateKey('Ed25519', false, ['sign', 'verify']) as CryptoKeyPair;
          publicKey = new Uint8Array(await api.subtle.exportKey('raw', keys.publicKey));
          const sig = await signClient(api, keys.privateKey, gatewayMessage('c9p-gateway pair v1', serverNonce, clientNonce, publicKey));
          if (ended) return;
          sent = true;
          ws.send(JSON.stringify({ pair: { code: code.trim(), publicKey: hex(publicKey), clientNonce: hex(clientNonce), sig } }));
          return;
        }
        const paired = frame.paired === undefined ? undefined : object(frame.paired);
        if (!paired) throw new Error('Pairing rejected or incomplete');
        const { clientId, serverKey, machineId, sig } = paired;
        if (typeof clientId !== 'string' || !CLIENT_ID.test(clientId)) throw new Error('Invalid gateway client id');
        unhex(machineId, 32, 'machine id');
        const message = gatewayMessage('c9p-gateway paired v1', serverNonce, clientNonce, publicKey, machineId as string, clientId);
        if (!await verifyServer(api, serverKey as string, sig, message)) throw new Error('Pairing reply is not signed by the returned server key');
        finish(undefined, { clientId, privateKey: keys!.privateKey, serverKey: serverKey as string, machineId: machineId as string });
      }).catch((e: unknown) => finish(e instanceof Error ? e : new Error(String(e))));
    });
    ws.addEventListener('error', () => finish(new Error('Pairing connection failed')));
    ws.addEventListener('close', (event: { code?: number }) => finish(new Error(`Pairing closed (${event.code ?? 'unknown'})`)));
  });
}
