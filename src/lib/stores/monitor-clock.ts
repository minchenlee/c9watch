import { browser } from '$app/environment';
import { readable } from 'svelte/store';
import { sessionKeyOf } from '$lib/provider';
import { SessionStatus } from '$lib/types';
import { sessions } from './sessions';

export const monitorClock = readable(Date.now(), (set) => {
	if (!browser) return;
	set(Date.now());
	const timer = window.setInterval(() => set(Date.now()), 1000);
	return () => window.clearInterval(timer);
});

/** The first observation of the current uninterrupted Working state. */
export const observedWorkingSince = readable(new Map<string, number>(), (set) => {
	if (!browser) return;
	let previous = new Map<string, number>();
	set(previous);
	return sessions.subscribe((current) => {
		const now = Date.now();
		const next = new Map<string, number>();
		for (const session of current) {
			if (session.status !== SessionStatus.Working) continue;
			const key = sessionKeyOf(session);
			next.set(key, previous.get(key) ?? now);
		}
		if (next.size === previous.size && Array.from(next.keys()).every((key) => previous.has(key))) return;
		previous = next;
		set(next);
	});
});
