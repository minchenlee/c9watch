/** 今天 page sections (redesign §8.13 D2, §8.14; build plan G10a). Pure:
 * `scripts/test-glass-pages.mjs` covers it.
 *
 * Order: 驗收完 → 被退回 → 需要留意 → 還在進行. 需要你 is one line on the
 * page (a link, no handling there). 「依你的規則自動做了」 stays hidden
 * until core keeps rules (Gap R).
 *
 * Live mode has no verifier (Gap V): the first section is 「今天完成」, the
 * sessions `history.list` finished since midnight, and 被退回 is empty.
 * The fixture's reviews come from the gaps file only while it is on. */
import type { C9pAwayActivity, C9pHistoryEntry } from '$lib/c9p-1';
import type { GlassModel, MemberView, RowView, WorkspaceView } from '../model';
import type { DemoReview } from '../model/gaps-demo';
import { historyTitle } from './workspace-extras';

export type TodayItem = { key: string; title: string; meta: string; workspaceId: string | null; at: number | null; atText: string | null };
export type ActiveRow = { key: string; workspaceId: string; title: string; working: number; away: AwaySum | null };
export type AwaySum = { filesEdited: number; commandsRun: number; toolCalls: number; subagentsCompleted: number; partial: boolean };
export type TodaySections = {
    /** 「驗收完」 with the fixture, else 「今天完成」. */
    doneTitle: string;
    done: TodayItem[];
    rejected: TodayItem[];
    /** 沒回應: a working member with no change for 20 minutes (G2 stale). */
    attention: TodayItem[];
    active: ActiveRow[];
};

export type TodayInput = {
    model: GlassModel;
    /** `history.list` rows; null while loading or after an error. */
    history: C9pHistoryEntry[] | null;
    away: Record<string, C9pAwayActivity>;
    /** Local midnight, ms. */
    midnight: number;
    now: number;
    /** Gaps-file reviews, only while the fixture is on. */
    demo: { accepted: DemoReview[]; rejected: DemoReview[] } | null;
};

const wsName = (list: WorkspaceView[], id: string) => list.find(w => w.id === id)?.name ?? id;
const minutes = (ms: number) => Math.max(1, Math.round(ms / 60_000));

/** The sessions `session.away` should be asked about: live members of rows
 * with work going on, since midnight (or the session start, if later). */
export function awayRequests(model: GlassModel, midnight: number): { sessionKey: string; sinceMs: number }[] {
    const out = new Map<string, number>();
    for (const row of activeRows(model)) for (const m of row.members) {
        if (!m.live || !m.sessionKey) continue;
        const started = m.session?.startedAtMs ?? m.agent?.startedAt ?? midnight;
        out.set(m.sessionKey, Math.max(midnight, Number.isFinite(started) ? started : midnight));
    }
    return [...out].map(([sessionKey, sinceMs]) => ({ sessionKey, sinceMs }));
}

const working = (m: MemberView) => m.live && m.status === 'working' && !m.stale;
function activeRows(model: GlassModel): RowView[] {
    return model.workspaces.flatMap(w => w.rows).filter(r => r.members.some(working));
}

function sumAway(members: MemberView[], away: Record<string, C9pAwayActivity>): AwaySum | null {
    const hits = members.map(m => (m.sessionKey ? away[m.sessionKey] : undefined)).filter((a): a is C9pAwayActivity => !!a);
    if (!hits.length) return null;
    const s = (f: (a: C9pAwayActivity) => number) => hits.reduce((n, a) => n + f(a), 0);
    return { filesEdited: s(a => a.filesEdited), commandsRun: s(a => a.commandsRun), toolCalls: s(a => a.toolCalls), subagentsCompleted: s(a => a.subagentsCompleted), partial: hits.some(a => a.partial) };
}

export function todaySections({ model, history, away, midnight, now, demo }: TodayInput): TodaySections {
    const review = (r: DemoReview, i: number): TodayItem => ({
        key: `${r.workspace}:${r.title}:${i}`, title: r.title, meta: `${wsName(model.workspaces, r.workspace)} › ${r.goal} · ${r.note}`,
        workspaceId: r.workspace, at: null, atText: r.at
    });
    const liveIds = new Set(model.workspaces.flatMap(w => w.rows.flatMap(r => r.members)).filter(m => m.live && m.session).map(m => m.session!.id));
    const done = demo
        ? demo.accepted.map(review)
        : (history ?? [])
            .filter(e => e.timestamp >= midnight && e.timestamp <= now && !liveIds.has(e.sessionId))
            .sort((a, b) => b.timestamp - a.timestamp)
            .map((e): TodayItem => ({ key: `${e.provider}:${e.sessionId}`, title: historyTitle(e), meta: e.projectName || e.project, workspaceId: null, at: e.timestamp, atText: null }));
    const attention = model.workspaces.flatMap(w => w.rows.flatMap(r => r.members.filter(m => m.live && m.stale).map((m): TodayItem => ({
        key: m.key, title: m.name, meta: `沒回應 · ${minutes(now - m.lastChange)} 分鐘沒有動靜 · ${w.name} › ${r.title}`,
        workspaceId: w.id, at: m.lastChange, atText: null
    })))).sort((a, b) => (a.at ?? 0) - (b.at ?? 0));
    const active = model.workspaces.flatMap(w => w.rows.filter(r => r.members.some(working)).map((r): ActiveRow => ({
        key: r.id, workspaceId: w.id, title: `${w.name} › ${r.title}`, working: r.members.filter(working).length, away: sumAway(r.members.filter(m => m.live), away)
    })));
    return { doneTitle: demo ? '驗收完' : '今天完成', done, rejected: demo ? demo.rejected.map(review) : [], attention, active };
}

/** 「今天：改了 3 個檔案 · 跑了 5 個指令」. `partial`: the counts are a lower bound. */
export function awayText(a: AwaySum | null): string {
    if (!a) return '';
    const parts = [
        a.filesEdited ? `改了 ${a.filesEdited} 個檔案` : '',
        a.commandsRun ? `跑了 ${a.commandsRun} 個指令` : '',
        a.subagentsCompleted ? `完成 ${a.subagentsCompleted} 個 subagent` : ''
    ].filter(Boolean);
    if (!parts.length) return a.toolCalls ? `今天：${a.toolCalls} 次工具呼叫` : '';
    return `今天${a.partial ? '至少' : ''}：${parts.join(' · ')}`;
}

/** Local midnight of `now`. */
export function localMidnight(now: number): number {
    const d = new Date(now);
    return new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
}
