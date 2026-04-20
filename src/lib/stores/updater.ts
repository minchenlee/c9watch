/**
 * Store for the user-controlled update flow.
 *
 * Lifecycle:
 *   1. App launch calls `runStartupCheck()` once. If an update is available,
 *      `updateAvailable` is set and (if not skipped) the banner renders.
 *   2. User lands on the Settings tab → `fetchReleaseNotes()` is called.
 *      The notes are cached by version so re-entering the tab doesn't re-fetch.
 *   3. User clicks "Download and install" → store drives download/install and
 *      reports progress via `downloadState`.
 *   4. User clicks "Skip this version" → the version is added to `skippedVersions`
 *      which persists to localStorage. The banner hides for that version.
 */

import { writable, derived, get } from 'svelte/store';
import { checkOnly, downloadOnly, installAndRelaunch, type UpdateHandle } from '../updater';
import { isTauri } from '../ws';

export type DownloadState = 'idle' | 'downloading' | 'ready' | 'installing' | 'error';

const SKIPPED_STORAGE_KEY = 'updaterSkippedVersions';

export const currentVersion = writable<string>('');
export const updateAvailable = writable<UpdateHandle | null>(null);
export const downloadState = writable<DownloadState>('idle');
export const downloadError = writable<string | null>(null);
export const downloadProgress = writable<{ received: number; total: number | null }>({
	received: 0,
	total: null
});
export const skippedVersions = writable<string[]>(loadSkipped());

/** Cache of release notes keyed by version tag, e.g. "v0.8.1" → markdown body. */
const releaseNotesCache = new Map<string, string | null>();
export const releaseNotes = writable<string | null>(null);
export const releaseNotesLoading = writable<boolean>(false);

/** True when we have an update AND the user hasn't skipped this version. */
export const bannerVisible = derived(
	[updateAvailable, skippedVersions],
	([$u, $skipped]) => !!$u && !$skipped.includes($u.version)
);

function loadSkipped(): string[] {
	if (typeof localStorage === 'undefined') return [];
	try {
		const raw = localStorage.getItem(SKIPPED_STORAGE_KEY);
		if (!raw) return [];
		const parsed = JSON.parse(raw);
		return Array.isArray(parsed) ? parsed.filter((v): v is string => typeof v === 'string') : [];
	} catch {
		return [];
	}
}

function saveSkipped(versions: string[]) {
	if (typeof localStorage === 'undefined') return;
	localStorage.setItem(SKIPPED_STORAGE_KEY, JSON.stringify(versions));
}

/** Mark a version as skipped — banner hides until a newer version appears. */
export function skipVersion(version: string) {
	skippedVersions.update((list) => {
		if (list.includes(version)) return list;
		const next = [...list, version];
		saveSkipped(next);
		return next;
	});
}

/** Clear all skipped versions (e.g. from a "Show all updates" toggle). */
export function clearSkippedVersions() {
	skippedVersions.set([]);
	saveSkipped([]);
}

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
 * Fetch release notes for the current available update from GitHub.
 * Cached per version so repeated tab entries don't re-fetch.
 * Network failure resolves to `null` (UI shows "Release notes unavailable").
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

	// Prefer the update payload's body if the backend already populated it.
	if (update.body && update.body.trim().length > 0) {
		releaseNotesCache.set(tag, update.body);
		releaseNotes.set(update.body);
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
		const body = typeof json.body === 'string' ? json.body : null;
		releaseNotesCache.set(tag, body);
		releaseNotes.set(body);
	} catch (err) {
		console.error('[updater-store] Failed to fetch release notes:', err);
		releaseNotesCache.set(tag, null);
		releaseNotes.set(null);
	} finally {
		releaseNotesLoading.set(false);
	}
}
