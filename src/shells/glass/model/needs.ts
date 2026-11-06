/** The 需要你 queue (build plan §G2, Gap N). Derived from c9p facts:
 * `SessionView` NeedsAttention (approval → 允許, question → 決定, else 處理),
 * `agent.attention` events and open `msg` requests sent to a human (決定).
 * 審 and 給 exist only in the demo gaps file. One item per member. */
import type { C9pAgentAttentionEvent, C9pMsgSummary } from '$lib/c9p-1';
import type { DemoNeed, NeedVerb } from './gaps-demo';
import type { Member } from './rooms';

/** What a later `agent.permission.answer` (D40, Gap P) needs to answer a
 * pending tool call. No action uses it yet. */
export type PermissionAsk = {
    sessionKey: string | null;
    agentId: string | null;
    toolName: string;
    toolInput: unknown;
    /** From `agent.attention` when a hook raised it; null otherwise. */
    toolUseId: unknown;
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
};

export type AttentionSeen = { event: C9pAgentAttentionEvent; at: number };

const TAG: Record<NeedVerb, string> = { 允許: '等你允許', 決定: '等你決定', 處理: '卡住', 給: '等你給', 審: '等你審' };
const ASK_TOOLS = new Set(['AskUserQuestion']);
const CLOSED_MAIL = new Set(['failed', 'rejected', 'expired', 'cancelled']);

function toolTitle(name: string, input: unknown): string {
    const i = (input ?? {}) as Record<string, unknown>;
    for (const k of ['command', 'question', 'file_path', 'path', 'url']) if (typeof i[k] === 'string') return i[k] as string;
    return name;
}

export type NeedsInput = {
    members: Member[];
    workspaceOf: (m: Member) => string;
    attention?: AttentionSeen[];
    mail?: C9pMsgSummary[];
    /** Demo only. Null in live mode, so no demo verb can show. */
    demo?: DemoNeed[] | null;
};

export function deriveNeeds({ members, workspaceOf, attention = [], mail = [], demo = null }: NeedsInput): Need[] {
    const byKey = new Map<string, Need>();
    const byAgent = new Map(members.filter(m => m.agentId).map(m => [m.agentId!, m]));
    const add = (m: Member, n: Omit<Need, 'id' | 'memberKey' | 'agentId' | 'sessionKey' | 'workspaceId' | 'blocking' | 'tag' | 'msgId'> & { msgId?: string | null }) => {
        if (byKey.has(m.key)) return;
        byKey.set(m.key, { id: `need:${m.key}`, memberKey: m.key, agentId: m.agentId, sessionKey: m.sessionKey, workspaceId: workspaceOf(m), blocking: null, tag: TAG[n.verb], msgId: null, ...n });
    };
    const latest = new Map<string, AttentionSeen>();
    for (const a of attention) if ((latest.get(a.event.id)?.at ?? -Infinity) <= a.at) latest.set(a.event.id, a);

    for (const m of members) {
        const s = m.session;
        if (!m.live || !s || s.status !== 'NeedsAttention') continue;
        const hook = m.agentId ? latest.get(m.agentId) : undefined;
        const w = s.waitingFor;
        const tool = s.pendingToolName ?? (w?.kind === 'approval' ? w.tool : null);
        const since = Date.parse(s.modified) || m.seenAt;
        if (w?.kind === 'question' || (tool && ASK_TOOLS.has(tool))) {
            add(m, { verb: '決定', title: toolTitle(tool ?? '', s.pendingToolInput) || s.latestMessage, since, source: 'session', permission: null });
        } else if (tool) {
            const permission = { sessionKey: m.sessionKey, agentId: m.agentId, toolName: tool, toolInput: s.pendingToolInput, toolUseId: hook?.event.toolUseId ?? null, generation: hook?.event.generation ?? null };
            add(m, { verb: '允許', title: toolTitle(tool, s.pendingToolInput), since, source: 'session', permission });
        } else {
            add(m, { verb: '處理', title: hook?.event.reason ?? s.latestMessage, since, source: 'session', permission: null });
        }
    }
    // An attention event counts only while its agent still needs you.
    for (const { event, at } of latest.values()) {
        const m = byAgent.get(event.id);
        if (m?.live && m.agent?.status === 'needs_attention') add(m, { verb: '處理', title: event.reason, since: at, source: 'attention', permission: null });
    }
    const humans = new Set(members.filter(m => m.role === 'human').map(m => m.agentId));
    const answered = new Set(mail.map(x => x.replyTo).filter(Boolean));
    for (const x of mail) {
        if (x.kind !== 'request' || !humans.has(x.to.id) || CLOSED_MAIL.has(x.state) || answered.has(x.msgId)) continue;
        const from = members.find(m => m.agentId && (x.from.addr?.endsWith(`/${m.agentId}`) || x.from.name === m.name));
        if (from) add(from, { verb: '決定', title: x.preview, since: x.createdAt, source: 'mail', permission: null, msgId: x.msgId });
    }
    // Demo text wins over a derived verb for the same agent; the facts stay.
    for (const d of demo ?? []) {
        const m = byAgent.get(d.agentId);
        if (!m) continue;
        const had = byKey.get(m.key);
        byKey.delete(m.key);
        add(m, { verb: d.verb, title: d.title, since: had?.since ?? m.seenAt, source: had?.source ?? 'demo', permission: had?.permission ?? null, msgId: had?.msgId ?? null });
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
