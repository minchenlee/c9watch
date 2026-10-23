import type { CostMode } from './cost-utils';
import type { DailyCost, SessionCostRecord, SessionProvider } from './types';

export interface CostSummary {
	cost: number;
	tokens: number;
	unpricedTokens: number;
}

/** Missing availability is tolerated for older Claude payloads, but never prices Codex implicitly. */
export function isCostAvailable(session: SessionCostRecord): boolean {
	return session.costAvailable ?? (session.provider !== 'codex' && session.provider !== 'cursor');
}

export function summarizeCostSessions(sessions: SessionCostRecord[]): CostSummary {
	return sessions.reduce<CostSummary>((summary, session) => {
		summary.cost += isCostAvailable(session) ? session.cost : 0;
		summary.tokens += session.totalTokens || 0;
		if (!isCostAvailable(session)) summary.unpricedTokens += session.totalTokens || 0;
		return summary;
	}, { cost: 0, tokens: 0, unpricedTokens: 0 });
}

/** Sum per-day summaries in day order (keeps the float order of the window totals). */
export function summarizeCostDays(days: DailyCost[]): CostSummary {
	return days.reduce<CostSummary>((total, day) => {
		const summary = summarizeCostSessions(day.sessions);
		total.cost += summary.cost;
		total.tokens += summary.tokens;
		total.unpricedTokens += summary.unpricedTokens;
		return total;
	}, { cost: 0, tokens: 0, unpricedTokens: 0 });
}

/**
 * Provider series of the COST charts, in stacking order. `provider` is the
 * SessionProvider id; `type` is the CSS class of the bar block and legend swatch.
 * `alwaysInLegend` series show in the legend even without usage.
 */
export const PROVIDER_SERIES = [
	{ provider: 'claudeCode', type: 'claude', label: 'Claude Code', color: 'var(--accent-amber)', alwaysInLegend: true },
	{ provider: 'codex', type: 'codex', label: 'Codex', color: 'var(--accent-blue)', alwaysInLegend: true },
	{ provider: 'cursor', type: 'cursor', label: 'Cursor', color: 'var(--accent-purple)', alwaysInLegend: false },
	{ provider: 'pi', type: 'pi', label: 'Pi', color: 'var(--accent-green)', alwaysInLegend: false }
] as const satisfies ReadonlyArray<{ provider: SessionProvider; type: string; label: string; color: string; alwaysInLegend: boolean }>;

/** Vertical gradient that stacks each series by its share. `values` follow PROVIDER_SERIES order. */
export function providerSeriesGradient(values: readonly number[]): string {
	const total = values.reduce((sum, value) => sum + value, 0);
	let offset = 0;
	const stops = PROVIDER_SERIES.map(({ color }, index) => {
		const value = values[index];
		const start = total > 0 ? (offset / total) * 100 : 0;
		offset += value;
		const end = total > 0 ? (offset / total) * 100 : 100;
		return `${color} ${start}% ${end}%`;
	});
	return `linear-gradient(to top, ${stops.join(', ')})`;
}

/** "Claude Code $1.00 (50%), Codex ..." for aria labels. `values` follow PROVIDER_SERIES order. */
export function providerSeriesBreakdown(values: readonly number[], format: (value: number) => string): string {
	const total = values.reduce((sum, value) => sum + value, 0);
	const percentage = (value: number) => total > 0 ? `${((value / total) * 100).toFixed(0)}%` : '0%';
	return PROVIDER_SERIES
		.map(({ label }, index) => `${label} ${format(values[index])} (${percentage(values[index])})`)
		.join(', ');
}

/** Bar blocks colored by series share, then 'empty' to fill. `values` follow PROVIDER_SERIES order. */
export function providerSeriesBlocks(values: readonly number[], fillPct: number, totalCols: number): Array<{ type: string }> {
	const filled = Math.round((fillPct / 100) * totalCols);
	const total = values.reduce((sum, value) => sum + value, 0);
	const arr: Array<{ type: string }> = [];
	for (let i = 0; i < filled && total > 0; i++) {
		const midpoint = ((i + 0.5) / filled) * total;
		let cumulative = 0;
		const index = values.findIndex((value) => {
			cumulative += value;
			return midpoint <= cumulative;
		});
		arr.push({ type: PROVIDER_SERIES[index === -1 ? PROVIDER_SERIES.length - 1 : index].type });
	}
	while (arr.length < totalCols) arr.push({ type: 'empty' });
	return arr;
}

/** USD charts contain only buckets with priced data; token charts retain every token-bearing day. */
export function selectChartDays(days: DailyCost[], mode: CostMode): DailyCost[] {
	return days.filter((day) => {
		if (mode === 'tokens') return day.sessions.some((session) => session.totalTokens > 0);
		return day.sessions.some(isCostAvailable);
	});
}
