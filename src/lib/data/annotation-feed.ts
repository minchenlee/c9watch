/** Snapshot plus replay for annotations. A replacement source map is applied
 * atomically; reconnect and cursor expiry rebuild it from a new snapshot. */
import type { C9pAnnotation, C9pAnnotationChangedEvent, C9pAnnotationRemovedEvent } from '../c9p-1';
import type { Transport, StreamFrame, Unsubscribe } from './transport';
export type AnnotationMap = ReadonlyMap<string, C9pAnnotation>;
export function annotationId(a: Pick<C9pAnnotation, 'target' | 'key'>): string {
    return JSON.stringify([a.target.kind, a.target.sessionKey, a.key]);
}
export function annotationFeed(t: Transport, apply: (rows: AnnotationMap) => void, failed: (error: unknown | null) => void = () => {}): Unsubscribe {
    let rows = new Map<string, C9pAnnotation>();
    let generation = 0;
    let stopped = false;
    let unsubscribe: Unsubscribe | null = null;
    let retry: ReturnType<typeof setTimeout> | null = null;
    function receive(frame: StreamFrame) {
        if (!('type' in frame) || frame.type !== 'event' || !('topic' in frame) || !('data' in frame)) return;
        const next = new Map(rows);
        if (frame.topic === 'annotation.changed') {
            const data = frame.data as C9pAnnotationChangedEvent;
            if (!data?.target?.sessionKey || typeof data.source !== 'string' || !data.values) return;
            for (const [id, a] of next) {
                if (a.target.kind === data.target.kind && a.target.sessionKey === data.target.sessionKey && a.key.startsWith(`${data.source}/`)) next.delete(id);
            }
            for (const a of Object.values(data.values)) next.set(annotationId(a), a);
        } else if (frame.topic === 'annotation.removed') {
            const data = frame.data as C9pAnnotationRemovedEvent;
            if (!data?.target?.sessionKey || typeof data.key !== 'string') return;
            next.delete(annotationId(data));
        } else return;
        rows = next;
        apply(new Map(rows));
    }
    function schedule(error: unknown, mine: number) {
        if (stopped || mine !== generation) return;
        generation++;
        unsubscribe?.(); unsubscribe = null;
        failed(error);
        if (retry) clearTimeout(retry);
        retry = null;
        // A denied method/topic cannot recover by replaying the same request.
        // Keep the last snapshot and expose the error until this feed is recreated.
        if (error && typeof error === 'object' && 'code' in error && error.code === 'not_allowed') return;
        retry = setTimeout(() => { retry = null; void refresh(); }, 1000);
    }
    async function refresh() {
        const mine = ++generation;
        unsubscribe?.(); unsubscribe = null;
        try {
            const snapshot = await t.request('annotation.list', {});
            if (stopped || mine !== generation) return;
            let ready = false;
            const buffered: StreamFrame[] = [];
            const stop = await t.subscribe({ topics: ['annotation.changed', 'annotation.removed'], since: snapshot.cursor }, frame => {
                if (stopped || mine !== generation) return;
                if ('error' in frame && frame.error) { schedule(frame.error, mine); return; }
                if (!ready) buffered.push(frame); else receive(frame);
            });
            if (stopped || mine !== generation) { stop(); return; }
            unsubscribe = stop;
            rows = new Map(snapshot.annotations.map(a => [annotationId(a), a]));
            ready = true;
            apply(new Map(rows));
            for (const frame of buffered) receive(frame);
            failed(null);
        } catch (error) { schedule(error, mine); }
    }
    void refresh();
    return () => { stopped = true; generation++; unsubscribe?.(); if (retry) clearTimeout(retry); };
}
