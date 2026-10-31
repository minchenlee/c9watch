/**
 * Demo mode flag — extracted into its own module to avoid circular
 * dependency between demo/index.ts and stores/sessions.ts.
 */

import { writable } from 'svelte/store';

const STORAGE_KEY = 'demoMode';

/**
 * Whether demo mode is active. Persisted to localStorage.
 */
export const isDemoMode = writable<boolean>(false);

/**
 * Check whether demo mode was previously persisted to localStorage.
 */
export function isPersistedDemoMode(): boolean {
	try {
		return localStorage.getItem(STORAGE_KEY) === 'true';
	} catch {
		return false;
	}
}

/**
 * Persist the current demo-mode value to localStorage.
 */
export function persistDemoMode(value: boolean): void {
	try {
		localStorage.setItem(STORAGE_KEY, JSON.stringify(value));
	} catch {
		// localStorage not available
	}
}
