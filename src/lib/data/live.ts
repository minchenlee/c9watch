/** Live data for the desktop app: c9p event streams while core is usable.
 * The in-process fallback has no event stream, so its native snapshot events
 * are applied only while the c9p stream reports `core_unavailable`. */
import type { Transport, StreamFrame, Unsubscribe } from './transport';
import type { DetectionDiagnostics, Session } from '../types';
import { sessionFeed, isUnavailable } from './session-feed';

type Feed<T> = (apply: (value: T) => void, failed: (error: unknown | null) => void) => Unsubscribe;
type Native<T> = (apply: (value: T) => void) => Promise<() => void>;
const SNAPSHOT_TIMEOUT_MS = 3000;

/** One fallback period runs from `core_unavailable` to recovery or stop.
 * Native events are change-only, so each period installs the listener, then
 * reads one native snapshot. Events that arrive during the read are buffered
 * and applied after it, in order, so the snapshot never replaces a newer
 * value. A read that does not settle in time flushes the buffer and its late
 * result is ignored. Ending the period cancels its listener, timer and
 * buffer; nothing starts for a period that has already ended. */
function fallbackPeriod<T>(native: Native<T>, apply: (value: T) => void, snapshot: () => Promise<T | null>): () => void {
    let live = true;
    let buffered: T[] | null = [];
    let timer: ReturnType<typeof setTimeout> | null = null;
    let unlisten: (() => void) | null = null;
    const deliver = (value: T) => { if (live) apply(value); };
    const flush = (value?: T | null) => {
        if (!live || !buffered) return;
        if (timer) clearTimeout(timer);
        timer = null;
        const queued = buffered; buffered = null;
        if (value) deliver(value);
        queued.forEach(deliver);
    };
    native(value => { if (!live) return; if (buffered) buffered.push(value); else deliver(value); }).then(stop => {
        if (!live) { stop(); return; }
        unlisten = stop;
        timer = setTimeout(() => flush(), SNAPSHOT_TIMEOUT_MS);
        Promise.resolve().then(snapshot).then(value => flush(value), () => flush());
    }, () => flush());
    return () => {
        live = false; buffered = null;
        if (timer) clearTimeout(timer);
        timer = null;
        try { unlisten?.(); } catch {}
        unlisten = null;
    };
}

/** Native snapshot events apply only during a fallback period: one queued
 * before the switch back to c9p is dropped, never applied over stream data. */
export function withNativeFallback<T>(feed: Feed<T>, native: Native<T>, apply: (value: T) => void, snapshot: () => Promise<T | null>): Unsubscribe {
    let stopped = false;
    let endPeriod: (() => void) | null = null;
    const end = () => { const e = endPeriod; endPeriod = null; e?.(); };
    const stopFeed = feed(value => { if (!stopped) apply(value); }, error => {
        if (stopped) return;
        if (isUnavailable(error)) { if (!endPeriod) endPeriod = fallbackPeriod(native, apply, snapshot); }
        else if (error === null) end();
    });
    return () => { stopped = true; stopFeed(); end(); };
}

/** Diagnostics are a whole value per event, emitted when they change.
 * Subscribing before the read means no change is missed; buffered events
 * apply after the read, in order, so the newest value wins. The bridge
 * resumes the stream from its cursor after a core reconnect. */
export function diagnosticsFeed(t: Transport, apply: (d: DetectionDiagnostics) => void, failed: (error: unknown | null) => void = () => {}): Unsubscribe {
    let generation = 0;
    let stopped = false;
    let unsubscribe: Unsubscribe | null = null;
    let retry: ReturnType<typeof setTimeout> | null = null;
    const receive = (frame: StreamFrame) => {
        if ('type' in frame && frame.type === 'event' && 'topic' in frame && frame.topic === 'diag.detection' && frame.data) {
            apply(frame.data as DetectionDiagnostics);
        }
    };
    function retryLater(error: unknown, mine: number) {
        if (stopped || mine !== generation) return;
        generation++;
        unsubscribe?.(); unsubscribe = null;
        if (retry) clearTimeout(retry);
        failed(error);
        retry = setTimeout(() => { retry = null; void refresh(); }, isUnavailable(error) ? 5000 : 1000);
    }
    async function refresh() {
        const mine = ++generation;
        unsubscribe?.(); unsubscribe = null;
        try {
            let ready = false;
            const buffered: StreamFrame[] = [];
            const stop = await t.subscribe({ topics: ['diag.detection'] }, frame => {
                if (stopped || mine !== generation) return;
                if ('error' in frame && frame.error) { retryLater(frame.error, mine); return; }
                if (ready) receive(frame); else buffered.push(frame);
            });
            if (stopped || mine !== generation) { stop(); return; }
            unsubscribe = stop;
            const snapshot = await t.request('diag.detection', {});
            if (stopped || mine !== generation) return;
            ready = true;
            failed(null);
            if (snapshot) apply(snapshot as DetectionDiagnostics);
            for (const frame of buffered.splice(0)) receive(frame);
        } catch (error) { retryLater(error, mine); }
    }
    void refresh();
    return () => { stopped = true; generation++; unsubscribe?.(); if (retry) clearTimeout(retry); };
}

export async function watchSessions(apply: (sessions: Session[]) => void): Promise<Unsubscribe> {
    const [{ transport }, { listenSessions }] = await Promise.all([import('./client'), import('./tauri-bridge')]);
    // Without core, `session.views` is the fallback's native scan (no cursor).
    // With a cursor, core is back and the feed's own recovery applies it.
    return withNativeFallback((a, f) => sessionFeed(transport(), a, f), listenSessions, apply, async () => {
        const snapshot = await transport().request('session.views', {});
        return snapshot.cursor == null ? snapshot.sessions as unknown as Session[] : null;
    });
}
export async function watchDiagnostics(apply: (d: DetectionDiagnostics) => void): Promise<Unsubscribe> {
    const [{ transport }, { listenDiagnostics }] = await Promise.all([import('./client'), import('./tauri-bridge')]);
    return withNativeFallback((a, f) => diagnosticsFeed(transport(), a, f), listenDiagnostics, apply,
        () => transport().request('diag.detection', {}) as Promise<DetectionDiagnostics | null>);
}
