import { browser } from '$app/environment';
import { writable } from 'svelte/store';

export const monitorCardFields = {
	model: 'Model',
	reasoningEffort: 'Reasoning effort',
	workingDuration: 'Working duration',
	gitBranch: 'Git branch',
	messageCount: 'Message count',
	cost: 'Cost or tokens',
	sessionAge: 'Session age'
} as const;

export type MonitorCardField = keyof typeof monitorCardFields;
type Preferences = Record<MonitorCardField, boolean>;
export const monitorCardFieldKeys = Object.keys(monitorCardFields) as MonitorCardField[];
const storageKey = 'c9watch.monitorCardPreferences.v1';

function readPreferences(): Preferences {
	let stored: Partial<Preferences> | null = null;
	if (browser) {
		try {
			stored = JSON.parse(localStorage.getItem(storageKey) ?? 'null');
		} catch {
			// Storage can be unavailable or contain invalid JSON.
		}
	}
	return Object.fromEntries(monitorCardFieldKeys.map((field) => [field, stored?.[field] !== false])) as Preferences;
}

export const monitorCardPreferences = writable(readPreferences());

export function setMonitorCardField(field: MonitorCardField, visible: boolean) {
	monitorCardPreferences.update((current) => {
		const next = { ...current, [field]: visible };
		try {
			localStorage.setItem(storageKey, JSON.stringify(next));
		} catch {
			// Keep the setting for this window even when storage is unavailable.
		}
		return next;
	});
}

if (browser) {
	window.addEventListener('storage', (event) => {
		if (event.key === storageKey || event.key === null) monitorCardPreferences.set(readPreferences());
	});
}
