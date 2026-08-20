/**
 * Store for subagent invocations detected by parsing parent session JSONL.
 *
 * No hook required: the backend (`get_subagents` Tauri command) re-scans each
 * project's JSONL files for Agent/Task tool_use entries and reports which
 * still lack a matching tool_result.
 */

import { writable, derived, get } from 'svelte/store';
import { invoke } from '@tauri-apps/api/core';
import { sessions } from './sessions';
import { isTauri } from '../ws';

export type SubagentStatus = 'running' | 'completed';

export interface SubagentInfo {
	id: string;
	agentType: string;
	description: string;
	startedAt: string;
	completedAt: string | null;
	parentSessionId: string;
	status: SubagentStatus;
	provider?: 'claudeCode' | 'codex' | 'cursor';
	sessionId?: string;
}

/** Raw map: parent session id -> subagents */
export const subagentsBySession = writable<Map<string, SubagentInfo[]>>(new Map());

/** How long to keep completed subagents visible after they finish, in ms. */
const COMPLETED_RETENTION_MS = 60_000;

/**
 * Derived view: same map but with stale completed subagents filtered out.
 * Components should consume this rather than the raw store.
 */
export const visibleSubagentsBySession = derived(subagentsBySession, ($map) => {
	const out = new Map<string, SubagentInfo[]>();
	const now = Date.now();
	for (const [pid, list] of $map.entries()) {
		const filtered = list.filter((s) => {
			if (s.status === 'running') return true;
			if (!s.completedAt) return true;
			const completedMs = new Date(s.completedAt).getTime();
			return now - completedMs < COMPLETED_RETENTION_MS;
		});
		if (filtered.length > 0) {
			out.set(pid, filtered);
		}
	}
	return out;
});

let pollHandle: ReturnType<typeof setInterval> | null = null;

async function refreshOnce() {
	if (!isTauri()) return;
	try {
		const raw = await invoke<Record<string, SubagentInfo[]>>('get_subagents');
		const m = new Map<string, SubagentInfo[]>();
		for (const [k, v] of Object.entries(raw)) {
			m.set(k, v);
		}
		subagentsBySession.set(m);
	} catch {
		// Backend may be unavailable in non-Tauri contexts; ignore.
	}
}

/**
 * Start polling for subagent updates. Triggered by the sessions store update
 * (re-fetch when sessions change) and on a slower fixed interval as a backstop.
 */
export function initializeSubagentPolling() {
	if (pollHandle !== null) return;
	// Re-fetch when the sessions list changes — that's our cheapest signal that
	// new transcript entries may have appeared.
	const unsub = sessions.subscribe(() => {
		refreshOnce();
	});
	// Fixed-interval backstop in case sessions are quiet but a long-running
	// subagent finishes mid-cycle.
	pollHandle = setInterval(refreshOnce, 4000);
	// Initial fetch
	refreshOnce();
	// Return a teardown for tests/HMR.
	return () => {
		if (pollHandle !== null) {
			clearInterval(pollHandle);
			pollHandle = null;
		}
		unsub();
	};
}

/** Test helper: read a snapshot of the current map. */
export function _snapshotForTests(): Map<string, SubagentInfo[]> {
	return get(subagentsBySession);
}
