/** Snapshot/cursor subscription for c9p clients. Expiry replaces the entire
 * state; reconnect never treats a failed read as an empty successful one. */
import type { Transport, StreamFrame, Unsubscribe } from './transport';
import type { Session } from '../types';
import type { C9pSessionView } from '../c9p-1';
import { sessionFromView } from './session';
export function sessionFeed(transport: Transport, apply: (sessions: Session[]) => void): Unsubscribe {
    let generation = 0;
    let stopped = false;
    let unsubscribe: Unsubscribe | null = null;
    let retry: ReturnType<typeof setTimeout> | null = null;
    let flushTimer: ReturnType<typeof setTimeout> | null = null;
    let rows: Session[] = [];
    let batch: StreamFrame[] = [];
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
            const snapshot = await transport.request('session.views', {});
            if (stopped || mine !== generation) return;
            let ready = false;
            const stop = await transport.subscribe({ topics: ['session.view','session.gone'], since: snapshot.cursor }, frame => {
                if (stopped || mine !== generation) return;
                if ('error' in frame && frame.error?.code === 'cursor_expired') {
                    // Incrementing generation synchronously invalidates this stream.
                    void refresh(); return;
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
        } catch {
            if (!stopped && mine === generation) retry = setTimeout(() => { retry = null; void refresh(); }, 1000);
        }
    }
    void refresh();
    return () => {
        stopped = true; generation++; unsubscribe?.();
        if (retry) clearTimeout(retry);
        if (flushTimer) clearTimeout(flushTimer);
    };
}
