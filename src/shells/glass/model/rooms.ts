/** Members (one per agent or session) and the rows of each workspace.
 * D40: no goal core in this line. A row is a lead tree (`parentId`) or one
 * talk session; a demo goal only renames a lead row. Generic on purpose: no
 * floor or seat layout here (the office extension is paused). */
import type { C9pAgentRow, C9pSessionView } from '$lib/c9p-1';
import type { DemoGoal } from './gaps-demo';

export type Role = 'lead' | 'worker' | 'talk' | 'human';
export type MemberStatus = 'needs' | 'working' | 'waiting' | 'done' | 'unknown';

export type Member = {
    /** Agent id, or the session key for a session with no agent row. */
    key: string;
    agentId: string | null;
    sessionKey: string | null;
    name: string;
    role: Role;
    parentId: string | null;
    path: string;
    status: MemberStatus;
    live: boolean;
    /** `AgentRow.mailPaused` (c9p 1.15); false without an agent row. */
    mailPaused: boolean;
    /** Unix ms of the row's own last activity (`modified`, `lastSeen`). */
    seenAt: number;
    agent: C9pAgentRow | null;
    session: C9pSessionView | null;
};

export type Row = {
    id: string;
    workspaceId: string;
    kind: 'lead' | 'talk';
    /** The goal name (demo), else the lead's or the talk's name. */
    title: string;
    goalId: string | null;
    lead: Member | null;
    /** The lead first, then its tree, then the talk member. */
    members: Member[];
    /** Live children a lead may have, else null. */
    cap: number | null;
    freeSeats: number | null;
    live: boolean;
};

/** E5 (D38): at most 8 live children per parent (Gap S). */
export const CHILD_CAP = 8;

function sessionStatus(s: string): MemberStatus {
    if (s === 'NeedsAttention') return 'needs';
    if (s === 'Working' || s === 'Connecting') return 'working';
    if (s === 'WaitingForInput') return 'waiting';
    return 'unknown';
}
function agentStatus(a: C9pAgentRow): MemberStatus {
    if (a.goneAt != null || a.state === 'retired' || a.state === 'failed') return 'done';
    if (a.status === 'needs_attention') return 'needs';
    if (a.status === 'working' || a.status === 'connecting') return 'working';
    if (a.status === 'waiting_for_input') return 'waiting';
    return 'unknown';
}
const ms = (iso: string | undefined) => { const t = iso ? Date.parse(iso) : NaN; return Number.isFinite(t) ? t : 0; };
const base = (p: string) => p.split('/').filter(Boolean).pop() ?? p;

/** One member per agent row (joined to its session by `sessionKey`), plus one
 * talk member per session no agent claims (an observed session). */
export function buildMembers(sessions: C9pSessionView[], agents: C9pAgentRow[]): Member[] {
    const bySession = new Map(sessions.map(s => [s.sessionKey, s]));
    const parents = new Set(agents.map(a => a.parentId).filter(Boolean));
    const claimed = new Set<string>();
    const out: Member[] = agents.map(a => {
        const s = a.sessionKey ? bySession.get(a.sessionKey) ?? null : null;
        if (s) claimed.add(s.sessionKey);
        const done = agentStatus(a) === 'done';
        const role: Role = a.role === 'human' ? 'human' : a.role === 'orch' || parents.has(a.id) ? 'lead' : a.parentId ? 'worker' : 'talk';
        const path = a.cwd ?? s?.projectPath ?? '';
        return {
            key: a.id, agentId: a.id, sessionKey: a.sessionKey, role, parentId: a.parentId, path,
            name: a.name ?? s?.customTitle ?? s?.sessionName ?? base(path),
            status: done ? 'done' : s ? sessionStatus(s.status) : agentStatus(a), live: !done, mailPaused: a.mailPaused === true,
            seenAt: Math.max(ms(s?.modified), a.lastSeen ?? 0, a.goneAt ?? 0, a.createdAt ?? 0), agent: a, session: s
        };
    });
    for (const s of sessions) {
        if (claimed.has(s.sessionKey)) continue;
        out.push({
            key: s.sessionKey, agentId: null, sessionKey: s.sessionKey, role: 'talk', parentId: null, path: s.projectPath,
            name: s.customTitle ?? s.sessionName ?? base(s.projectPath), status: sessionStatus(s.status), live: true,
            mailPaused: false, seenAt: ms(s.modified), agent: null, session: s
        });
    }
    return out;
}

/** The top of a member's tree: walks `parentId` while the parent is known. */
export function treeRoot(m: Member, byId: Map<string, Member>): Member {
    let cur = m;
    const seen = new Set([m.key]);
    while (cur.parentId) {
        const up = byId.get(cur.parentId);
        if (!up || seen.has(up.key)) break;
        seen.add(up.key);
        cur = up;
    }
    return cur;
}

/** Rows per workspace, in member order. Humans are not on a row. A tree
 * that spans repos gives one row per workspace: each row holds the members
 * in that workspace and references the same lead (which may live in another
 * workspace). Cap and free seats are on the lead's own row only. */
export function buildRows(members: Member[], goals: DemoGoal[] | null, workspaceOf: (m: Member) => string): Row[] {
    const byId = new Map(members.filter(m => m.agentId).map(m => [m.agentId!, m]));
    const rows = new Map<string, Row>();
    for (const m of members) {
        if (m.role === 'human') continue;
        const root = treeRoot(m, byId);
        const ws = workspaceOf(m);
        const home = workspaceOf(root) === ws;
        // A worker whose lead is not listed still heads a lead row.
        const lead = root.role !== 'talk';
        const id = `${lead ? 'lead' : 'talk'}:${root.key}${home ? '' : `@${ws}`}`;
        let row = rows.get(id);
        if (!row) {
            const goal = lead ? goals?.find(g => g.leadId === root.agentId) ?? null : null;
            row = {
                id, workspaceId: ws, kind: lead ? 'lead' : 'talk', title: goal?.name ?? root.name, goalId: goal?.id ?? null,
                lead: lead ? root : null, members: [], cap: lead && home ? goal?.cap ?? CHILD_CAP : null, freeSeats: null, live: false
            };
            rows.set(id, row);
        }
        if (m === root) row.members.unshift(m); else row.members.push(m);
        row.live ||= m.live;
    }
    for (const row of rows.values()) {
        const lead = row.lead;
        if (lead && row.cap != null) row.freeSeats = Math.max(0, row.cap - members.filter(m => m.live && m.parentId === lead.agentId).length);
    }
    return [...rows.values()];
}
