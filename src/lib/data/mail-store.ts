/** Agent mail over c9p: `msg.list` snapshot plus live `msg.state` events,
 * and the human controls (`msg.cancel`, `mail.pause`, `mail.resume`,
 * `msg.send`). This file has no runtime imports, so the node tests load it
 * as is. The Svelte store is `stores/mail.ts`.
 *
 * Every mail method is denied (`not_allowed`) when a build does not expose
 * mail to this window. That is a terminal state, not an error to retry. */
import type { C9pAgentRow, C9pMsgSendResult, C9pMsgStateEvent, C9pMsgSummary } from '../c9p-1';
import type { StreamFrame, Transport } from './transport';

export type MailStatus = 'loading' | 'ready' | 'unavailable' | 'error';
export interface MailSnapshot {
    status: MailStatus;
    /** Why the status is `unavailable` or `error`. */
    error: string | null;
    messages: ReadonlyMap<string, C9pMsgSummary>;
    /** Agent id → its session key, for the card line. */
    sessionKeys: ReadonlyMap<string, string>;
}
export interface MailFeed {
    stop(): void;
    /** Put a row in now, for example the result of this client's own send. */
    upsert(row: C9pMsgSummary): void;
}

/** Pages read on load: 5 × 200 = the newest 1000 mails. */
export const PAGE_LIMIT = 200;
export const MAX_PAGES = 5;
const RETRY_MS = 5000;
/** An agent without a session key yet (not listed, or listed with
 * `sessionKey: null` until detection finds its session) makes a later event
 * run `agent.list` again: after at least this wait, ... */
export const AGENT_DEBOUNCE_MS = 1000;
/** ... at most once per this interval overall, ... */
export const AGENT_REFRESH_MS = 5000;
/** ... and for one agent only within this time after its first ask. */
export const AGENT_GIVE_UP_MS = 60_000;
const UNAVAILABLE = new Set(['not_allowed', 'unknown_method', 'upgrade_required', 'unsupported']);

export function errorCode(error: unknown): string | null {
    return error && typeof error === 'object' && 'code' in error && typeof (error as { code: unknown }).code === 'string'
        ? (error as { code: string }).code : null;
}
export function isUnavailableError(error: unknown): boolean {
    const code = errorCode(error);
    return code !== null && UNAVAILABLE.has(code);
}
function errorText(error: unknown): string {
    if (error && typeof error === 'object' && 'msg' in error) return String((error as { msg: unknown }).msg);
    if (error instanceof Error) return error.message;
    return String(error);
}

/** The agent id in a `<machine>/<id>` address. */
export function agentIdOf(addr: string | null | undefined): string | null {
    if (!addr) return null;
    const i = addr.lastIndexOf('/');
    return i < 0 ? addr : addr.slice(i + 1);
}

/** Apply one `msg.state` event. A known row takes the event only when it is
 * not older than the row's state. An unknown row is built from a `queued`
 * event, which carries the whole summary. Returns null when the event cannot
 * make a row (the caller then reads it with `msg.read`). */
export function applyState(row: C9pMsgSummary | undefined, e: C9pMsgStateEvent): C9pMsgSummary | null {
    if (row) {
        if (e.at < row.stateAt) return row;
        return { ...row, state: e.state, stateAt: e.at, detail: e.detail ?? null, via: e.via ?? row.via };
    }
    if (!e.sender || !e.recipient) return null;
    return {
        msgId: e.msgId, from: e.sender, to: e.recipient, kind: e.kind ?? 'request', state: e.state,
        stateAt: e.at, createdAt: e.at, detail: e.detail ?? null, via: e.via ?? null,
        replyTo: e.replyTo ?? null, threadId: e.threadId ?? null, noticeId: null, hop: 0, preview: e.preview ?? ''
    };
}

/** The row this client shows right after its own `msg.send`, before the
 * `queued` event arrives. The send result has no engine time, so the row's
 * `stateAt` is 0: any engine event or `msg.list` row wins over it, and it
 * never replaces a state the engine already reported. `createdAt` is local
 * time, only for ordering and for keeping the row across a refresh. */
export function rowFromSend(result: C9pMsgSendResult, body: string, now: number): C9pMsgSummary {
    return {
        msgId: result.msgId, from: { kind: 'human', addr: null, name: null }, to: result.to, kind: 'request',
        state: result.state, stateAt: 0, createdAt: now, detail: null, via: null, replyTo: null,
        threadId: null, noticeId: null, hop: 0, preview: [...body].slice(0, 200).join('')
    };
}

export function mailFeed(t: Transport, apply: (s: MailSnapshot) => void, now: () => number = () => Date.now()): MailFeed {
    let messages = new Map<string, C9pMsgSummary>();
    let sessionKeys = new Map<string, string>();
    let status: MailStatus = 'loading';
    let error: string | null = null;
    let generation = 0;
    let stopped = false;
    let unsubscribe: (() => void) | null = null;
    let retry: ReturnType<typeof setTimeout> | null = null;
    /** msgId → events that arrived while its `msg.read` was in flight. */
    const reading = new Map<string, C9pMsgStateEvent[]>();
    /** Agent id → when a refresh was first asked for it. After
     * AGENT_GIVE_UP_MS the agent is left alone (a retired agent, or one
     * that never gets a session, cannot keep `agent.list` running). */
    const askedAgents = new Map<string, number>();
    let agentTimer: ReturnType<typeof setTimeout> | null = null;
    let lastAgentList = -Infinity;
    const publish = () => apply({ status, error, messages: new Map(messages), sessionKeys: new Map(sessionKeys) });

    function setAgents(agents: C9pAgentRow[]) {
        sessionKeys = new Map(agents.filter(a => a.sessionKey).map(a => [a.id, a.sessionKey as string]));
    }
    /** A live event named an agent with no session key yet: list again
     * (debounced, rate-limited, bounded per agent), so a new agent's card
     * gets its mail line once detection finds its session. */
    function noticeAgents(row: C9pMsgSummary, mine: number) {
        const ids = [row.to.id, row.from.kind === 'agent' ? agentIdOf(row.from.addr) : null];
        const at = now();
        let unresolved = false;
        for (const id of ids) {
            if (!id || sessionKeys.has(id)) continue;
            const first = askedAgents.get(id);
            if (first === undefined) askedAgents.set(id, at);
            else if (at - first > AGENT_GIVE_UP_MS) continue;
            unresolved = true;
        }
        if (!unresolved || agentTimer) return;
        const wait = Math.max(AGENT_DEBOUNCE_MS, lastAgentList + AGENT_REFRESH_MS - at);
        agentTimer = setTimeout(() => {
            agentTimer = null;
            if (stopped || mine !== generation) return;
            lastAgentList = now();
            t.request('agent.list', {}).then(r => {
                if (stopped || mine !== generation) return;
                setAgents(r.agents ?? []);
                publish();
            }, () => {});
        }, wait);
    }
    function readMissing(e: C9pMsgStateEvent, mine: number) {
        const waiting = reading.get(e.msgId);
        if (waiting) { waiting.push(e); return; }
        reading.set(e.msgId, [e]);
        const done = () => reading.delete(e.msgId);
        t.request('msg.read', { msgId: e.msgId }).then(r => {
            const events = reading.get(r.msgId) ?? [];
            done();
            if (stopped || mine !== generation) return;
            const { body: _body, attempts: _a, provenance: _p, status: _s, ...summary } = r;
            const old = messages.get(r.msgId);
            // An event may have made the row while the read was in flight.
            let row: C9pMsgSummary = old && old.stateAt >= summary.stateAt ? old : summary;
            // Events that came during the read: each applies only if not older.
            for (const ev of events) row = applyState(row, ev) ?? row;
            messages.set(r.msgId, row);
            noticeAgents(row, mine);
            publish();
        }, done);
    }
    function receive(frame: StreamFrame, mine: number) {
        if (!('type' in frame) || frame.type !== 'event' || !('topic' in frame) || frame.topic !== 'msg.state') return;
        const e = frame.data as C9pMsgStateEvent;
        if (!e || typeof e.msgId !== 'string' || typeof e.state !== 'string' || typeof e.at !== 'number') return;
        const row = applyState(messages.get(e.msgId), e);
        if (!row) { readMissing(e, mine); return; }
        if (row === messages.get(e.msgId)) return;
        messages.set(e.msgId, row);
        noticeAgents(row, mine);
        publish();
    }
    function fail(err: unknown, mine: number) {
        if (stopped || mine !== generation) return;
        generation++;
        unsubscribe?.(); unsubscribe = null;
        if (retry) clearTimeout(retry);
        retry = null;
        error = errorText(err);
        if (isUnavailableError(err)) { status = 'unavailable'; publish(); return; }
        // Keep the last rows on screen; read again later.
        status = 'error';
        publish();
        retry = setTimeout(() => { retry = null; void refresh(); }, RETRY_MS);
    }
    async function refresh() {
        const mine = ++generation;
        const startedAt = now();
        unsubscribe?.(); unsubscribe = null;
        try {
            // A build without mail denies this subscribe or the first list
            // with `not_allowed`; both end in `unavailable`.
            let ready = false;
            const buffered: StreamFrame[] = [];
            const stop = await t.subscribe({ topics: ['msg.state'] }, frame => {
                if (stopped || mine !== generation) return;
                if ('error' in frame && frame.error) { fail(frame.error, mine); return; }
                if (ready) receive(frame, mine); else buffered.push(frame);
            });
            if (stopped || mine !== generation) { stop(); return; }
            unsubscribe = stop;
            // Read the snapshot after the stream is open, so no change is lost.
            const items: C9pMsgSummary[] = [];
            let page = await t.request('msg.list', { limit: PAGE_LIMIT });
            for (let n = 1; ; n++) {
                if (stopped || mine !== generation) return;
                items.push(...page.items);
                if (!page.next || n >= MAX_PAGES) break;
                page = await t.request('msg.list', { limit: PAGE_LIMIT, before: page.next });
            }
            let agents: C9pAgentRow[] = [];
            lastAgentList = now();
            try { agents = (await t.request('agent.list', {})).agents ?? []; } catch { /* names only */ }
            if (stopped || mine !== generation) return;
            const next = new Map(items.map(m => [m.msgId, m]));
            // A row added while this read ran (an own send) stays.
            for (const [id, row] of messages) if (!next.has(id) && row.createdAt >= startedAt) next.set(id, row);
            messages = next;
            setAgents(agents);
            askedAgents.clear();
            status = 'ready'; error = null;
            ready = true;
            for (const frame of buffered.splice(0)) receive(frame, mine);
            publish();
        } catch (err) { fail(err, mine); }
    }
    publish();
    void refresh();
    return {
        stop() {
            stopped = true; generation++; unsubscribe?.(); unsubscribe = null;
            if (retry) clearTimeout(retry);
            if (agentTimer) clearTimeout(agentTimer);
        },
        upsert(row) {
            if (stopped) return;
            const old = messages.get(row.msgId);
            // Engine time orders states; a row never goes back to an older one.
            if (old && old.stateAt > row.stateAt) return;
            messages.set(row.msgId, old ? { ...old, ...row, createdAt: old.createdAt } : row);
            publish();
        }
    };
}

// ── Thread view model ────────────────────────────────────────────────────

export type MailSource = 'you' | 'terminal' | 'agent' | 'engine';
export interface MailThread {
    agentId: string;
    name: string;
    /** Oldest first. */
    messages: C9pMsgSummary[];
    latest: C9pMsgSummary;
    /** Mails that need a human look: `uncertain`, `failed` or held. */
    attention: number;
    queued: number;
}

export function isHeld(m: Pick<C9pMsgSummary, 'state' | 'detail'>): boolean {
    return !!m.detail?.startsWith('held:') && (m.state === 'delivering' || m.state === 'uncertain');
}
export function needsAttention(m: Pick<C9pMsgSummary, 'state' | 'detail'>): boolean {
    return m.state === 'uncertain' || m.state === 'failed' || isHeld(m);
}
export function sourceOf(m: Pick<C9pMsgSummary, 'msgId' | 'from'>, own: ReadonlySet<string>): MailSource {
    if (m.from.kind === 'agent') return 'agent';
    if (m.from.kind === 'engine') return 'engine';
    return own.has(m.msgId) ? 'you' : 'terminal';
}
/** Chip text for a state. */
export function stateLabel(m: Pick<C9pMsgSummary, 'state' | 'detail'>): string {
    if (isHeld(m)) return 'HELD';
    if (m.state === 'delivering') return 'SENDING';
    if (m.state === 'delivered') return '✓ DELIVERED';
    return m.state.toUpperCase();
}
export function routeLabel(via: string | null): string | null {
    if (via === 'socket') return 'PEER';
    if (via === 'paste') return 'PASTE';
    return via ? via.toUpperCase() : null;
}

/** The thread the MAIL tab shows: the focused agent when it has a thread,
 * else the newest thread. The tab pins this choice (it writes it to the
 * focus), so newer mail never moves the selection, or a draft, to another
 * agent. */
export function selectThread(threads: readonly Pick<MailThread, 'agentId'>[], focus: string | null): string | null {
    return focus && threads.some(t => t.agentId === focus) ? focus : threads[0]?.agentId ?? null;
}

/** One thread per agent: mail to it, and mail it sent. An agent-to-agent
 * mail shows in both agents' threads. Newest thread first. */
export function threadsOf(messages: Iterable<C9pMsgSummary>): MailThread[] {
    const byAgent = new Map<string, { name: string; messages: C9pMsgSummary[] }>();
    const add = (id: string, name: string | null, m: C9pMsgSummary) => {
        let t = byAgent.get(id);
        if (!t) { t = { name: name ?? id, messages: [] }; byAgent.set(id, t); }
        else if (name && t.name === id) t.name = name;
        t.messages.push(m);
    };
    for (const m of messages) {
        add(m.to.id, m.to.name, m);
        const from = m.from.kind === 'agent' ? agentIdOf(m.from.addr) : null;
        if (from && from !== m.to.id) add(from, m.from.name, m);
    }
    const threads: MailThread[] = [];
    for (const [agentId, t] of byAgent) {
        t.messages.sort((a, b) => a.createdAt - b.createdAt || (a.msgId < b.msgId ? -1 : 1));
        threads.push({
            agentId, name: t.name, messages: t.messages, latest: t.messages[t.messages.length - 1],
            attention: t.messages.filter(m => m.to.id === agentId && needsAttention(m)).length,
            queued: t.messages.filter(m => m.to.id === agentId && m.state === 'queued').length
        });
    }
    return threads.sort((a, b) => b.latest.createdAt - a.latest.createdAt);
}
