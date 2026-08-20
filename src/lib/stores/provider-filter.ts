import { browser } from '$app/environment';
import { writable } from 'svelte/store';
import type { ProviderFilter } from '$lib/provider';

const STORAGE_KEY = 'c9watch.providerFilter';

function sanitize(value: string | null): ProviderFilter {
	return value === 'claudeCode' || value === 'codex' || value === 'cursor' ? value : 'all';
}

export const providerFilter = writable<ProviderFilter>(
	browser ? sanitize(localStorage.getItem(STORAGE_KEY)) : 'all'
);

if (browser) {
	providerFilter.subscribe((value) => localStorage.setItem(STORAGE_KEY, sanitize(value)));
	window.addEventListener('storage', (event) => {
		if (event.key === STORAGE_KEY) providerFilter.set(sanitize(event.newValue));
	});
}
