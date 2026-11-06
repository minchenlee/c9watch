/** Workspace page data beyond the model (redesign §8.3): 已完成
 * (`history.list`), 成本 (`cost.get`) and 記憶與指示 (`memory.list`), each
 * filtered to one workspace by repo root. Pure, except `loadExtras`.
 * `scripts/test-glass-ui.mjs` covers the filters. */
import type { C9pCostData, C9pHistoryEntry, C9pMemoryFile, C9pProjectMemory, C9pSessionCostRecord } from '$lib/c9p-1';
import { request } from '$lib/data/client';
import { repoRoot } from '../model/workspaces';
import type { DemoGoal, DemoRule } from '../model/gaps-demo';

export type Extras = { history: C9pHistoryEntry[]; cost: C9pCostData | null; memory: C9pProjectMemory[] };
/** Per section: the data, or the error that kept it away. */
export type Loaded<T> = { ok: true; value: T } | { ok: false; error: unknown };

const settle = <T>(p: Promise<T>): Promise<Loaded<T>> => p.then(value => ({ ok: true as const, value }), error => ({ ok: false as const, error }));

/** Most pages one source is read for. */
export const MAX_PAGES = 10;

/** Follows `nextOffset` from 0 and joins the pages. Stops at the last page,
 * after `MAX_PAGES`, or when the cursor does not move forward (the rows read
 * so far are kept). The core caps a page by size too, so one request never
 * proves that every row came back. */
export async function readPages<T>(fetch: (offset: number) => Promise<{ items: T[]; nextOffset?: number | null }>, max = MAX_PAGES): Promise<T[]> {
    const out: T[] = [];
    let offset = 0;
    for (let page = 0; page < max; page++) {
        const r = await fetch(offset);
        out.push(...r.items);
        const next = r.nextOffset;
        if (next == null || !Number.isFinite(next) || next <= offset) break;
        offset = next;
    }
    return out;
}

/** One read of each source; history and memory follow their pages. */
export async function loadExtras(): Promise<{ history: Loaded<C9pHistoryEntry[]>; cost: Loaded<C9pCostData | null>; memory: Loaded<C9pProjectMemory[]> }> {
    const [history, cost, memory] = await Promise.all([
        settle(readPages(async offset => { const r = await request('history.list', { page: { offset, limit: 1000 } }); return { items: (r?.entries ?? []) as C9pHistoryEntry[], nextOffset: r?.nextOffset }; })),
        settle(request('cost.get', {}).then(r => (r ?? null) as C9pCostData | null)),
        settle(readPages(async offset => { const r = await request('memory.list', { page: { offset, limit: 1000 } }); return { items: (r?.projects ?? []) as C9pProjectMemory[], nextOffset: r?.nextOffset }; }))
    ]);
    return { history, cost, memory };
}

const inWs = (path: string | null | undefined, root: string) => !!path && repoRoot(path) === root;

/** Past sessions of this workspace, newest first. `live` holds the session
 * ids still on a card under 進行中; they are not done yet. */
export function historyFor(entries: C9pHistoryEntry[], root: string, live: ReadonlySet<string> = new Set()): C9pHistoryEntry[] {
    return entries.filter(e => inWs(e.project, root) && !live.has(e.sessionId)).sort((a, b) => b.timestamp - a.timestamp);
}

/** Splits `a \`code\` b` into text and code parts (demo rule texts). */
export function codeParts(text: string): { code: boolean; text: string }[] {
    return text.split('`').map((t, i) => ({ code: i % 2 === 1, text: t })).filter(p => p.text);
}

export function historyTitle(e: C9pHistoryEntry): string {
    return e.customTitle || e.aiTitle || e.codexTitle || e.cursorTitle || e.display || e.sessionId;
}

/** Tokens of one record; `totalTokens` when the core sent it. */
export const tokensOf = (r: C9pSessionCostRecord) => Number.isFinite(r.totalTokens) ? r.totalTokens : r.inputTokens + r.outputTokens + r.cachedInputTokens + r.reasoningOutputTokens;

/** How many records behind a total have a USD price (`costAvailable`).
 * `none`: no estimate at all; `some`: the cost is the priced subtotal only. */
export type Pricing = 'all' | 'some' | 'none';
export type ModelSpend = { model: string; tokens: number; cost: number; pricing: Pricing };
export type WorkspaceSpend = {
    /** Tokens of this workspace: today, since the 1st, all records. */
    today: number;
    month: number;
    total: number;
    /** ≈ API price of the same records (not what was paid). */
    todayCost: number;
    monthCost: number;
    todayPricing: Pricing;
    monthPricing: Pricing;
    /** This workspace's share of all tokens since the 1st, 0–100; null without data. */
    monthShare: number | null;
    byModel: ModelSpend[];
};

/** `today` is `YYYY-MM-DD`; the month is its first 7 characters. */
export function spendFor(cost: C9pCostData | null, root: string, today: string): WorkspaceSpend | null {
    if (!cost) return null;
    const all = cost.projectCosts.flatMap(p => p.sessions.map(r => ({ r, project: r.project || p.project })));
    const mine = all.filter(x => inWs(x.project, root)).map(x => x.r);
    if (!mine.length) return null;
    const month = today.slice(0, 7);
    const sum = (rs: C9pSessionCostRecord[], f: (r: C9pSessionCostRecord) => number) => rs.reduce((a, r) => a + f(r), 0);
    const todays = mine.filter(r => r.date === today);
    const months = mine.filter(r => r.date.startsWith(month));
    const allMonth = sum(all.map(x => x.r).filter(r => r.date.startsWith(month)), tokensOf);
    const byModel = new Map<string, C9pSessionCostRecord[]>();
    for (const r of mine) byModel.set(r.model, [...(byModel.get(r.model) ?? []), r]);
    const priced = (rs: C9pSessionCostRecord[]) => rs.filter(r => r.costAvailable);
    return {
        today: sum(todays, tokensOf), month: sum(months, tokensOf), total: sum(mine, tokensOf),
        todayCost: sum(priced(todays), r => r.cost), monthCost: sum(priced(months), r => r.cost),
        todayPricing: pricingOf(todays), monthPricing: pricingOf(months),
        monthShare: allMonth > 0 ? (sum(months, tokensOf) / allMonth) * 100 : null,
        byModel: [...byModel].map(([model, rs]): ModelSpend => ({ model, tokens: sum(rs, tokensOf), cost: sum(priced(rs), r => r.cost), pricing: pricingOf(rs) }))
            .sort((a, b) => b.tokens - a.tokens || a.model.localeCompare(b.model))
    };
}

/** No records count as `all`: there is nothing unpriced. */
export function pricingOf(rs: C9pSessionCostRecord[]): Pricing {
    const n = rs.filter(r => r.costAvailable).length;
    return n === rs.length ? 'all' : n === 0 ? 'none' : 'some';
}

/** The estimate text for one total. */
export function priceLabel(cost: number, pricing: Pricing, usd: (n: number) => string): string {
    if (pricing === 'none') return '沒有價格';
    return pricing === 'some' ? `≈ API 價 ${usd(cost)}（只算有價格的部分）` : `≈ API 價 ${usd(cost)}`;
}

export function localDay(t: number): string {
    const d = new Date(t);
    return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
}

/** Memory files of this workspace (the repo and its worktrees). */
export function memoryFor(projects: C9pProjectMemory[], root: string): (C9pMemoryFile & { projectPath: string; dir: string })[] {
    return projects
        .filter(p => inWs(p.projectPath, root))
        .flatMap(p => p.files.map(f => ({ ...f, projectPath: p.projectPath, dir: p.memoryDirPath })));
}

/** Demo rules that apply to this workspace: scope is the workspace name,
 * a `NAME › …` row, or the name of one of its goals. Fixture only. */
export function rulesFor(rules: DemoRule[], ws: { id: string; name: string }, goals: DemoGoal[]): DemoRule[] {
    const goalNames = new Set(goals.filter(g => g.workspace === ws.id).map(g => g.name));
    return rules.filter(r => r.scope === ws.name || r.scope.startsWith(`${ws.name} ›`) || goalNames.has(r.scope));
}
