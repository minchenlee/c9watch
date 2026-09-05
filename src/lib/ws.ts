/**
 * WebSocket client for mobile/browser → desktop communication
 */

type EventCallback = (data: any) => void;

class WsClient {
	private ws: WebSocket | null = null;
	private url: string = '';
	private _connected = false;
	private nextRequestId = 0;
	private pending = new Map<number, { resolve: (value: any) => void; reject: (reason: any) => void }>();
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
			if (msg.type === 'error') pending.reject(new Error(msg.message));
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
