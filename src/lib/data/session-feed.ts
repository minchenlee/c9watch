/** Snapshot/cursor subscription for c9p clients. Expiry replaces the entire
 * state; reconnect never treats a failed read as an empty successful one.
 * `failed` gets each error, and `null` once the stream is live again. */
import type { Transport, StreamFrame, Unsubscribe } from './transport';
import type { Session } from '../types';
import type { C9pSessionView } from '../c9p-1';
import { sessionFromView } from './session';
export const isUnavailable = (error: unknown) => (error as { code?: unknown } | null)?.code === 'core_unavailable';
export function sessionFeed(transport: Transport, apply: (sessions: Session[]) => void, failed: (error: unknown | null) => void = () => {}): Unsubscribe {
    let generation = 0;
    let stopped = false;
    let unsubscribe: Unsubscribe | null = null;
    let retry: ReturnType<typeof setTimeout> | null = null;
    let flushTimer: ReturnType<typeof setTimeout> | null = null;
    let rows: Session[] = [];
    let batch: StreamFrame[] = [];
    // The last failure was core_unavailable (the desktop fallback).
    let unavailable = false;
    let onVisible: (() => void) | null = null;
    function flush() {
        flushTimer = null;
        if (stopped) return;
        for (const frame of batch.splice(0)) {
            if (!('data' in frame) || !('topic' in frame) || frame.type !== 'event') continue;
            const data = frame.data as any;
            if (!data || typeof data.sessionKey !== 'string') continue;
            if (frame.topic === 'session.gone') rows = rows.filter(s => s.sessionKey !== data.sessionKey);
            if (frame.topic === 'session.view') {
                const session = sessionFromView(data as C9pSessionView);
                const index = rows.findIndex(s => s.sessionKey === session.sessionKey);
                if (index < 0) rows.push(session); else rows[index] = session;
            }
        }
        apply([...rows]);
    }
    async function refresh() {
        const mine = ++generation;
        unsubscribe?.(); unsubscribe = null;
        if (flushTimer) clearTimeout(flushTimer);
        flushTimer = null; batch = [];
        try {
            if (unavailable) {
                // Without core, `session.views` is a full native scan, but a
                // subscribe fails at once: probe with it first.
                (await transport.subscribe({ topics: ['session.view', 'session.gone'] }, () => {}))();
                if (stopped || mine !== generation) return;
            }
            const snapshot = await transport.request('session.views', {});
            if (stopped || mine !== generation) return;
            // Core always sends a cursor. Without one it is the fallback's scan,
            // and a stream with no cursor could miss a session.gone.
            if (snapshot.cursor == null) throw { code: 'core_unavailable', message: 'Snapshot has no cursor' };
            let ready = false;
            const stop = await transport.subscribe({ topics: ['session.view','session.gone'], since: snapshot.cursor }, frame => {
                if (stopped || mine !== generation) return;
                if ('error' in frame && frame.error) {
                    // Incrementing generation synchronously invalidates this stream.
                    if (frame.error.code === 'cursor_expired') void refresh();
                    else retryLater(frame.error, mine);
                    return;
                }
                batch.push(frame);
                if (ready && flushTimer === null) flushTimer = setTimeout(flush, 16);
            });
            if (stopped || mine !== generation) { stop(); return; }
            unsubscribe = stop;
            rows = snapshot.sessions.map(sessionFromView);
            ready = true;
            // Drain frames that raced the subscribe response before publishing.
            flush();
            unavailable = false;
            failed(null);
        } catch (error) { retryLater(error, mine); }
    }
    /** Keeps the last rows. Without core (the desktop fallback) the snapshot
     * is a full local scan, so it is retried less often. */
    function retryLater(error: unknown, mine: number) {
        if (stopped || mine !== generation) return;
        generation++;
        unsubscribe?.(); unsubscribe = null;
        if (retry) clearTimeout(retry);
        // Queued deltas of the ended stream must not publish over newer data.
        if (flushTimer) clearTimeout(flushTimer);
        flushTimer = null; batch = [];
        unavailable = isUnavailable(error);
        failed(error);
        retry = setTimeout(() => { retry = null; whenVisible(); }, unavailable ? 5000 : 1000);
    }
    /** A hidden window does not probe for core; it retries on becoming visible. */
    function whenVisible() {
        if (!unavailable || typeof document === 'undefined' || !document.hidden) { void refresh(); return; }
        onVisible = () => {
            if (document.hidden) return;
            document.removeEventListener('visibilitychange', onVisible!); onVisible = null;
            void refresh();
        };
        document.addEventListener('visibilitychange', onVisible);
    }
    void refresh();
    return () => {
        stopped = true; generation++; unsubscribe?.();
        if (retry) clearTimeout(retry);
        if (flushTimer) clearTimeout(flushTimer);
        if (onVisible) document.removeEventListener('visibilitychange', onVisible);
    };
}
