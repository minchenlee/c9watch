/** Glass starts its own data. Classic starts its data in `(app)/+layout.svelte`,
 * which glass does not render. Only `src/lib/data/` feeds are used here,
 * never the Classic `$lib/stores/*`. */
import { derived, readable, writable } from 'svelte/store';
import type { C9pAgentAttentionEvent, C9pAgentRow, C9pSessionView } from '$lib/c9p-1';
import { transport } from '$lib/data/client';
import type { StreamFrame } from '$lib/data/transport';
import { sessionFeed } from '$lib/data/session-feed';
import { annotationFeed, type AnnotationMap } from '$lib/data/annotation-feed';
import { isDemoMode, isPersistedDemoMode } from '$lib/data/demo/mode';
import { demoGlassOn } from '$lib/data/demo';
import { deriveGlassModel, type GlassModel, type Gaps } from './model';
import type { AttentionSeen } from './model/needs';
import type { ChangeLog } from './model/stale';
import { DEMO_GOALS, DEMO_NEEDS, DEMO_WORKSPACES } from './model/gaps-demo';

export const sessions = writable<C9pSessionView[]>([]);
export const agents = writable<C9pAgentRow[]>([]);
/** The last `agent.attention` per agent id (permission prompts, notices). */
export const attention = writable<AttentionSeen[]>([]);
export const annotations = writable<AnnotationMap>(new Map());
/** The last feed error; `null` while every feed is live. */
export const dataError = writable<unknown | null>(null);
/** True when the glass demo fixture answers (demo mode and `c9watch.demoGlass`).
 * Only then may the shell show fixture-only data such as demo workspaces. */
export const fixtureOn = writable(false);

/** Re-derives once a minute, so 「很久沒動靜」 shows without new rows. */
const clock = readable(Date.now(), set => { const t = setInterval(() => set(Date.now()), 60_000); return () => clearInterval(t); });
const DEMO_GAPS: Gaps = { workspaces: DEMO_WORKSPACES, goals: DEMO_GOALS, needs: DEMO_NEEDS };
let changes: ChangeLog = new Map();
/** The derived glass view model. Demo gaps only while the fixture is on. */
export const model = derived([sessions, agents, attention, fixtureOn, clock], ([$s, $a, $att, $fixture, $now]): GlassModel => {
    const m = deriveGlassModel({ sessions: $s, agents: $a, attention: $att, gaps: $fixture ? DEMO_GAPS : null, changes, now: Math.max($now, Date.now()) });
    changes = m.changes;
    return m;
});

/** `agent.list` again on every `agent.upsert` / `agent.gone` / `agent.failed`
 * (one subscription to `agent.*`, the only agent form the bridge allows).
 * One read at a time; frames during a read cause one more read. If the
 * subscribe is refused or the stream ends, the list is read again whenever
 * the session feed publishes, at most every 2 s. */
function agentFeed(onError: (e: unknown) => void): { stop: () => void; sessionsChanged: () => void } {
    const t = transport();
    let stopped = false, reading = false, again = false, live = false, last = 0;
    let stopSub: (() => void) | null = null;
    async function list() {
        if (stopped) return;
        if (reading) { again = true; return; }
        reading = true; last = Date.now();
        try {
            const rows = (await t.request('agent.list', {}))?.agents ?? [];
            if (stopped) return;
            agents.set(rows);
            const needing = new Set(rows.filter(a => a.status === 'needs_attention').map(a => a.id));
            attention.update(list => list.filter(x => needing.has(x.event.id)));
        } catch (e) { if (!stopped) onError(e); }
        reading = false;
        if (again) { again = false; void list(); }
    }
    function frame(f: StreamFrame) {
        if (stopped) return;
        if ('error' in f && f.error) { live = false; return; }
        if (!('type' in f) || f.type !== 'event' || !('topic' in f)) return;
        if (f.topic === 'agent.attention') {
            const event = f.data as C9pAgentAttentionEvent;
            attention.update(list => [...list.filter(x => x.event.id !== event.id), { event, at: f.ts ?? Date.now() }]);
        } else if (f.topic === 'agent.upsert' || f.topic === 'agent.gone' || f.topic === 'agent.failed') void list();
    }
    // Subscribe first, then read, so no change falls between the two.
    t.subscribe({ topics: ['agent.*'] }, frame).then(
        stop => { if (stopped) stop(); else { stopSub = stop; live = true; } },
        () => { live = false; }
    ).finally(() => void list());
    return {
        stop() { stopped = true; stopSub?.(); },
        sessionsChanged() { if (!live && !reading && Date.now() - last >= 2000) void list(); }
    };
}

/** Starts every glass feed. Returns the stop function; the shell calls it on
 * unmount, so a switch to Classic leaves no glass feed running. */
export function startGlassData(): () => void {
    const demo = isPersistedDemoMode();
    if (demo) isDemoMode.set(true);
    fixtureOn.set(demo && demoGlassOn());
    changes = new Map();
    const t = transport();
    const agentRows = agentFeed(dataError.set);
    // The feed's Session rows are SessionView rows with a cast status.
    const stopSessions = sessionFeed(t, rows => { sessions.set(rows as unknown as C9pSessionView[]); agentRows.sessionsChanged(); }, dataError.set);
    const stopAnnotations = annotationFeed(t, annotations.set, dataError.set);
    return () => { agentRows.stop(); stopSessions(); stopAnnotations(); };
}
