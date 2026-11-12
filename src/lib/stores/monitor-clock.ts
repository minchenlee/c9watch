import { browser } from '$app/environment';
import { readable } from 'svelte/store';

export const monitorClock = readable(Date.now(), (set) => {
	if (!browser) return;
	set(Date.now());
	const timer = window.setInterval(() => set(Date.now()), 1000);
	return () => window.clearInterval(timer);
});
