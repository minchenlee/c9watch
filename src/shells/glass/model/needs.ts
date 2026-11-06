/** The 需要你 queue (build plan §G2, Gap N). Derived from c9p facts:
 * `SessionView` NeedsAttention (approval → 允許, question → 決定, else 處理),
 * open `agent.attention` events (處理) and open `msg` requests sent to a
 * human (決定). 審 and 給 exist only in the demo gaps file. One item per member. */
import type { C9pAgentAttentionEvent, C9pAgentRow, C9pAgentTurnEndEvent, C9pMsgSummary } from '$lib/c9p-1';
import { agentIdOf, needsAttention as mailNeedsLook } from '$lib/data/mail-store';
import type { DemoNeed, NeedVerb } from './gaps-demo';
import type { Member } from './rooms';

/** What a later `agent.permission.answer` (D40, Gap P) needs to answer a
 * pending tool call. No action uses it yet. */
export type PermissionAsk = {
    sessionKey: string | null;
    agentId: string | null;
    toolName: string;
    toolInput: unknown;
    /** Always null today: no c9p fact ties a pending prompt to its
     * `tool_use_id` (`hook.permission_request` emits no attention; an
     * `agent.attention` toolUseId belongs to another call, such as an
     * auto-mode denial). The answer method must supply it. */
    toolUseId: null;
    /** The agent row's current generation; null without an agent row. */
    generation: number | null;
};

export type Need = {
    id: string;
    verb: NeedVerb;
    tag: string;
    title: string;
    memberKey: string;
    agentId: string | null;
    sessionKey: string | null;
    workspaceId: string;
    /** Unix ms the wait started (best estimate). */
    since: number;
    /** Agents this item blocks; null until a lead writes the list (items 31/32). */
    blocking: number | null;
    source: 'session' | 'attention' | 'mail' | 'demo';
    permission: PermissionAsk | null;
    msgId: string | null;
    /** This one request, not just this member: `id` stays the same for every
     * request of one member. A mail request is its `msgId`; an attention
     * event is its key and time; a session prompt is its session, agent
     * generation, verb and text. Handled marks, drafts and receipts use it. */
    instance: string;
};

/** `at`: the event's time. `receivedAt`: local time it arrived, compared
 * with `agentsAt` (both on this client's clock); `at` when absent. */
export type AttentionSeen = { event: C9pAgentAttentionEvent; at: number; receivedAt?: number };

/** One slot per agent, reason kind and mail: a newer event of the same kind
 * replaces the older one. */
export const attentionKey = (e: C9pAgentAttentionEvent) => `${e.id}|${e.reason.split(':')[0]}|${e.msgId ?? ''}`;

export type AttentionFacts = {
    agents: ReadonlyMap<string, C9pAgentRow>;
    mail: ReadonlyMap<string, C9pMsgSummary>;
    /** The last `agent.turn_end` per agent id. */
    turnEnds: ReadonlyMap<string, C9pAgentTurnEndEvent>;
    /** Local time the `agent.list` behind `agents` was asked; -Infinity if unknown. */
    agentsAt?: number;
};

/** The attention events still open. An event is cleared when:
 * - its agent is listed as gone, retired or failed;
 * - its agent's generation is newer than the event's (a restart);
 * - it names a mail (`mail_uncertain`, `mail_held`) and that mail is listed
 *   and no longer needs a look (not uncertain, failed or held);
 * - it is `mail_paused` and an agent row asked after the event says
 *   `mailPaused: false` (an older row cannot clear it: a pause from elsewhere
 *   sends no `agent.upsert`);
 * - it names no mail (`auto_mode_denied` and other hook reasons) and the
 *   agent ended a turn after it, in the same or a newer generation.
 * The detected status (working, waiting) never clears an event. */
export function openAttention(seen: AttentionSeen[], { agents, mail, turnEnds, agentsAt = -Infinity }: AttentionFacts): AttentionSeen[] {
    return seen.filter(({ event: e, at, receivedAt }) => {
        const a = agents.get(e.id);
        const gen = typeof e.generation === 'number' ? e.generation : null;
        if (a && (a.goneAt != null || a.state === 'retired' || a.state === 'failed')) return false;
        if (a && gen != null && a.generation > gen) return false;
        if (e.msgId) { const m = mail.get(e.msgId); return !m || mailNeedsLook(m); }
        if (e.reason === 'mail_paused') return !(a?.mailPaused === false && agentsAt > (receivedAt ?? at));
        const end = turnEnds.get(e.id);
        return !(end && end.at > at && (gen == null || end.generation >= gen));
    });
}

const TAG: Record<NeedVerb, string> = { 允許: '等你允許', 決定: '等你決定', 處理: '卡住', 給: '等你給', 審: '等你審' };
const ASK_TOOLS = new Set(['AskUserQuestion']);
const CLOSED_MAIL = new Set(['failed', 'rejected', 'expired', 'cancelled']);
const LOST_REPLY = new Set([...CLOSED_MAIL, 'uncertain']);

function toolTitle(name: string, input: unknown): string {
    const i = (input ?? {}) as Record<string, unknown>;
    for (const k of ['command', 'question', 'file_path', 'path', 'url']) if (typeof i[k] === 'string') return i[k] as string;
    return name;
}

export type NeedsInput = {
    members: Member[];
    workspaceOf: (m: Member) => string;
    /** Open events only (`openAttention`). */
    attention?: AttentionSeen[];
    mail?: C9pMsgSummary[];
    /** Demo only. Null in live mode, so no demo verb can show. */
    demo?: DemoNeed[] | null;
};

export function deriveNeeds({ members, workspaceOf, attention = [], mail = [], demo = null }: NeedsInput): Need[] {
    const byKey = new Map<string, Need>();
    const byAgent = new Map(members.filter(m => m.agentId).map(m => [m.agentId!, m]));
    const add = (m: Member, n: Omit<Need, 'id' | 'memberKey' | 'agentId' | 'sessionKey' | 'workspaceId' | 'blocking' | 'tag' | 'msgId' | 'instance'> & { msgId?: string | null; event?: string }) => {
        if (byKey.has(m.key)) return;
        const { event, ...rest } = n;
        const instance = n.msgId ? `mail:${n.msgId}` : event ? `event:${event}` : `${n.source}:${m.key}|g${m.agent?.generation ?? 0}|${n.verb}|${n.title}`;
        byKey.set(m.key, { id: `need:${m.key}`, memberKey: m.key, agentId: m.agentId, sessionKey: m.sessionKey, workspaceId: workspaceOf(m), blocking: null, tag: TAG[n.verb], msgId: null, ...rest, instance });
    };

    for (const m of members) {
        const s = m.session;
        if (!m.live || !s || s.status !== 'NeedsAttention') continue;
        const w = s.waitingFor;
        const tool = s.pendingToolName ?? (w?.kind === 'approval' ? w.tool : null);
        const since = Date.parse(s.modified) || m.seenAt;
        if (w?.kind === 'question' || (tool && ASK_TOOLS.has(tool))) {
            add(m, { verb: '決定', title: toolTitle(tool ?? '', s.pendingToolInput) || s.latestMessage, since, source: 'session', permission: null });
        } else if (tool) {
            const permission: PermissionAsk = { sessionKey: m.sessionKey, agentId: m.agentId, toolName: tool, toolInput: s.pendingToolInput, toolUseId: null, generation: m.agent?.generation ?? null };
            add(m, { verb: '允許', title: toolTitle(tool, s.pendingToolInput), since, source: 'session', permission });
        } else {
            add(m, { verb: '處理', title: s.latestMessage, since, source: 'session', permission: null });
        }
    }
    // Newest open event first, whatever the detected status says.
    for (const { event, at } of [...attention].sort((a, b) => b.at - a.at)) {
        const m = byAgent.get(event.id);
        if (m?.live) add(m, { verb: '處理', title: event.reason, since: at, source: 'attention', permission: null, msgId: event.msgId ?? null, event: `${attentionKey(event)}|g${event.generation ?? ''}@${at}` });
    }
    const humans = new Set(members.filter(m => m.role === 'human').map(m => m.agentId));
    // Answered: a mail whose `replyTo` names the request, sent by the side
    // that was asked (a human, or the recipient agent), and not lost. The
    // engine stores a reply with any kind (`info` by default), so the kind
    // does not count. Engine notices and failed or cancelled replies never count.
    const byId = new Map(mail.map(x => [x.msgId, x]));
    const answered = new Set(mail.filter(r => {
        const q = r.replyTo ? byId.get(r.replyTo) : undefined;
        return q && !LOST_REPLY.has(r.state) && r.from.kind !== 'engine'
            && (r.from.kind === 'human' || agentIdOf(r.from.addr) === q.to.id);
    }).map(r => r.replyTo));
    const named = (name: string | null) => { const hit = name ? members.filter(m => m.agentId && m.name === name) : []; return hit.length === 1 ? hit[0] : undefined; };
    for (const x of mail) {
        if (x.kind !== 'request' || !humans.has(x.to.id) || CLOSED_MAIL.has(x.state) || answered.has(x.msgId)) continue;
        // By address; a name only when there is no address and it is unique.
        const id = agentIdOf(x.from.addr);
        const from = id ? byAgent.get(id) : named(x.from.name);
        if (from?.live) add(from, { verb: '決定', title: x.preview, since: x.createdAt, source: 'mail', permission: null, msgId: x.msgId });
    }
    // Demo text wins over a derived verb for the same agent; the facts stay.
    for (const d of demo ?? []) {
        const m = byAgent.get(d.agentId);
        if (!m) continue;
        const had = byKey.get(m.key);
        byKey.delete(m.key);
        add(m, { verb: d.verb, title: d.title, since: had?.since ?? m.seenAt, source: had?.source ?? 'demo', permission: had?.permission ?? null, msgId: had?.msgId ?? null, event: had?.instance });
        byKey.get(m.key)!.tag = d.tag;
    }
    return [...byKey.values()];
}

/** redesign §8.12: one sort key, agents blocked, ties by wait. Blocked counts
 * are used only when some item has one; otherwise only the wait counts
 * (longest first). */
export function sortNeeds(items: Need[]): Need[] {
    const counted = items.some(n => n.blocking != null);
    return [...items].sort((a, b) =>
        (counted ? (b.blocking ?? 0) - (a.blocking ?? 0) : 0) || a.since - b.since || a.id.localeCompare(b.id));
}
