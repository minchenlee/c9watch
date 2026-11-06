/** The `C9uiHost` object an extension gets in `mount(root, host)` (glass
 * build plan §3.3; `ext` per owner D41 R7). Extensions are trusted code
 * (D31): this object is a convenience contract, not a sandbox.
 *
 * Only type imports: the node unit test (`scripts/test-glass-ext.mjs`)
 * loads this file on its own. The shell passes the real parts in `HostDeps`. */
import type { Transport } from '$lib/data/transport';

export type GlassMode = 'off' | 'subtle' | 'full';
export type IntentName = 'open-session' | 'open-handling' | 'navigate' | 'focus-needs-you';
export type IntentResult = 'done' | 'refused' | 'cancelled';
export type HostEvent = 'context' | 'theme' | 'resize' | 'suspend' | 'resume';
export type HostContext = { workspaceId?: string; goalId?: string; sessionKey?: string; needsItemId?: string };
export type HostTheme = { tokens: Record<string, string>; glass: GlassMode };

export interface C9uiHost {
    v: 'c9ui/1';
    /** R7: exactly these keys. `call` and `on` are reserved for the core
     * part of an extension: in v1 `call` rejects with `{ code: 'refused' }`
     * and `on` returns a no-op unsubscribe. */
    ext: {
        id: string;
        version: string;
        call(method: string, params?: unknown): Promise<never>;
        on(event: string, cb: (v: unknown) => void): () => void;
    };
    c9p: Transport;
    readonly theme: HostTheme;
    readonly context: HostContext;
    prefs: { get(): unknown; set(blob: unknown): void };
    intent(name: IntentName, args?: Record<string, unknown>): Promise<IntentResult>;
    on(event: HostEvent, cb: (v: unknown) => void): () => void;
    ready(): void;
    keys: { forward(e: KeyboardEvent): void };
}

/** What an extension's entry module exports. */
export type ExtModule = { mount(root: ShadowRoot, host: C9uiHost): { destroy(): void } | void | Promise<{ destroy(): void } | void> };

export const MAX_PREFS_BYTES = 64 * 1024;
export const READY_TIMEOUT_MS = 5000;
const EVENTS: ReadonlySet<string> = new Set(['context', 'theme', 'resize', 'suspend', 'resume']);

export const prefsKey = (id: string) => `c9ext.${id}.prefs`;

/** v1 prefs live in this window's localStorage, one blob per extension. */
export function readPrefs(id: string, storage: Storage | undefined = globalThis.localStorage): unknown {
    try {
        const raw = storage?.getItem(prefsKey(id));
        return raw == null ? undefined : JSON.parse(raw);
    } catch { return undefined; }
}

/** Throws a RangeError over 64 KiB and a TypeError for a value JSON cannot
 * hold, so the extension learns at once. Storage errors are swallowed. */
export function writePrefs(id: string, blob: unknown, storage: Storage | undefined = globalThis.localStorage): void {
    if (blob === undefined) { try { storage?.removeItem(prefsKey(id)); } catch { /* no storage */ } return; }
    const raw = JSON.stringify(blob);
    if (raw === undefined) throw new TypeError('prefs must be a JSON value');
    if (new TextEncoder().encode(raw).length > MAX_PREFS_BYTES) throw new RangeError('prefs are larger than 64 KiB');
    try { storage?.setItem(prefsKey(id), raw); } catch { /* no storage or full */ }
}

export type HostDeps = {
    c9p: Transport;
    theme(): HostTheme;
    context(): HostContext;
    /** Opens a glass route hash (`#/needs`). False when it is not a page. */
    navigate(hash: string): boolean;
    forwardKey(e: KeyboardEvent): void;
    onReady(): void;
    storage?: Storage;
};

export type HostControl = {
    host: C9uiHost;
    /** Sends an event to the extension's listeners. A throwing listener is
     * logged and does not stop the others. */
    emit(event: HostEvent, value: unknown): void;
    /** After this every call is a no-op and every intent is refused. */
    dispose(): void;
};

export function createHost(ext: { id: string; version: string }, deps: HostDeps): HostControl {
    const listeners = new Map<HostEvent, Set<(v: unknown) => void>>();
    let live = true;
    const host: C9uiHost = {
        v: 'c9ui/1',
        ext: Object.freeze({
            id: ext.id,
            version: ext.version,
            call: () => Promise.reject(Object.freeze({ code: 'refused', msg: 'extension core calls are not available yet' })),
            on: () => () => {}
        }),
        c9p: deps.c9p,
        get theme() { return deps.theme(); },
        get context() { return { ...deps.context() }; },
        prefs: Object.freeze({
            get: () => readPrefs(ext.id, deps.storage),
            set: (blob: unknown) => { if (live) writePrefs(ext.id, blob, deps.storage); }
        }),
        async intent(name, args = {}) {
            if (!live) return 'refused';
            switch (name) {
                case 'navigate': return typeof args.to === 'string' && deps.navigate(args.to) ? 'done' : 'refused';
                case 'focus-needs-you': return deps.navigate('#/needs') ? 'done' : 'refused';
                // The handling panel (G4/G5) and the session view are not
                // reachable from the host yet.
                default: return 'refused';
            }
        },
        on(event, cb) {
            if (!live || !EVENTS.has(event) || typeof cb !== 'function') return () => {};
            const set = listeners.get(event) ?? new Set();
            listeners.set(event, set);
            set.add(cb);
            return () => { set.delete(cb); };
        },
        ready() { if (live) deps.onReady(); },
        keys: Object.freeze({ forward: (e: KeyboardEvent) => { if (live) deps.forwardKey(e); } })
    };
    Object.freeze(host);
    return {
        host,
        emit(event, value) {
            if (!live) return;
            for (const cb of [...(listeners.get(event) ?? [])]) {
                try { cb(value); } catch (err) { console.error(`[c9ext ${ext.id}] ${event} listener threw`, err); }
            }
        },
        dispose() { live = false; listeners.clear(); }
    };
}
