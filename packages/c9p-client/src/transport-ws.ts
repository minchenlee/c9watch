import { object, type Transport } from './transport.js';

export interface Credentials {
  clientId: string;
  token: string;
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
function unhex(s: string): Uint8Array {
  if (!/^(?:[0-9a-fA-F]{2})+$/.test(s)) throw new Error('Invalid hex authentication field');
  return Uint8Array.from(s.match(/../g)!, (x) => parseInt(x, 16));
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

/** Gateway /c9p. No c9p frame is sent until the pinned server proves its key.
 * Wire encoding: UTF-8 token/nonces/machineId, hex MAC/key/signature. Needs M-b.
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
      let ended = false;
      let phase: 'challenge' | 'signature' | 'ready' = 'challenge';
      let serverNonce = '';
      const clientNonce = hex(api.getRandomValues(new Uint8Array(32)));
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
            if (typeof frame.challenge !== 'string' || !frame.challenge || frame.challenge.length > 1024) throw new Error('Missing gateway challenge');
            serverNonce = frame.challenge;
            const key = await api.subtle.importKey('raw', utf8(this.credentials.token), { name: 'HMAC', hash: 'SHA-256' }, false, ['sign']);
            const mac = hex(new Uint8Array(await api.subtle.sign('HMAC', key, utf8(serverNonce + clientNonce + this.credentials.machineId))));
            if (ended) return;
            phase = 'signature';
            ws.send(JSON.stringify({ auth: { clientId: this.credentials.clientId, clientNonce, mac } }));
          } else if (phase === 'signature') {
            if (typeof frame.sig !== 'string') throw new Error('Missing gateway signature');
            const keyBytes = unhex(this.credentials.serverKey);
            if (keyBytes.length !== 32) throw new Error('Invalid pinned server key');
            const key = await api.subtle.importKey('raw', keyBytes, 'Ed25519', false, ['verify']);
            const verified = await api.subtle.verify('Ed25519', key, unhex(frame.sig), utf8(serverNonce + clientNonce));
            if (!verified) throw new Error('Gateway identity does not match the pinned server key');
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
 * Pairing is trust on first use: verify the returned server key out of band.
 * Codes and tokens must never be placed in URLs, logs or source files.
 */
export function pair(url: string, code: string, options: WsOptions = {}): Promise<Credentials> {
  if (!code.trim()) return Promise.reject(new Error('Pairing code required'));
  return new Promise((resolve, reject) => {
    const ws = socket(url, options);
    let ended = false;
    let sent = false;
    const finish = (error?: Error, result?: Credentials) => {
      if (ended) return;
      ended = true; clearTimeout(timer); ws.close();
      if (error) reject(error); else resolve(result!);
    };
    const timer = setTimeout(() => finish(new Error('Pairing timed out')), options.timeoutMs ?? 5000);
    ws.addEventListener('message', (event: { data: unknown }) => {
      try {
        if (typeof event.data !== 'string' || utf8(event.data).byteLength > 4096) throw new Error('Invalid pairing frame');
        const frame = object(JSON.parse(event.data));
        if (!sent) {
          if (typeof frame.challenge !== 'string') throw new Error('Missing pairing challenge');
          sent = true; ws.send(JSON.stringify({ pair: { code: code.trim() } })); return;
        }
        for (const key of ['clientId', 'token', 'serverKey', 'machineId']) {
          if (typeof frame[key] !== 'string' || !frame[key]) throw new Error('Pairing rejected or incomplete');
        }
        if (unhex(frame.serverKey as string).length !== 32) throw new Error('Invalid server key');
        finish(undefined, frame as unknown as Credentials);
      } catch (e) { finish(e instanceof Error ? e : new Error(String(e))); }
    });
    ws.addEventListener('error', () => finish(new Error('Pairing connection failed')));
    ws.addEventListener('close', (event: { code?: number }) => finish(new Error(`Pairing closed (${event.code ?? 'unknown'})`)));
  });
}
