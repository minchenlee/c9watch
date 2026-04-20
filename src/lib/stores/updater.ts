/**
 * Store for the user-controlled update flow.
 *
 * Lifecycle:
 *   1. App launch calls `runStartupCheck()` once. If an update is available,
 *      `updateAvailable` is set and the banner renders.
 *   2. User lands on the Settings tab → `fetchReleaseNotes()` is called.
 *      The notes are cached by version so re-entering the tab doesn't re-fetch.
 *   3. User clicks "Download and install" → store drives download/install and
 *      reports progress via `downloadState`.
 */

import { writable, derived, get } from 'svelte/store';
import { checkOnly, downloadOnly, installAndRelaunch, type UpdateHandle } from '../updater';
import { isTauri } from '../ws';

export type DownloadState = 'idle' | 'downloading' | 'ready' | 'installing' | 'error';

export const currentVersion = writable<string>('');
export const updateAvailable = writable<UpdateHandle | null>(null);
export const downloadState = writable<DownloadState>('idle');
export const downloadError = writable<string | null>(null);
export const downloadProgress = writable<{ received: number; total: number | null }>({
	received: 0,
	total: null
});

/** Cache of release notes keyed by version tag, e.g. "v0.8.1" → markdown body. */
const releaseNotesCache = new Map<string, string | null>();
export const releaseNotes = writable<string | null>(null);
export const releaseNotesLoading = writable<boolean>(false);

export const bannerVisible = derived(updateAvailable, ($u) => !!$u);

/** Run once on launch: read version + check for an update in the background. */
export async function runStartupCheck() {
	if (!isTauri()) return;
	try {
		const { getVersion } = await import('@tauri-apps/api/app');
		currentVersion.set(await getVersion());
	} catch (err) {
		console.error('[updater-store] Failed to read app version:', err);
	}
	const update = await checkOnly();
	updateAvailable.set(update);
}

/** Manual re-check (from the "Check for updates" button in Settings). */
export async function manualCheck() {
	if (!isTauri()) return;
	const update = await checkOnly();
	updateAvailable.set(update);
}

/** Kick off download + install. State/progress is driven via the store. */
export async function startDownloadAndInstall() {
	const update = get(updateAvailable);
	if (!update) return;

	downloadError.set(null);
	downloadProgress.set({ received: 0, total: null });
	downloadState.set('downloading');

	try {
		await downloadOnly(update, (ev) => {
			if (ev.event === 'Started') {
				downloadProgress.set({ received: 0, total: ev.data.contentLength ?? null });
			} else if (ev.event === 'Progress') {
				downloadProgress.update((p) => ({
					received: p.received + ev.data.chunkLength,
					total: p.total
				}));
			} else if (ev.event === 'Finished') {
				downloadState.set('ready');
			}
		});
		// Some platforms don't emit Finished; make sure we flip to ready.
		if (get(downloadState) === 'downloading') downloadState.set('ready');

		downloadState.set('installing');
		await installAndRelaunch(update);
		// Relaunch happens — anything below may not run.
	} catch (err) {
		console.error('[updater-store] Download/install failed:', err);
		downloadError.set(err instanceof Error ? err.message : String(err));
		downloadState.set('error');
	}
}

/**
 * Fetch release notes for the current available update.
 * Prefers the GitHub API (richer markdown) over `update.body` which is often
 * a short placeholder from `latest.json`. Cached per version so repeated tab
 * entries don't re-fetch. Network failure falls back to `update.body`, then null.
 */
export async function fetchReleaseNotes() {
	const update = get(updateAvailable);
	if (!update) {
		releaseNotes.set(null);
		return;
	}

	const tag = `v${update.version}`;
	if (releaseNotesCache.has(tag)) {
		releaseNotes.set(releaseNotesCache.get(tag) ?? null);
		return;
	}

	releaseNotesLoading.set(true);
	try {
		const res = await fetch(
			`https://api.github.com/repos/minchenlee/c9watch/releases/tags/${encodeURIComponent(tag)}`,
			{ headers: { Accept: 'application/vnd.github+json' } }
		);
		if (!res.ok) throw new Error(`GitHub returned ${res.status}`);
		const json = (await res.json()) as { body?: string };
		const body = typeof json.body === 'string' && json.body.trim().length > 0 ? json.body : null;
		const fallback = update.body && update.body.trim().length > 0 ? update.body : null;
		const finalBody = body ?? fallback;
		releaseNotesCache.set(tag, finalBody);
		releaseNotes.set(finalBody);
	} catch (err) {
		console.error('[updater-store] Failed to fetch release notes:', err);
		const fallback = update.body && update.body.trim().length > 0 ? update.body : null;
		releaseNotesCache.set(tag, fallback);
		releaseNotes.set(fallback);
	} finally {
		releaseNotesLoading.set(false);
	}
}
