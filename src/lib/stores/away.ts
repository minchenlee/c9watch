/**
 * "While you were away" state: when the user last looked at each session,
 * and the backend's activity counts since then. Lives only in this window;
 * nothing is persisted across restarts.
 */

import { writable, get } from 'svelte/store';
import type { AwayActivity, Session } from '../types';
import { getAwayActivity } from '../api';
import { sessionKeyOf } from '../provider';
import { awaySinceMs } from '../session-utils';
import { sessions, expandedSessionId } from './sessions';

/** Session key → ms when the user last opened its overlay or pressed FOCUS. */
export const lastSeenAt = writable<Record<string, number>>({});

/** Ms when the c9watch window last lost focus. */
export const windowBlurAt = writable<number | null>(null);

/** Session key → activity since that session's away start. */
export const awayActivity = writable<Record<string, AwayActivity>>({});

export function markSessionSeen(key: string): void {
	lastSeenAt.update((seen) => ({ ...seen, [key]: Date.now() }));
}

/** FOCUS only knows the pid; mark every session behind it. */
export function markPidSeen(pid: number): void {
	for (const session of get(sessions)) {
		if (session.pid === pid) markSessionSeen(sessionKeyOf(session));
	}
}

let lastSignature = '';
let latestRequest = 0;

async function refresh(list: Session[]): Promise<void> {
	const seen = get(lastSeenAt);
	const blurAt = get(windowBlurAt);
	const waiting = list.filter((session) => session.waitingFor);
	const requests = waiting.map((session) => {
		const key = sessionKeyOf(session);
		return { sessionKey: key, sinceMs: awaySinceMs(session, key, seen, blurAt) };
	});
	// Re-query only when a waiting session changed or its away start moved.
	const signature = JSON.stringify([requests, waiting.map((session) => session.modified)]);
	if (signature === lastSignature) return;
	lastSignature = signature;
	const request = ++latestRequest;
	try {
		const activity = await getAwayActivity(requests);
		// Drop a slow response that a newer request already superseded.
		if (request === latestRequest) awayActivity.set(activity);
	} catch (error) {
		if (request === latestRequest) lastSignature = '';
		console.error('Failed to load away activity:', error);
	}
}

/** Start tracking; returns a cleanup function. */
export function initAwayTracking(): () => void {
	const onBlur = () => windowBlurAt.set(Date.now());
	window.addEventListener('blur', onBlur);
	const update = () => void refresh(get(sessions));
	// Opening and closing the overlay both count as looking at the session.
	let expanded: string | null = null;
	const unsubscribers = [
		sessions.subscribe(update),
		lastSeenAt.subscribe(update),
		windowBlurAt.subscribe(update),
		expandedSessionId.subscribe((key) => {
			if (key) markSessionSeen(key);
			if (expanded && expanded !== key) markSessionSeen(expanded);
			expanded = key;
		})
	];
	return () => {
		window.removeEventListener('blur', onBlur);
		for (const unsubscribe of unsubscribers) unsubscribe();
		lastSignature = '';
	};
}
