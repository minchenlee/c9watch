/** c9p framing over an authenticated WebSocket. Auth is supplied by the
 * pairing layer: it must verify the pinned server key before resolving.
 * No c9p frame is ever sent before that promise resolves. */
import type { C9pMethod, C9pMethods, C9pSubscribeParams, C9pHelloResult } from '../c9p-1';
import { C9P_API_MAJOR, C9P_API_MINOR } from '../c9p-1';
import { C9pError, type StreamFrame, type Transport, type Unsubscribe } from './transport';
type Authenticator = (socket: WebSocket) => Promise<void>;
interface Subscription { params: C9pSubscribeParams; onFrame: (frame: StreamFrame) => void; wireId?: string; closed: boolean }
export class C9pWebSocketTransport implements Transport {
    private socket: WebSocket | null = null;
    private connecting: Promise<void> | null = null;
    private nextId = 0;
    private stopped = false;
    private reconnect: ReturnType<typeof setTimeout> | null = null;
    private pending = new Map<string, { resolve: (v: any) => void; reject: (e: Error) => void; timer: ReturnType<typeof setTimeout> }>();
    private subscriptions = new Set<Subscription>();
    hello: C9pHelloResult | null = null;
    constructor(private url: string, private authenticate: Authenticator) {}
    async connect(): Promise<void> {
        if (this.connecting) return this.connecting;
        if (this.socket?.readyState === WebSocket.OPEN && this.hello) return;
        this.stopped = false;
        this.connecting = this.open().finally(() => { this.connecting = null; });
        return this.connecting;
    }
    private async open() {
        const ws = new WebSocket(this.url);
        this.socket = ws;
        try {
            await new Promise<void>((resolve, reject) => {
                ws.onopen = () => resolve();
                ws.onerror = () => reject(new Error('WebSocket connection failed'));
                ws.onclose = () => reject(new Error('WebSocket closed before authentication'));
            });
            let authTimer: ReturnType<typeof setTimeout>;
            try {
                await Promise.race([
                    this.authenticate(ws),
                    new Promise<never>((_, reject) => { authTimer = setTimeout(() => reject(new Error('Gateway authentication timed out')), 5000); })
                ]);
            } finally { clearTimeout(authTimer!); }
            if (this.stopped || this.socket !== ws || ws.readyState !== WebSocket.OPEN) throw new Error('Connection closed');
            ws.onmessage = event => this.receive(event);
            ws.onclose = () => this.disconnected();
            this.hello = await this.send('hello', { apiMajor: C9P_API_MAJOR, apiMinor: C9P_API_MINOR, client: { kind: 'web', name: 'c9watch' }, want: ['conversation.progress'] });
            if (this.hello?.apiMajor !== C9P_API_MAJOR) throw new C9pError('upgrade_required', 'Incompatible core protocol');
            for (const sub of this.subscriptions) if (!sub.closed) await this.startSubscription(sub);
        } catch (e) {
            ws.close();
            if (this.socket === ws) this.disconnected();
            throw e;
        }
    }
    async request<M extends C9pMethod>(method: M, params: C9pMethods[M]['params']): Promise<C9pMethods[M]['result']> {
        await this.connect();
        if (this.hello?.breakingSince.includes(method)) throw new C9pError('upgrade_required', `${method} changed in this core`);
        if (!this.hello?.methods[method]) throw new C9pError('unknown_method', `${method} is unavailable`);
        return this.send(method, params);
    }
    private send(method: string, params: unknown, id = String(++this.nextId)): Promise<any> {
        return new Promise((resolve, reject) => {
            const ws = this.socket;
            if (!ws || ws.readyState !== WebSocket.OPEN) { reject(new Error('WebSocket disconnected')); return; }
            const timer = setTimeout(() => { this.pending.delete(id); reject(new Error(`${method} timed out`)); }, 120_000);
            this.pending.set(id, { resolve, reject, timer });
            try {
                const text = JSON.stringify({ v: C9P_API_MAJOR, id, method, params });
                if (new TextEncoder().encode(text).byteLength > 64 * 1024) throw new C9pError('frame_too_large', 'Gateway request exceeds 64 KiB');
                ws.send(text);
            }
            catch (e) { clearTimeout(timer); this.pending.delete(id); reject(e); }
        });
    }
    async subscribe(params: C9pSubscribeParams, onFrame: (frame: StreamFrame) => void): Promise<Unsubscribe> {
        await this.connect();
        const sub: Subscription = { params: { ...params }, onFrame, closed: false };
        this.subscriptions.add(sub);
        try { await this.startSubscription(sub); }
        catch (e) { this.subscriptions.delete(sub); throw e; }
        return () => {
            sub.closed = true;
            this.subscriptions.delete(sub);
            if (sub.wireId && this.hello) void this.send('events.unsubscribe', { sub: sub.wireId }).catch(() => {});
        };
    }
    private async startSubscription(sub: Subscription) {
        const id = String(++this.nextId);
        sub.wireId = id;
        try {
            const result = await this.send('events.subscribe', sub.params, id);
            // A live-only notification subscription stays live-only on reconnect.
            if (sub.params.since != null) sub.params.since = result.cursor;
        } catch (e) {
            if (e instanceof C9pError && e.code === 'cursor_expired') {
                sub.closed = true;
                this.subscriptions.delete(sub);
                sub.onFrame({ v: 1, ok: false, error: { code: 'cursor_expired', msg: e.message, retryable: false, hint: 'Fetch session.views and replace the store before subscribing with its cursor' } });
            } else throw e;
        }
    }
    private receive(event: MessageEvent) {
        if (typeof event.data !== 'string' || event.data.length > (1 << 20)) { this.socket?.close(1009); return; }
        let frame: any;
        try { frame = JSON.parse(event.data); } catch { this.socket?.close(1002); return; }
        if (frame.type === 'event') {
            for (const sub of this.subscriptions) if (!sub.closed && sub.wireId === frame.sub) {
                if (sub.params.since != null) sub.params.since = frame.cursor;
                sub.onFrame(frame);
            }
            return;
        }
        if (frame.type === 'progress') {
            for (const sub of this.subscriptions) if (!sub.closed) sub.onFrame(frame);
            return;
        }
        const p = this.pending.get(frame.id);
        if (p) {
            this.pending.delete(frame.id); clearTimeout(p.timer);
            if (frame.ok) p.resolve(frame.result);
            else p.reject(Object.assign(new C9pError(frame.error?.code ?? 'bad_response', frame.error?.msg ?? 'Core request failed', frame.error?.retryable), frame.error));
        } else if (frame.error?.code === 'cursor_expired') {
            for (const sub of this.subscriptions) if (sub.wireId === frame.id) {
                sub.closed = true; this.subscriptions.delete(sub); sub.onFrame(frame);
            }
        }
    }
    private disconnected() {
        this.hello = null;
        for (const p of this.pending.values()) { clearTimeout(p.timer); p.reject(new Error('WebSocket disconnected')); }
        this.pending.clear();
        if (!this.stopped && !this.reconnect && this.subscriptions.size) this.reconnect = setTimeout(() => {
            this.reconnect = null;
            void this.connect().catch(() => {});
        }, 1000);
    }
    close() {
        this.stopped = true;
        if (this.reconnect) clearTimeout(this.reconnect);
        this.reconnect = null;
        this.subscriptions.clear();
        this.socket?.close(); this.disconnected(); this.socket = null;
    }
}

export { hexGatewayAuthenticator, type GatewayPairing } from './ws-c9p-auth';
