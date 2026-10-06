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
let loadedAt = 0;
const HISTORY_CACHE_TTL_MS = 10_000;

/** Fetch (or refetch) session history. Callers can await. */
export async function refreshSessionHistory(): Promise<void> {
	// The app preloads history and the History tab asks for it again on mount.
	// Reuse a recent successful result so that second mount does not repeat a
	// potentially expensive archive scan immediately.
	if (loadedAt > 0 && Date.now() - loadedAt < HISTORY_CACHE_TTL_MS) return;
	if (inFlight) return inFlight;
	historyLoading.set(true);
	historyError.set(null);
	inFlight = (async () => {
		try {
			const entries = await getSessionHistory();
			historyEntries.set(entries);
			loadedAt = Date.now();
		} catch (e) {
			historyError.set(String(e));
		} finally {
			historyLoading.set(false);
			inFlight = null;
		}
	})();
	return inFlight;
}
