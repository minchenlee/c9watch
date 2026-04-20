/**
 * Auto-updater — split into discrete steps so the UI can drive the flow:
 *   checkOnly()          → returns an Update handle or null (no download)
 *   downloadOnly(update, onEvent) → downloads bytes (no install)
 *   installAndRelaunch(update)    → installs the downloaded bytes + relaunches
 *
 * Call sites should treat all functions as no-ops when not running inside Tauri.
 */

import { isTauri } from './ws';

export type UpdateHandle = import('@tauri-apps/plugin-updater').Update;
export type DownloadEvent = import('@tauri-apps/plugin-updater').DownloadEvent;

/**
 * Check for an available update without downloading it.
 * Returns the Update handle (with `.version` / `.body` / etc.) or null.
 */
export async function checkOnly(): Promise<UpdateHandle | null> {
	if (!isTauri()) return null;
	try {
		const { check } = await import('@tauri-apps/' + 'plugin-updater');
		return await check();
	} catch (error) {
		console.error('[updater] Update check failed:', error);
		return null;
	}
}

/**
 * Download the update bytes. Call once before `installAndRelaunch`.
 * `onEvent` receives Started / Progress / Finished events from the plugin.
 */
export async function downloadOnly(
	update: UpdateHandle,
	onEvent?: (progress: DownloadEvent) => void
): Promise<void> {
	if (!isTauri()) return;
	await update.download(onEvent);
}

/**
 * Install the downloaded bytes and relaunch the app.
 * Must be called after a successful `downloadOnly`.
 */
export async function installAndRelaunch(update: UpdateHandle): Promise<void> {
	if (!isTauri()) return;
	await update.install();
	const { relaunch } = await import('@tauri-apps/' + 'plugin-process');
	await relaunch();
}
