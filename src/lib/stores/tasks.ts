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

/** Provider-scoped session key -> ordered task list (sorted by numeric id). */
export const tasksBySession = writable<Map<string, Task[]>>(new Map());

let pollHandle: ReturnType<typeof setInterval> | null = null;

function normalizeStatus(s: unknown): TaskStatus {
	if (s === 'in_progress' || s === 'completed') return s;
	return 'pending';
}

async function refreshOnce() {
	if (!isTauri()) return;
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
 * Start polling for TodoWrite task updates. Re-fetches on `sessions` store
 * changes (cheapest signal that JSONL activity may have appended TodoWrite
 * tool_use entries) and on a fixed 4 s backstop.
 */
export function initializeTasksPolling() {
	if (pollHandle !== null) return;
	const unsub = sessions.subscribe(() => {
		refreshOnce();
	});
	pollHandle = setInterval(refreshOnce, 4000);
	refreshOnce();
	return () => {
		if (pollHandle !== null) {
			clearInterval(pollHandle);
			pollHandle = null;
		}
		unsub();
	};
}

/** Test helper: read a snapshot of the current map. */
export function _snapshotForTests(): Map<string, Task[]> {
	return get(tasksBySession);
}
