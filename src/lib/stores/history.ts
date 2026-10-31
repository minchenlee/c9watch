/**
 * Shared history store: fetches session history once at app startup so the
 * HISTORY tab renders instantly when the user switches to it (otherwise the
 * getSessionHistory IPC blocks the main thread and lags the tab highlight).
 */

import { get, writable } from 'svelte/store';
import { getSessionHistory } from '../api';
import { providerSessionKey } from '../provider';
import type { HistoryEntry } from '../types';

export const historyEntries = writable<HistoryEntry[]>([]);
export const historyLoading = writable<boolean>(true);
export const historyError = writable<string | null>(null);

/**
 * Drop rows whose provider + session id was already seen. The list is keyed by
 * that pair, and Svelte stops updating the whole page on a repeated key.
 * The backend returns rows newest first, so the newest copy is kept.
 */
function uniqueBySession(entries: HistoryEntry[]): HistoryEntry[] {
	const seen = new Set<string>();
	return entries.filter((entry) => {
		const key = providerSessionKey(entry.provider, entry.sessionId);
		if (seen.has(key)) return false;
		seen.add(key);
		return true;
	});
}

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
	// Only the first load shows "Loading": a refetch keeps the rows on screen,
	// so reopening the tab does not tear the list down and draw it again.
	if (get(historyEntries).length === 0) historyLoading.set(true);
	historyError.set(null);
	inFlight = (async () => {
		try {
			const entries = await getSessionHistory();
			historyEntries.set(uniqueBySession(entries));
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
