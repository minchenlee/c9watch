/**
 * Workflow store: lists Claude Code workflow runs for the WORKFLOWS tab.
 *
 * The journal files on disk are updated in place while a run is in progress,
 * so a running workflow is surfaced "live" simply by re-listing on an interval.
 * Polling is started by the tab when it mounts and stopped when it unmounts;
 * the interval keeps running only while at least one run is `running`, falling
 * back to a slower idle refresh otherwise.
 */

import { writable, get } from 'svelte/store';
import { listWorkflows } from '../api';
import type { WorkflowSummary } from '../types';

export const workflows = writable<WorkflowSummary[]>([]);
export const workflowsLoading = writable<boolean>(true);
export const workflowsError = writable<string | null>(null);

let inFlight: Promise<void> | null = null;

/** Fetch (or refetch) the workflow list. Callers can await. */
export async function refreshWorkflows(): Promise<void> {
	if (inFlight) return inFlight;
	workflowsError.set(null);
	inFlight = (async () => {
		try {
			const list = await listWorkflows();
			workflows.set(list);
		} catch (e) {
			workflowsError.set(String(e));
		} finally {
			workflowsLoading.set(false);
			inFlight = null;
		}
	})();
	return inFlight;
}

const LIVE_MS = 3000;
const IDLE_MS = 15000;
let timer: ReturnType<typeof setTimeout> | null = null;

function hasRunning(): boolean {
	return get(workflows).some((w) => w.status === 'running');
}

function tick() {
	refreshWorkflows().finally(() => {
		if (timer === null) return; // stopped while in-flight
		timer = setTimeout(tick, hasRunning() ? LIVE_MS : IDLE_MS);
	});
}

/** Begin polling. Safe to call repeatedly (idempotent). */
export function startWorkflowPolling() {
	if (timer !== null) return;
	timer = setTimeout(() => {}, 0); // mark active before first async tick
	tick();
}

/** Stop polling (call on tab unmount). */
export function stopWorkflowPolling() {
	if (timer !== null) {
		clearTimeout(timer);
		timer = null;
	}
}
