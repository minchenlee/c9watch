/**
 * WebSocket client for mobile/browser → desktop communication
 */

type EventCallback = (data: any) => void;

/** How a client proves itself in the first frame: a stored token, or a one-time pairing code. */
export type WsAuth = { token: string } | { code: string };

/** Close code: the first frame was missing, late, or malformed. */
export const CLOSE_BAD_HANDSHAKE = 4400;
/** Close code: token rejected. Pair again; do not retry. */
export const CLOSE_UNAUTHORIZED = 4401;
/** Close code: pairing code wrong, expired or used. A stored token is unaffected. */
export const CLOSE_PAIR_REJECTED = 4403;

const CONNECT_TIMEOUT_MS = 5000;

/** What the desktop lets a remote client do beyond watching cards. Both default off. */
export interface RemoteCapabilities {
	conversation: boolean;
	open: boolean;
}
export type RemoteCapability = keyof RemoteCapabilities;

/** The request needs a capability the desktop has turned off for remote devices. */
export class CapabilityDisabledError extends Error {
	readonly capability: RemoteCapability;
	constructor(capability: RemoteCapability, message?: string) {
		super(message ?? `${capability} is off for remote devices`);
		this.name = 'CapabilityDisabledError';
		this.capability = capability;
	}
}

/** Requests the server refuses unless the matching capability is on. */
const REQUIRED_CAPABILITY: Record<string, RemoteCapability> = {
	getConversation: 'conversation',
	openSession: 'open',
};

class WsClient {
	private ws: WebSocket | null = null;
	private host = '';
	private auth: WsAuth | null = null;
	private _connected = false;
	private nextRequestId = 0;
	private pending = new Map<number, { resolve: (value: any) => void; reject: (reason: any) => void }>();
	private listeners = new Map<string, Set<EventCallback>>();
	private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
	private capabilities: RemoteCapabilities | null = null;

	get isConnected() {
		return this._connected;
	}

	/**
	 * Whether the desktop allows `capability` for this client, from the last
	 * `authOk`/`paired`. Before an ack (or from a server that sends none) this
	 * is true: the server is the real gate and refuses anyway.
	 */
	allows(capability: RemoteCapability): boolean {
		return this.capabilities?.[capability] !== false;
	}

	/**
	 * Open the socket to `host` ("host:port"), send the auth frame first, and
	 * resolve only when the server accepts it. The token never goes in the URL.
	 */
	async connect(host: string, auth: WsAuth): Promise<void> {
		// One socket at a time: a pending reconnect with an older auth must
		// not replace this one, and the socket being replaced must not stay open.
		if (this.reconnectTimer) {
			clearTimeout(this.reconnectTimer);
			this.reconnectTimer = null;
		}
		if (this.ws) {
			const previous = this.ws;
			this.ws = null;
			this._connected = false;
			this.rejectPending('Connection closed');
			previous.close();
		}
		this.host = host;
		this.auth = auth;
		return new Promise((resolve, reject) => {
			let settled = false;
			const settle = (error?: Error) => {
				if (settled) return;
				settled = true;
				clearTimeout(timeout);
				if (error) reject(error);
				else resolve();
			};
			let socket: WebSocket;
			try {
				socket = new WebSocket(wsUrlFor(host));
			} catch (e) {
				reject(e);
				return;
			}
			this.ws = socket;

			// Covers the open and the server's ack.
			const timeout = setTimeout(() => {
				settle(new Error('Connection timeout'));
				socket.close();
			}, CONNECT_TIMEOUT_MS);

			socket.onopen = () => {
				const frame = 'token' in auth
					? { type: 'auth', token: auth.token }
					: { type: 'pair', code: auth.code };
				socket.send(JSON.stringify(frame));
			};

			socket.onerror = () => {
				if (!this._connected) settle(new Error('Connection failed'));
			};

			socket.onclose = (event: CloseEvent) => {
				const unauthorized = event?.code === CLOSE_UNAUTHORIZED;
				const pairRejected = event?.code === CLOSE_PAIR_REJECTED;
				settle(new Error(
					unauthorized ? 'Not paired' : pairRejected ? 'Pairing code rejected' : 'Connection closed'
				));
				if (this.ws !== socket) return; // replaced or disconnected
				this._connected = false;
				this.rejectPending('Connection closed');
				if (pairRejected) {
					// Only the code failed; a stored token is still good.
					this.auth = null;
					return;
				}
				if (unauthorized) {
					// Only the token this socket sent is void. A newer one stored
					// meanwhile (another tab, a fresh pairing) stays.
					const rejected = this.auth && 'token' in this.auth ? this.auth.token : null;
					this.auth = null;
					if (rejected !== null) clearStoredToken(rejected);
					this.emit('authRequired', null);
					return;
				}
				// Reconnects only with a token: a pairing code is single-use
				// and every failed try counts against it.
				this.scheduleReconnect();
			};

			socket.onmessage = (event) => {
				if (this._connected) {
					this.handleMessage(event);
					return;
				}
				let msg: any;
				try {
					msg = JSON.parse(event.data);
				} catch {
					return;
				}
				if (msg.type === 'paired' && typeof msg.token === 'string') {
					this.auth = { token: msg.token };
					setStoredConnection(host, msg.token);
				} else if (msg.type !== 'authOk') {
					return;
				}
				this.capabilities = msg.capabilities ?? null;
				this._connected = true;
				console.log('[ws] Connected');
				settle();
				this.emit('capabilities', this.capabilities);
			};
		});
	}

	disconnect() {
		if (this.reconnectTimer) {
			clearTimeout(this.reconnectTimer);
			this.reconnectTimer = null;
		}
		this.ws?.close();
		this.ws = null;
		this._connected = false;
	}

	async request<T = any>(type: string, data?: Record<string, any>): Promise<T> {
		if (!this.ws || this.ws.readyState !== WebSocket.OPEN) {
			throw new Error('WebSocket not connected');
		}
		const capability = REQUIRED_CAPABILITY[type];
		if (capability && !this.allows(capability)) {
			throw new CapabilityDisabledError(capability);
		}
		return new Promise((resolve, reject) => {
			const requestId = ++this.nextRequestId;
			this.pending.set(requestId, { resolve, reject });
			try {
				this.ws!.send(JSON.stringify({ type, ...data, requestId }));
			} catch (error) {
				this.pending.delete(requestId);
				reject(error);
			}
		});
	}

	on(event: string, callback: EventCallback) {
		if (!this.listeners.has(event)) {
			this.listeners.set(event, new Set());
		}
		this.listeners.get(event)!.add(callback);
	}

	off(event: string, callback: EventCallback) {
		this.listeners.get(event)?.delete(callback);
	}

	private handleMessage(event: MessageEvent) {
		try {
			const msg = JSON.parse(event.data);

			// Server push events → forward to event listeners
			if (msg.type === 'sessionsUpdated') {
				this.emit('sessionsUpdated', msg.data);
				return;
			}
			if (msg.type === 'notification') {
				this.emit('notification', msg.data);
				return;
			}
			if (msg.type === 'conversationProgress') {
				if (msg.data.requestId == null || this.pending.has(msg.data.requestId)) {
					this.emit('conversationProgress', msg.data);
				}
				return;
			}

			// Correlate responses: background conversations can finish out of order.
			const pending = this.pending.get(msg.requestId);
			if (!pending) return;
			this.pending.delete(msg.requestId);
			if (msg.type === 'error' && msg.code === 'capabilityDisabled') {
				pending.reject(new CapabilityDisabledError(msg.capability, msg.message));
			} else if (msg.type === 'error') pending.reject(new Error(msg.message));
			else pending.resolve(msg.data ?? msg);
		} catch (e) {
			console.error('[ws] Failed to parse message:', e);
		}
	}

	private emit(event: string, data: any) {
		this.listeners.get(event)?.forEach((cb) => {
			try {
				cb(data);
			} catch (e) {
				console.error('[ws] Listener error:', e);
			}
		});
	}

	private rejectPending(reason: string) {
		for (const request of this.pending.values()) request.reject(new Error(reason));
		this.pending.clear();
	}

	private scheduleReconnect() {
		const auth = this.auth;
		if (this.reconnectTimer || !this.host || !auth || !('token' in auth)) return;
		console.log('[ws] Reconnecting in 3s...');
		this.reconnectTimer = setTimeout(() => {
			this.reconnectTimer = null;
			this.connect(this.host, auth).catch(() => {
				// onclose will trigger another scheduleReconnect
			});
		}, 3000);
	}
}

export const wsClient = new WsClient();

// ── Transport helpers ────────────────────────────────────────────────

/** Check if running inside Tauri desktop (not just bundled JS with the property) */
export function isTauri(): boolean {
	return typeof window !== 'undefined' &&
		typeof (window as any).__TAURI_INTERNALS__?.invoke === 'function';
}

const HOST_KEY = 'c9watch-ws-host';
const TOKEN_KEY = 'c9watch-client-token';
/** Pre-C0h key: a full ws:// URL that carried the token. */
const LEGACY_URL_KEY = 'c9watch-ws-url';

/** The URL for a "host:port". The page's own scheme picks ws or wss. */
export function wsUrlFor(host: string): string {
	const secure = typeof location !== 'undefined' && location.protocol === 'https:';
	return `${secure ? 'wss' : 'ws'}://${host}/ws`;
}

/** The last desktop this device paired with, even when its token was dropped. */
export function getStoredHost(): string | null {
	try {
		return localStorage.getItem(HOST_KEY);
	} catch {
		return null;
	}
}

/** The paired desktop and this device's token, if any. */
export function getStoredConnection(): { host: string; token: string } | null {
	try {
		const host = localStorage.getItem(HOST_KEY);
		const token = localStorage.getItem(TOKEN_KEY);
		return host && token ? { host, token } : null;
	} catch {
		return null;
	}
}

export function setStoredConnection(host: string, token: string) {
	try {
		localStorage.setItem(HOST_KEY, host);
		localStorage.setItem(TOKEN_KEY, token);
	} catch {
		// Private mode: the connection works, it is just not remembered.
	}
}

/** Remove the stored token; with `only`, just when it is still that token. */
function clearStoredToken(only?: string) {
	try {
		if (only !== undefined && localStorage.getItem(TOKEN_KEY) !== only) return;
		localStorage.removeItem(TOKEN_KEY);
	} catch {
		// Nothing stored.
	}
}

export function clearStoredConnection() {
	clearStoredToken();
	try {
		localStorage.removeItem(HOST_KEY);
	} catch {
		// Nothing stored.
	}
}

/**
 * Drop the pre-C0h stored URL. Returns true when it carried a token, so the
 * UI can say "Pair again".
 */
export function migrateLegacyWsUrl(): boolean {
	try {
		const old = localStorage.getItem(LEGACY_URL_KEY);
		if (old === null) return false;
		localStorage.removeItem(LEGACY_URL_KEY);
		return /[?&]token=/.test(old);
	} catch {
		return false;
	}
}

/** Should we use WebSocket transport? (vs Tauri IPC) */
export function useWebSocket(): boolean {
	return !!getStoredConnection() || !isTauri();
}
