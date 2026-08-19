/**
 * WebSocket client for mobile/browser → desktop communication
 */

type EventCallback = (data: any) => void;

class WsClient {
	private ws: WebSocket | null = null;
	private url: string = '';
	private _connected = false;
	private pendingResolve: ((value: any) => void) | null = null;
	private pendingReject: ((reason: any) => void) | null = null;
	private listeners = new Map<string, Set<EventCallback>>();
	private reconnectTimer: ReturnType<typeof setTimeout> | null = null;

	get isConnected() {
		return this._connected;
	}

	async connect(url: string): Promise<void> {
		this.url = url;
		return new Promise((resolve, reject) => {
			try {
				this.ws = new WebSocket(url);
			} catch (e) {
				reject(e);
				return;
			}

			const timeout = setTimeout(() => {
				reject(new Error('Connection timeout'));
				this.ws?.close();
			}, 5000);

			this.ws.onopen = () => {
				clearTimeout(timeout);
				this._connected = true;
				console.log('[ws] Connected');
				resolve();
			};

			this.ws.onerror = () => {
				clearTimeout(timeout);
				if (!this._connected) reject(new Error('Connection failed'));
			};

			this.ws.onclose = () => {
				this._connected = false;
				this.rejectPending('Connection closed');
				this.scheduleReconnect();
			};

			this.ws.onmessage = (event) => this.handleMessage(event);
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
		return new Promise((resolve, reject) => {
			this.pendingResolve = resolve as (value: any) => void;
			this.pendingReject = reject;
			this.ws!.send(JSON.stringify({ type, ...data }));
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
				this.emit('conversationProgress', msg.data);
				return;
			}

			// Request-response: resolve or reject the pending promise
			if (msg.type === 'error') {
				this.pendingReject?.(new Error(msg.message));
			} else {
				this.pendingResolve?.(msg.data ?? msg);
			}
			this.pendingResolve = null;
			this.pendingReject = null;
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
		this.pendingReject?.(new Error(reason));
		this.pendingResolve = null;
		this.pendingReject = null;
	}

	private scheduleReconnect() {
		if (this.reconnectTimer || !this.url) return;
		console.log('[ws] Reconnecting in 3s...');
		this.reconnectTimer = setTimeout(() => {
			this.reconnectTimer = null;
			this.connect(this.url).catch(() => {
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

/** Get stored WS URL (set by QR code scan on mobile) */
export function getStoredWsUrl(): string | null {
	try {
		return localStorage.getItem('c9watch-ws-url');
	} catch {
		return null;
	}
}

/** Store WS URL from QR code scan */
export function setStoredWsUrl(url: string) {
	localStorage.setItem('c9watch-ws-url', url);
}

/** Clear stored WS URL */
export function clearStoredWsUrl() {
	localStorage.removeItem('c9watch-ws-url');
}

/** Should we use WebSocket transport? (vs Tauri IPC) */
export function useWebSocket(): boolean {
	return !!getStoredWsUrl() || !isTauri();
}
