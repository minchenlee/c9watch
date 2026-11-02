/** Live data for the desktop app: c9p event streams while core is usable.
 * The in-process fallback has no event stream, so its native snapshot events
 * are applied only while the c9p stream reports `core_unavailable`. */
import type { Transport, StreamFrame, Unsubscribe } from './transport';
import type { DetectionDiagnostics, Session } from '../types';
import { sessionFeed, isUnavailable } from './session-feed';

type Feed<T> = (apply: (value: T) => void, failed: (error: unknown | null) => void) => Unsubscribe;
type Native<T> = (apply: (value: T) => void) => Promise<() => void>;

/** `snapshot`, when given, is read once the native listener is in place: a
 * fallback that saw no change since core took over emits nothing. Native
 * events that arrive during the read are buffered and applied after it, in
 * order, so the snapshot never replaces a newer value. */
export function withNativeFallback<T>(feed: Feed<T>, native: Native<T>, apply: (value: T) => void, snapshot?: () => Promise<T | null>): Unsubscribe {
    let stopped = false;
    let fallback = false;
    let unlisten: Promise<() => void> | null = null;
    const drop = () => { const u = unlisten; unlisten = null; void u?.then(stop => stop()).catch(() => {}); };
    const stopFeed = feed(value => { if (!stopped) apply(value); }, error => {
        if (stopped) return;
        if (isUnavailable(error)) {
            fallback = true;
            // Checked on delivery: a native event queued before the switch
            // back to c9p is dropped, never applied over newer stream data.
            if (!unlisten) {
                let buffered: T[] | null = snapshot ? [] : null;
                const deliver = (value: T) => { if (fallback && !stopped && unlisten === mine) apply(value); };
                const mine = unlisten = native(value => { if (buffered) buffered.push(value); else deliver(value); });
                if (snapshot) void mine.then(snapshot)
                    .then(value => { if (value) deliver(value); }, () => {})
                    .finally(() => { const queued = buffered ?? []; buffered = null; queued.forEach(deliver); });
            }
        } else if (error === null) { fallback = false; drop(); }
    });
    return () => { stopped = true; stopFeed(); drop(); };
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
    return withNativeFallback((a, f) => sessionFeed(transport(), a, f), listenSessions, apply);
}
export async function watchDiagnostics(apply: (d: DetectionDiagnostics) => void): Promise<Unsubscribe> {
    const [{ transport }, { listenDiagnostics }] = await Promise.all([import('./client'), import('./tauri-bridge')]);
    return withNativeFallback((a, f) => diagnosticsFeed(transport(), a, f), listenDiagnostics, apply,
        () => transport().request('diag.detection', {}) as Promise<DetectionDiagnostics | null>);
}
