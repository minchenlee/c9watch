import { browser } from '$app/environment';
import { writable } from 'svelte/store';

export const usageProviders = ['claudeCode', 'codex', 'cursor'] as const;
export type UsageProvider = typeof usageProviders[number];
export interface UsagePreferences {
	percentages: 'auto' | 'always' | 'never';
	colors: 'provider' | 'monochrome';
	icons: boolean;
	providers: Record<UsageProvider, boolean>;
}
export const defaultUsagePreferences: UsagePreferences = {
	percentages: 'auto', colors: 'provider', icons: true,
	providers: { claudeCode: true, codex: true, cursor: true }
};
const key = 'c9watch.usagePreferences.v1';
export function normalizeUsagePreferences(value: unknown): UsagePreferences {
	const raw = value && typeof value === 'object' ? value as Partial<UsagePreferences> : {};
	return {
		percentages: raw.percentages === 'always' || raw.percentages === 'never' ? raw.percentages : 'auto',
		colors: raw.colors === 'monochrome' ? 'monochrome' : 'provider',
		icons: raw.icons !== false,
		providers: Object.fromEntries(usageProviders.map(provider => [provider, raw.providers?.[provider] !== false])) as Record<UsageProvider, boolean>
	};
}
function read(): UsagePreferences {
	try { return normalizeUsagePreferences(JSON.parse(localStorage.getItem(key) ?? 'null')); }
	catch { return normalizeUsagePreferences(null); }
}
export const usagePreferences = writable(browser ? read() : normalizeUsagePreferences(null));
export function saveUsagePreferences(next: UsagePreferences): string | null {
	const normalized = normalizeUsagePreferences(next);
	try {
		localStorage.setItem(key, JSON.stringify(normalized));
		usagePreferences.set(normalized);
		return null;
	} catch { return 'Could not save usage preferences. Please try again.'; }
}
if (browser) {
	window.addEventListener('storage', event => { if (event.key === key || event.key === null) usagePreferences.set(read()); });
	window.addEventListener('focus', () => usagePreferences.set(read()));
}
