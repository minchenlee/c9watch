/** 用量 page math (redesign §8.4, §8.14; build plan G10a). Pure:
 * `scripts/test-glass-pages.mjs` covers it.
 *
 * - Periods are local calendar days (`YYYY-MM-DD`, the cost records' `date`).
 * - 實付 is the owner's monthly fee (a host pref), spread over the days of
 *   each month. A row's 分攤 is its token share of the period's 實付.
 * - ≈ API 價 counts only records with a price (`costAvailable`). A total with
 *   no priced record has no price: it is never shown as $0 (G3 lesson). */
import type { C9pCostData, C9pSessionCostRecord } from '$lib/c9p-1';
import type { DemoWorkspace } from '../model/gaps-demo';
import { workspaceFor } from '../model/workspaces';
import { pricingOf, tokensOf, type Pricing } from './workspace-extras';

export type Period = 'today' | '7d' | '30d' | 'month' | 'lastMonth';
export const PERIODS: { id: Period; label: string }[] = [
    { id: 'today', label: '今天' },
    { id: '7d', label: '7 天' },
    { id: '30d', label: '30 天' },
    { id: 'month', label: '本月' },
    { id: 'lastMonth', label: '上個月' }
];
export const isPeriod = (v: unknown): v is Period => PERIODS.some(p => p.id === v);

/** Both ends included, oldest first. */
export type Range = { from: string; to: string; days: string[] };

const DAY_MS = 86_400_000;
/** `YYYY-MM-DD` → ms at UTC midnight. Day math in UTC has no DST gaps. */
function utc(day: string): number {
    const [y, m, d] = day.split('-').map(Number);
    return Date.UTC(y, m - 1, d);
}
const dayOf = (ms: number) => new Date(ms).toISOString().slice(0, 10);

/** `day` moved by `n` calendar days. */
export function shiftDay(day: string, n: number): string {
    return dayOf(utc(day) + n * DAY_MS);
}
export function daysInMonth(day: string): number {
    const [y, m] = day.split('-').map(Number);
    return new Date(Date.UTC(y, m, 0)).getUTCDate();
}

/** The days of a period that ends on `today` (本月 and 上個月 are calendar months). */
export function rangeFor(period: Period, today: string): Range {
    let from = today, to = today;
    if (period === '7d') from = shiftDay(today, -6);
    else if (period === '30d') from = shiftDay(today, -29);
    else if (period === 'month') from = `${today.slice(0, 7)}-01`;
    else if (period === 'lastMonth') {
        to = shiftDay(`${today.slice(0, 7)}-01`, -1);
        from = `${to.slice(0, 7)}-01`;
    }
    const days: string[] = [];
    for (let t = utc(from); t <= utc(to); t += DAY_MS) days.push(dayOf(t));
    return { from, to, days };
}

/** The monthly fee spread over the days: each day is fee / days of its month.
 * null without a fee. */
export function paidFor(range: Range, fee: number | null): number | null {
    if (fee === null || !Number.isFinite(fee) || fee < 0) return null;
    // Count days per month first, so a whole month is exactly the fee.
    const perMonth = new Map<string, number>();
    for (const d of range.days) perMonth.set(d.slice(0, 7), (perMonth.get(d.slice(0, 7)) ?? 0) + 1);
    let paid = 0;
    for (const [month, n] of perMonth) paid += (fee * n) / daysInMonth(`${month}-01`);
    return paid;
}

export type UsageRow = {
    key: string;
    label: string;
    /** Workspace rows link to their page. */
    workspaceId?: string;
    tokens: number;
    /** ≈ API price of the priced records only. */
    apiCost: number;
    pricing: Pricing;
    /** Token share of 實付; null without a fee. */
    paid: number | null;
};
export type DayBar = { date: string; tokens: number; apiCost: number; pricing: Pricing };
export type UsageSummary = {
    range: Range;
    tokens: number;
    unpricedTokens: number;
    apiCost: number;
    pricing: Pricing;
    paid: number | null;
    daily: DayBar[];
    byWorkspace: UsageRow[];
    byModel: UsageRow[];
};

/** Every record once: `dailyCosts` holds each record under its date. */
function recordsIn(cost: C9pCostData | null, range: Range): C9pSessionCostRecord[] {
    if (!cost) return [];
    return cost.dailyCosts.flatMap(d => d.sessions).filter(r => r.date >= range.from && r.date <= range.to);
}

const sum = (rs: C9pSessionCostRecord[], f: (r: C9pSessionCostRecord) => number) => rs.reduce((a, r) => a + f(r), 0);
const priced = (rs: C9pSessionCostRecord[]) => sum(rs.filter(r => r.costAvailable), r => r.cost);

/** `demo` is the gaps file's workspace list, only while the fixture is on:
 * it keeps the demo names. Live rows are named by their repo folder. */
export function summarize(cost: C9pCostData | null, range: Range, fee: number | null, demo: DemoWorkspace[] | null): UsageSummary {
    const rs = recordsIn(cost, range);
    const tokens = sum(rs, tokensOf);
    const paid = paidFor(range, fee);
    const share = (t: number) => (paid === null ? null : tokens > 0 ? (paid * t) / tokens : 0);
    const row = (key: string, label: string, group: C9pSessionCostRecord[], workspaceId?: string): UsageRow => {
        const t = sum(group, tokensOf);
        return { key, label, workspaceId, tokens: t, apiCost: priced(group), pricing: pricingOf(group), paid: share(t) };
    };
    const groupBy = (key: (r: C9pSessionCostRecord) => string) => {
        const m = new Map<string, C9pSessionCostRecord[]>();
        for (const r of rs) m.set(key(r), [...(m.get(key(r)) ?? []), r]);
        return m;
    };
    const byTokens = (a: UsageRow, b: UsageRow) => b.tokens - a.tokens || a.label.localeCompare(b.label);
    const wsOf = (r: C9pSessionCostRecord) => workspaceFor(r.project, demo);
    const byWorkspace = [...groupBy(r => wsOf(r).id)].map(([id, group]) => row(id, wsOf(group[0]).name, group, id)).sort(byTokens);
    const byModel = [...groupBy(r => r.model || '（沒有模型名稱）')].map(([model, group]) => row(model, model, group)).sort(byTokens);
    const perDay = groupBy(r => r.date);
    const daily = range.days.map((date): DayBar => {
        const group = perDay.get(date) ?? [];
        return { date, tokens: sum(group, tokensOf), apiCost: priced(group), pricing: pricingOf(group) };
    });
    return {
        range, tokens, unpricedTokens: sum(rs.filter(r => !r.costAvailable), tokensOf),
        apiCost: priced(rs), pricing: pricingOf(rs), paid, daily, byWorkspace, byModel
    };
}

/** The ≈ API price text. No priced record: 「沒有價格」, never $0. */
export function apiPriceText(cost: number, pricing: Pricing, usd: (n: number) => string): string {
    if (pricing === 'none') return '沒有價格';
    return pricing === 'some' ? `${usd(cost)}（部分）` : usd(cost);
}

/** Demo agent hours (Gap: core has no agent work time) in a period. */
export function hoursIn(range: Range, today: string, perDay: { days: number; hours: number }[]): number {
    return perDay.filter(h => range.days.includes(shiftDay(today, -h.days))).reduce((a, h) => a + h.hours, 0);
}

/** Provider window labels in the page's language. Unknown labels stay as sent. */
export function windowLabel(label: string): string {
    const l = label.trim().toLowerCase();
    if (l === '5-hour' || l === '5h') return '5 小時';
    if (l === 'weekly' || l === '7-day') return '每週';
    if (l === 'monthly') return '每月';
    return label;
}

/** A window at or above this percent is orange (redesign §8.4). */
export const QUOTA_WARN = 90;
