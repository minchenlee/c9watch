/** Glass starts its own data. Classic starts its data in `(app)/+layout.svelte`,
 * which glass does not render. Only `src/lib/data/` feeds are used here,
 * never the Classic `$lib/stores/*`. */
import { derived, get, readable, writable } from 'svelte/store';
import type { C9pAgentAttentionEvent, C9pAgentRow, C9pAgentTurnEndEvent, C9pMsgSummary, C9pSessionView } from '$lib/c9p-1';
import { transport } from '$lib/data/client';
import type { StreamFrame } from '$lib/data/transport';
import { sessionFeed } from '$lib/data/session-feed';
import { annotationFeed, type AnnotationMap } from '$lib/data/annotation-feed';
import { mail as sharedMail } from '$lib/data/stores/mail';
import { isDemoMode, isPersistedDemoMode } from '$lib/data/demo/mode';
import { demoGlassOn, setDemoGlassActive } from '$lib/data/demo';
import { deriveGlassModel, type GlassModel, type Gaps } from './model';
import { attentionKey, openAttention, type AttentionSeen } from './model/needs';
import { sessionKeyOf } from './model/rooms';
import type { ChangeLog } from './model/stale';
import { DEMO_GOALS, DEMO_NEEDS, DEMO_WORKSPACES } from './model/gaps-demo';

export const sessions = writable<C9pSessionView[]>([]);
export const agents = writable<C9pAgentRow[]>([]);
/** Local time the `agent.list` behind `agents` was asked. */
export const agentsAt = writable(-Infinity);
/** Open `agent.attention` events, one per `attentionKey`. Cleared only by
 * the rules of `openAttention`, never by the detected status. */
export const attention = writable<AttentionSeen[]>([]);
/** The last `agent.turn_end` per agent id (clears hook attention). */
export const turnEnds = writable<ReadonlyMap<string, C9pAgentTurnEndEvent>>(new Map());
/** Mail rows from the shared mail feed (empty where mail is not exposed). */
export const mail = writable<C9pMsgSummary[]>([]);
export const annotations = writable<AnnotationMap>(new Map());
/** The last feed error; `null` while every feed is live. */
export const dataError = writable<unknown | null>(null);
/** True when the glass demo fixture answers (demo mode and `c9watch.demoGlass`).
 * Only then may the shell show fixture-only data such as demo workspaces. */
export const fixtureOn = writable(false);

/** Session keys of agents the person retired here (D42). `agent.retire`
 * drops the agent row at once, but the session view stays until core's next
 * detection or `session.gone`; without this the model would read it as an
 * observed session and bring the stopped item back. A key is dropped when
 * the session feed no longer lists it. */
export const retiredSessions = writable<ReadonlySet<string>>(new Set());
export function hideRetiredSession(key: string | null | undefined): void {
    if (key) retiredSessions.update(s => (s.has(key) ? s : new Set(s).add(key)));
}
function pruneRetiredSessions(rows: C9pSessionView[]): void {
    retiredSessions.update(s => {
        if (!s.size) return s;
        const live = new Set(rows.map(sessionKeyOf));
        return [...s].every(k => live.has(k)) ? s : new Set([...s].filter(k => live.has(k)));
    });
}

/** Re-derives once a minute, so 「很久沒動靜」 shows without new rows. */
const clock = readable(Date.now(), set => { const t = setInterval(() => set(Date.now()), 60_000); return () => clearInterval(t); });
const DEMO_GAPS: Gaps = { workspaces: DEMO_WORKSPACES, goals: DEMO_GOALS, needs: DEMO_NEEDS };
let changes: ChangeLog = new Map();
/** The derived glass view model. Demo gaps only while the fixture is on. */
export const model = derived([sessions, agents, agentsAt, attention, turnEnds, mail, fixtureOn, clock, retiredSessions], ([$all, $a, $at, $att, $ends, $mail, $fixture, $now, $retired]): GlassModel => {
    const $s = $retired.size ? $all.filter(v => !$retired.has(sessionKeyOf(v))) : $all;
    const m = deriveGlassModel({ sessions: $s, agents: $a, agentsAt: $at, attention: $att, turnEnds: $ends, mail: $mail, gaps: $fixture ? DEMO_GAPS : null, changes, now: Math.max($now, Date.now()) });
    changes = m.changes;
    return m;
});

/** Drops the attention events `openAttention` closed, so the list stays small. */
function pruneAttention() {
    const facts = { agents: new Map(get(agents).map(a => [a.id, a])), mail: new Map(get(mail).map(m => [m.msgId, m])), turnEnds: get(turnEnds), agentsAt: get(agentsAt) };
    attention.update(list => { const open = openAttention(list, facts); return open.length === list.length ? list : open; });
}

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
        const askedAt = last;
        try {
            const rows = (await t.request('agent.list', {}))?.agents ?? [];
            if (stopped) return;
            agents.set(rows); agentsAt.set(askedAt);
            pruneAttention();
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
            const key = attentionKey(event);
            attention.update(list => [...list.filter(x => attentionKey(x.event) !== key), { event, at: f.ts ?? Date.now(), receivedAt: Date.now() }]);
            // A pause from elsewhere sends no agent.upsert: read the rows again.
            if (event.reason === 'mail_paused') void list();
        } else if (f.topic === 'agent.turn_end') {
            const end = f.data as C9pAgentTurnEndEvent;
            turnEnds.update(m => new Map(m).set(end.id, end));
            pruneAttention();
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
    setDemoGlassActive(true);
    changes = new Map();
    attention.set([]); turnEnds.set(new Map()); mail.set([]); agentsAt.set(-Infinity);
    const t = transport();
    // The shared data-layer mail store: its feed starts with the first
    // subscriber and stops with the last, and G5's send/pause use the same one.
    const stopMail = sharedMail.subscribe(snap => { mail.set([...snap.messages.values()]); pruneAttention(); });
    const agentRows = agentFeed(dataError.set);
    // The feed's Session rows are SessionView rows with a cast status.
    const stopSessions = sessionFeed(t, rows => { const views = rows as unknown as C9pSessionView[]; pruneRetiredSessions(views); sessions.set(views); agentRows.sessionsChanged(); }, dataError.set);
    const stopAnnotations = annotationFeed(t, annotations.set, dataError.set);
    return () => { agentRows.stop(); stopMail(); stopSessions(); stopAnnotations(); setDemoGlassActive(false); };
}
