/**
 * Shared cost store: fetches CostData once and derives a per-session lookup
 * map. Also holds the USD/TOKENS display mode, persisted to localStorage so
 * the toggle is consistent across CostTracker, SessionCard, and overlays.
 */

import { writable, derived, get } from 'svelte/store';
import { browser } from '$app/environment';
import { getCostData } from '../api';
import type { CostData, SessionCostRecord } from '../types';
import { buildSessionCostMap, type CostMode } from '../cost-utils';

const COST_MODE_KEY = 'c9watch.costMode';

function loadCostMode(): CostMode {
	if (!browser) return 'usd';
	const saved = localStorage.getItem(COST_MODE_KEY);
	return saved === 'tokens' ? 'tokens' : 'usd';
}

export const costMode = writable<CostMode>(loadCostMode());

if (browser) {
	costMode.subscribe((m) => {
		localStorage.setItem(COST_MODE_KEY, m);
	});
}

export const costData = writable<CostData | null>(null);

export const sessionCostMap = derived(costData, ($d) =>
	buildSessionCostMap($d)
);

let inFlight: Promise<void> | null = null;

/** Fetch (or refetch) cost data; callers can await the result. */
export async function refreshCostData(): Promise<void> {
	if (inFlight) return inFlight;
	inFlight = (async () => {
		try {
			const data = await getCostData();
			costData.set(data);
		} catch (e) {
			console.error('[cost] Failed to load cost data:', e);
		} finally {
			inFlight = null;
		}
	})();
	return inFlight;
}

/** Convenience: snapshot the cost record for one session. */
export function getSessionCost(sessionId: string): SessionCostRecord | undefined {
	return get(sessionCostMap).get(sessionId);
}
