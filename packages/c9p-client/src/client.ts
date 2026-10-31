import { C9P_API_MAJOR, C9P_API_MINOR, C9P_PUBLIC_METHODS, C9P_PUBLIC_TOPICS } from './types.js';
import type { C9pHelloResult, C9pMethods, C9pMethod, C9pSubscribeParams, C9pEventFrame, C9pTopics, C9pTopic } from './types.js';
import { object, type Transport } from './transport.js';

export class C9pError extends Error {
  constructor(readonly code: string, message: string, readonly retryable = false, readonly hint?: string, readonly data?: unknown) {
    super(`${code}: ${message}`); this.name = 'C9pError';
  }
}
export type TopicEvent<T extends C9pTopic> = T extends C9pTopic
  ? Omit<C9pEventFrame, 'topic' | 'data'> & { topic: T; data: C9pTopics[T]['data'] } : never;
export interface Subscription {
  /** Last event whose callback completed successfully, or the initial subscribe cursor. */
  readonly cursor: string | undefined;
  unsubscribe(): Promise<void>;
}
interface LiveSubscription {
  id: string;
  params: C9pSubscribeParams;
  cursor?: string;
  active: boolean;
  queue: Promise<void>;
  queued: number;
  onEvent: (e: C9pEventFrame) => void | Promise<void>;
  onError: (e: Error) => void;
}
interface Pending {
  resolve: (value: unknown) => void;
  reject: (e: Error) => void;
  timer: ReturnType<typeof setTimeout>;
}

/** One multiplexed connection. Reconnect explicitly; mutations are never retried. */
export class C9pClient {
  hello?: C9pHelloResult;
  private transport?: Transport;
  private nextId = 0;
  private pending = new Map<string, Pending>();
  private subscriptions = new Set<LiveSubscription>();
  private connecting = false;
  private generation = 0;
  constructor(readonly factory: () => Transport, readonly options: {
    timeoutMs?: number;
    maxQueuedEvents?: number;
    onDisconnect?: (error: Error) => void;
  } = {}) {}

  async connect(): Promise<C9pHelloResult> {
    if (this.transport || this.connecting) throw new Error('Client already connected or connecting');
    this.connecting = true;
    const generation = ++this.generation;
    try {
      const transport = this.transport = this.factory();
      await transport.open(
        (v) => { if (generation === this.generation) this.receive(v); },
        (e) => { if (generation === this.generation) this.disconnected(e); }
      );
      const hello = await this.raw('hello', {
        apiMajor: C9P_API_MAJOR, apiMinor: C9P_API_MINOR,
        client: { kind: 'sdk', name: '@c9watch/c9p-client', version: '0.1.0' }, want: [],
      }) as C9pHelloResult;
      if (hello.apiMajor !== C9P_API_MAJOR) throw new C9pError('upgrade_required', 'API major mismatch');
      if (!hello.methods || !Array.isArray(hello.breakingSince)) throw new Error('Core lacks preview discovery; update core');
      if (generation !== this.generation || !this.transport) throw new Error('Disconnected during hello');
      this.hello = hello;
      return hello;
    } catch (e) { this.dropConnection(asError(e)); throw e; }
    finally { this.connecting = false; }
  }

  /** Recreate transport, repeat hello, resume active streams from completed cursors. */
  async reconnect(): Promise<C9pHelloResult> {
    this.dropConnection(new Error('Reconnecting'));
    const generation = this.generation;
    // Wait for in-flight application callbacks before choosing a resume cursor.
    await Promise.all([...this.subscriptions].map((s) => s.queue));
    if (generation !== this.generation) throw new Error('Reconnect cancelled');
    const hello = await this.connect();
    for (const s of this.subscriptions) {
      if (!s.active) continue;
      try { await this.attach(s); }
      catch (e) { s.active = false; s.onError(asError(e)); }
    }
    if (!this.transport || !this.hello) throw new Error('Reconnect cancelled');
    return hello;
  }

  request<M extends C9pMethod>(method: M, params: C9pMethods[M]['params']): Promise<C9pMethods[M]['result']> {
    try { this.checkMethod(method); }
    catch (e) { return Promise.reject(e); }
    return this.raw(method, params) as Promise<C9pMethods[M]['result']>;
  }

  async subscribe<T extends C9pTopic>(params: C9pSubscribeParams, onEvent: (event: TopicEvent<T>) => void | Promise<void>, onError: (error: Error) => void): Promise<Subscription> {
    this.checkMethod('events.subscribe');
    const matches = (topic: string, pattern: string) => pattern.endsWith('.*')
      ? topic.startsWith(pattern.slice(0, -1)) : topic === pattern;
    const patterns = params.topics?.length ? params.topics : C9P_PUBLIC_TOPICS;
    const topics = C9P_PUBLIC_TOPICS.filter((t) => this.hello!.topics.includes(t) && patterns.some((p) => matches(t, p)));
    for (const pattern of params.topics ?? []) {
      if (!topics.some((t) => matches(t, pattern))) throw new C9pError('not_allowed', `Topic ${pattern} is not public and advertised for this principal`);
    }
    if (!topics.length) throw new C9pError('not_allowed', 'No public topics advertised for this principal');
    const s: LiveSubscription = { id: '', params: { ...params, topics: [...topics] }, cursor: params.since ?? undefined,
      active: true, queue: Promise.resolve(), queued: 0,
      onEvent: onEvent as (e: C9pEventFrame) => void | Promise<void>, onError };
    this.subscriptions.add(s);
    try { await this.attach(s); }
    catch (e) { this.subscriptions.delete(s); throw e; }
    return {
      get cursor() { return s.cursor; },
      unsubscribe: async () => {
        s.active = false; this.subscriptions.delete(s);
        if (this.hello) await this.request('events.unsubscribe', { sub: s.id });
        await s.queue;
      },
    };
  }

  close(): void {
    for (const s of this.subscriptions) s.active = false;
    this.subscriptions.clear();
    this.dropConnection(new Error('Client closed'));
  }

  private checkMethod(method: string): void {
    if (!(C9P_PUBLIC_METHODS as readonly string[]).includes(method)) throw new C9pError('not_allowed', 'Method is not in this SDK public schema');
    if (!this.hello) throw new Error('Call connect() first');
    if (this.hello.breakingSince.includes(method)) throw new C9pError('breaking_preview', `${method} changed; update the SDK`);
    if (!Object.hasOwn(this.hello.methods, method)) throw new C9pError('unsupported', `${method} is not advertised by core`);
  }
  private async attach(s: LiveSubscription): Promise<void> {
    this.checkMethod('events.subscribe');
    s.id = String(++this.nextId);
    const result = await this.raw('events.subscribe', { ...s.params, ...(s.cursor === undefined ? {} : { since: s.cursor }) }, s.id) as { cursor: string };
    if (typeof result?.cursor !== 'string') {
      this.disconnected(new Error('Subscription response has no cursor'));
      throw new Error('Subscription response has no cursor');
    }
    if (!s.cursor) s.cursor = result.cursor;
  }
  private raw(method: string, params: unknown, id = String(++this.nextId)): Promise<unknown> {
    const transport = this.transport;
    if (!transport) return Promise.reject(new Error('Client disconnected'));
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id); reject(new C9pError('timeout', `${method} timed out`));
        // A timed-out subscription may still exist server-side; close to release it.
        if (method === 'events.subscribe') this.disconnected(new Error('Subscription timed out'));
      }, this.options.timeoutMs ?? 10000);
      this.pending.set(id, { resolve, reject, timer });
      try { transport.send({ v: C9P_API_MAJOR, id, method, params }); }
      catch (e) { clearTimeout(timer); this.pending.delete(id); reject(asError(e)); }
    });
  }
  private receive(value: unknown): void {
    try {
      const frame = object(value);
      if (frame.v !== C9P_API_MAJOR) throw new Error('Invalid c9p frame version');
      if (frame.type === 'event') {
        const s = [...this.subscriptions].find((s) => s.active && s.id === frame.sub);
        if (!s) return;
        if (!(C9P_PUBLIC_TOPICS as readonly unknown[]).includes(frame.topic)) return;
        if (typeof frame.cursor !== 'string' || typeof frame.topic !== 'string') throw new Error('Invalid event frame');
        if (++s.queued > (this.options.maxQueuedEvents ?? 1024)) {
          s.active = false; s.onError(new Error('Event consumer fell behind; resnapshot and subscribe again'));
          this.disconnected(new Error('Event queue full')); return;
        }
        s.queue = s.queue.then(async () => {
          if (!s.active) return;
          await s.onEvent(frame as unknown as C9pEventFrame);
          s.cursor = frame.cursor as string;
        }).catch((e: unknown) => {
          s.active = false;
          if (this.hello) void this.request('events.unsubscribe', { sub: s.id }).catch(() => {});
          s.onError(asError(e));
        })
          .finally(() => { s.queued--; });
        return;
      }
      if (frame.type === 'progress') return;
      if (typeof frame.id !== 'string' || typeof frame.ok !== 'boolean') throw new Error('Invalid response frame');
      const pending = this.pending.get(frame.id);
      if (pending) {
        clearTimeout(pending.timer); this.pending.delete(frame.id);
        if (frame.ok) pending.resolve(frame.result);
        else pending.reject(responseError(frame));
      } else if (!frame.ok) {
        const s = [...this.subscriptions].find((s) => s.active && s.id === frame.id);
        if (s) { s.active = false; s.onError(responseError(frame)); }
      }
    } catch (e) { this.disconnected(asError(e)); }
  }
  private dropConnection(error: Error): void {
    ++this.generation;
    const transport = this.transport; this.transport = undefined; this.hello = undefined;
    transport?.close();
    for (const p of this.pending.values()) { clearTimeout(p.timer); p.reject(error); }
    this.pending.clear();
  }
  private disconnected(error: Error): void {
    if (!this.transport) return;
    this.dropConnection(error);
    this.options.onDisconnect?.(error);
  }
}
function asError(e: unknown): Error { return e instanceof Error ? e : new Error(String(e)); }
function responseError(frame: Record<string, unknown>): C9pError {
  const e = object(frame.error);
  return new C9pError(String(e.code), String(e.msg), e.retryable === true, typeof e.hint === 'string' ? e.hint : undefined, e.data);
}
