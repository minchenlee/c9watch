import type { CostMode } from './cost-utils';
import type { DailyCost, SessionCostRecord } from './types';

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

/** USD charts contain only buckets with priced data; token charts retain every token-bearing day. */
export function selectChartDays(days: DailyCost[], mode: CostMode): DailyCost[] {
	return days.filter((day) => {
		if (mode === 'tokens') return day.sessions.some((session) => session.totalTokens > 0);
		return day.sessions.some(isCostAvailable);
	});
}
