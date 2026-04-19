/**
 * Shared history store: fetches session history once at app startup so the
 * HISTORY tab renders instantly when the user switches to it (otherwise the
 * getSessionHistory IPC blocks the main thread and lags the tab highlight).
 */

import { writable } from 'svelte/store';
import { getSessionHistory } from '../api';
import type { HistoryEntry } from '../types';

export const historyEntries = writable<HistoryEntry[]>([]);
export const historyLoading = writable<boolean>(true);
export const historyError = writable<string | null>(null);

let inFlight: Promise<void> | null = null;

/** Fetch (or refetch) session history. Callers can await. */
export async function refreshSessionHistory(): Promise<void> {
	if (inFlight) return inFlight;
	historyLoading.set(true);
	historyError.set(null);
	inFlight = (async () => {
		try {
			const entries = await getSessionHistory();
			historyEntries.set(entries);
		} catch (e) {
			historyError.set(String(e));
		} finally {
			historyLoading.set(false);
			inFlight = null;
		}
	})();
	return inFlight;
}
