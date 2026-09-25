/**
 * Store for TodoWrite task lists, sourced from `~/.claude/tasks/<session_id>/*.json`.
 *
 * No backend hook required: each polling tick iterates the current `sessions`
 * store and invokes the `get_session_tasks` Tauri command per session id.
 * Read-only — writes back to disk would race with Claude itself.
 */

import { writable, get } from 'svelte/store';
import { invoke } from '@tauri-apps/api/core';
import { sessions } from './sessions';
import { isTauri } from '../ws';
import type { Task, TaskStatus } from '../types';
import { providerOf, providerSessionKey } from '../provider';

let pollingEnabled = false;
let watched = false;
let stopPolling: (() => void) | null = null;

/**
 * Provider-scoped session key -> ordered task list (sorted by numeric id).
 * Polling runs only while the map has subscribers (the expanded card
 * overlay's todo panel).
 */
export const tasksBySession = writable<Map<string, Task[]>>(new Map(), () => {
	watched = true;
	if (pollingEnabled && stopPolling === null) stopPolling = startPolling();
	return () => {
		watched = false;
		stopPolling?.();
		stopPolling = null;
	};
});

function normalizeStatus(s: unknown): TaskStatus {
	if (s === 'in_progress' || s === 'completed') return s;
	return 'pending';
}

async function refreshOnce() {
	// Hidden windows skip the backstop poll; the sessions store change applied
	// on becoming visible triggers a fresh read.
	if (!isTauri() || document.hidden) return;
	// TodoWrite tasks are a Claude Code namespace. Do not query or cache them
	// for Codex/Cursor sessions whose opaque IDs may collide with Claude IDs.
	const ids = get(sessions)
		.filter((s) => providerOf(s) === 'claudeCode')
		.map((s) => s.id);
	const next = new Map<string, Task[]>();

	await Promise.all(
		ids.map(async (sessionId) => {
			try {
				const raw = await invoke<unknown[]>('get_session_tasks', { sessionId });
				const tasks: Task[] = raw.map((t) => {
					const obj = t as Record<string, unknown>;
					const subject = typeof obj.subject === 'string' ? obj.subject
						: typeof obj.description === 'string' ? obj.description
						: '';
					const activeForm = typeof obj.activeForm === 'string' ? obj.activeForm : subject;
					return {
						id: String(obj.id ?? ''),
						subject,
						activeForm,
						status: normalizeStatus(obj.status),
					};
				});
				if (tasks.length > 0) {
					next.set(providerSessionKey('claudeCode', sessionId), tasks);
				}
			} catch {
				// Backend may be unavailable; skip silently (mirrors subagents).
			}
		}),
	);

	tasksBySession.set(next);
}

/**
 * Enable polling for TodoWrite task updates. While `tasksBySession` has
 * subscribers, re-fetches on `sessions` store changes (cheapest signal that
 * JSONL activity may have appended TodoWrite tool_use entries) and on a fixed
 * 4 s backstop.
 */
export function initializeTasksPolling() {
	if (pollingEnabled) return;
	pollingEnabled = true;
	if (watched && stopPolling === null) stopPolling = startPolling();
	return () => {
		pollingEnabled = false;
		stopPolling?.();
		stopPolling = null;
	};
}

function startPolling(): () => void {
	// The sessions subscription fires immediately, which performs the initial fetch.
	const unsub = sessions.subscribe(() => {
		refreshOnce();
	});
	const pollHandle = setInterval(refreshOnce, 4000);
	return () => {
		clearInterval(pollHandle);
		unsub();
	};
}

/** Test helper: read a snapshot of the current map. */
export function _snapshotForTests(): Map<string, Task[]> {
	return get(tasksBySession);
}
