/**
 * Shared helpers for displaying session cost and token counts.
 * Used by CostTracker, SessionCard, and the overlay components.
 */

import type { CostData, SessionCostRecord } from './types';

export type CostMode = 'usd' | 'tokens';

/** Format a dollar amount with at most 2 decimals, adapting to small values. */
export function formatCost(usd: number): string {
	if (!isFinite(usd) || usd <= 0) return '$0.00';
	if (usd < 0.01) return '<$0.01';
	if (usd < 1) return '$' + usd.toFixed(2);
	if (usd < 100) return '$' + usd.toFixed(2);
	return '$' + usd.toFixed(0);
}

/** Format a token count with k/M suffix for compact display. */
export function formatTokens(tokens: number): string {
	if (!isFinite(tokens) || tokens <= 0) return '0';
	if (tokens < 1000) return String(Math.round(tokens));
	if (tokens < 10_000) return (tokens / 1000).toFixed(1) + 'k';
	if (tokens < 1_000_000) return Math.round(tokens / 1000) + 'k';
	if (tokens < 10_000_000) return (tokens / 1_000_000).toFixed(2) + 'M';
	return (tokens / 1_000_000).toFixed(1) + 'M';
}

/** Render either the USD cost or the token count, depending on mode. */
export function formatCostOrTokens(usd: number, tokens: number, mode: CostMode, costAvailable = true): string {
	if (mode === 'usd' && !costAvailable) return 'UNPRICED';
	return mode === 'usd' ? formatCost(usd) : formatTokens(tokens);
}

/**
 * Build a map of sessionId -> aggregated cost record from the CostData payload.
 * A session can appear on multiple days (split per date); we sum cost and
 * totalTokens across days and keep the most recent timestamp/model.
 */
export function buildSessionCostMap(data: CostData | null): Map<string, SessionCostRecord> {
	const map = new Map<string, SessionCostRecord>();
	if (!data) return map;

	for (const day of data.dailyCosts) {
		for (const rec of day.sessions) {
			const existing = map.get(rec.sessionId);
			if (!existing) {
				map.set(rec.sessionId, { ...rec });
			} else {
				existing.cost += rec.cost;
				existing.totalTokens += rec.totalTokens;
				existing.inputTokens = (existing.inputTokens || 0) + (rec.inputTokens || 0);
				existing.cachedInputTokens = (existing.cachedInputTokens || 0) + (rec.cachedInputTokens || 0);
				existing.outputTokens = (existing.outputTokens || 0) + (rec.outputTokens || 0);
				existing.reasoningOutputTokens = (existing.reasoningOutputTokens || 0) + (rec.reasoningOutputTokens || 0);
				existing.costAvailable = (existing.costAvailable ?? existing.provider !== 'codex') && (rec.costAvailable ?? rec.provider !== 'codex');
				if (rec.timestamp > existing.timestamp) {
					existing.timestamp = rec.timestamp;
					existing.model = rec.model;
					existing.date = rec.date;
				}
			}
		}
	}
	return map;
}

/** Pretty-name for a raw model id, matching CostTracker's convention. */
export function modelDisplayName(model: string): string {
	if (model.startsWith('claude-sonnet')) return 'Sonnet';
	if (model.startsWith('claude-opus')) return 'Opus';
	if (model.startsWith('claude-haiku')) return 'Haiku';
	if (model === '<synthetic>' || model === 'unknown') return '—';
	return model;
}
