/**
 * Demo mode state management, toggle logic, and status simulation
 */

import { get } from 'svelte/store';
import { sessions } from '../stores/sessions';
import type { Session } from '../types';
import type { SessionStatus } from '../types';
import { getDemoSessions, statusMessages, statusTransitions, withDemoAwayFields } from './data';
import { isDemoMode, isPersistedDemoMode, persistDemoMode } from './mode';

export { isDemoMode } from './mode';

const SIMULATION_INTERVAL_MS = 4000;

let simulationTimer: ReturnType<typeof setInterval> | null = null;

/**
 * Pick a random next status for a session based on transition rules.
 */
function nextStatus(current: SessionStatus): SessionStatus {
	const candidates = statusTransitions[current];
	return candidates[Math.floor(Math.random() * candidates.length)];
}

/**
 * Advance one random session to a new status, update its timestamp
 * and latest message, then push the updated list to the store.
 */
function tick() {
	const current = get(sessions);
	if (current.length === 0) return;

	// Pick a random session to transition
	const idx = Math.floor(Math.random() * current.length);
	const session = current[idx];
	const newStatus = nextStatus(session.status);

	// If status didn't actually change, skip the update
	if (newStatus === session.status) return;

	const msgs = statusMessages[session.id];
	const updated: Session = withDemoAwayFields({
		...session,
		status: newStatus,
		modified: new Date().toISOString(),
		messageCount: session.messageCount + (newStatus === session.status ? 0 : 1),
		latestMessage: msgs ? msgs[newStatus] : session.latestMessage
	});

	const next = [...current];
	next[idx] = updated;
	sessions.set(next);
}

function startSimulation() {
	stopSimulation();
	simulationTimer = setInterval(tick, SIMULATION_INTERVAL_MS);
}

function stopSimulation() {
	if (simulationTimer !== null) {
		clearInterval(simulationTimer);
		simulationTimer = null;
	}
}

/**
 * Toggle demo mode on/off.
 * When enabled, populates the sessions store with demo data and starts simulation.
 * When disabled, clears the sessions store and stops simulation.
 */
export function toggleDemoMode() {
	const current = get(isDemoMode);
	const next = !current;
	isDemoMode.set(next);
	persistDemoMode(next);

	if (next) {
		sessions.set(getDemoSessions());
		startSimulation();
	} else {
		stopSimulation();
		sessions.set([]);
	}
}

/**
 * Called on app init. If demo mode was persisted, load demo sessions into the store
 * and start the simulation. Returns true if demo mode is active.
 */
export function loadDemoDataIfActive(): boolean {
	if (isPersistedDemoMode()) {
		isDemoMode.set(true);
		sessions.set(getDemoSessions());
		startSimulation();
		return true;
	}
	return false;
}
