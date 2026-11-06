/** c9p → glass view model. Pure: the caller passes the rows, the clock and
 * the last change log, and keeps the change log it gets back.
 * `gaps` is the demo gaps file and only the glass fixture may pass it; in
 * live mode it is null, so no demo goal, name, verb or verifier can show. */
import type { C9pAgentRow, C9pMsgSummary, C9pSessionView } from '$lib/c9p-1';
import type { DemoGoal, DemoNeed, DemoWorkspace } from './gaps-demo';
import { workspaceFor, type Workspace } from './workspaces';
import { buildMembers, buildRows, type Member, type Row } from './rooms';
import { deriveNeeds, sortNeeds, type AttentionSeen, type Need } from './needs';
import { isStale, trackChanges, type ChangeLog } from './stale';

export type Gaps = { workspaces: DemoWorkspace[]; goals: DemoGoal[]; needs: DemoNeed[] };
export type MemberView = Member & { workspaceId: string; lastChange: number; stale: boolean };
export type RowView = Omit<Row, 'lead' | 'members'> & { lead: MemberView | null; members: MemberView[] };
export type WorkspaceView = Workspace & { rows: RowView[] };
export type GlassModel = { workspaces: WorkspaceView[]; needs: Need[]; changes: ChangeLog };

export type ModelInput = {
    sessions: C9pSessionView[];
    agents: C9pAgentRow[];
    attention?: AttentionSeen[];
    mail?: C9pMsgSummary[];
    gaps?: Gaps | null;
    changes?: ChangeLog;
    now: number;
};

const sig = (m: Member) => `${m.status}|${m.session?.waitingFor?.kind ?? ''}|${m.session?.pendingToolName ?? ''}|${m.agent?.state ?? ''}`;

export function deriveGlassModel({ sessions, agents, attention, mail, gaps = null, changes = new Map(), now }: ModelInput): GlassModel {
    const members = buildMembers(sessions, agents);
    const nextChanges = trackChanges(changes, members.map(m => ({ key: m.key, sig: sig(m), seenAt: m.seenAt })), now);
    const wsByRoot = new Map<string, Workspace>();
    const wsOf = (m: Member) => {
        const w = workspaceFor(m.path, gaps?.workspaces ?? null);
        if (!wsByRoot.has(w.root)) wsByRoot.set(w.root, w);
        return wsByRoot.get(w.root)!;
    };
    const views = new Map<Member, MemberView>(members.map(m => {
        const lastChange = nextChanges.get(m.key)!.at;
        return [m, { ...m, workspaceId: wsOf(m).id, lastChange, stale: m.live && isStale(m.status === 'working', lastChange, now) }];
    }));
    const rowsByWs = new Map<string, RowView[]>();
    for (const row of buildRows(members, gaps?.goals ?? null).values()) {
        const lead = row.lead ? views.get(row.lead)! : null;
        const first = views.get(row.members[0])!;
        const list = rowsByWs.get(first.workspaceId) ?? [];
        list.push({ ...row, lead, members: row.members.map(m => views.get(m)!) });
        rowsByWs.set(first.workspaceId, list);
    }
    const order: string[] = gaps?.workspaces.map(w => w.id) ?? [];
    const rank = (w: Workspace) => { const i = order.indexOf(w.id); return i < 0 ? order.length : i; };
    const workspaces = [...wsByRoot.values()]
        .filter(w => rowsByWs.has(w.id))
        .sort((a, b) => rank(a) - rank(b) || a.name.localeCompare(b.name) || a.root.localeCompare(b.root))
        .map(w => ({ ...w, rows: rowsByWs.get(w.id)! }));
    const needs = sortNeeds(deriveNeeds({ members, workspaceOf: m => views.get(m)!.workspaceId, attention, mail, demo: gaps?.needs ?? null }));
    return { workspaces, needs, changes: nextChanges };
}
