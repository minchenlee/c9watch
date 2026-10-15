/**
 * Shared session display logic. Type-only imports keep this module pure, so
 * scripts/test-*.mjs can transpile and test it directly.
 */

import type { AwayActivity, Session, WaitingFor } from './types';

/** "Approval: Bash", "Question"... Tool arguments are never shown. */
export function waitingForLabel(waitingFor: WaitingFor | null | undefined): string | null {
	switch (waitingFor?.kind) {
		case 'approval':
			return `Approval: ${waitingFor.tool}`;
		case 'question':
			return 'Question';
		case 'attention':
			return 'Needs attention';
		case 'input':
			return 'Idle, your turn';
		default:
			return null;
	}
}

function count(n: number, singular: string, plural: string, atLeast: boolean): string | null {
	if (n <= 0) return null;
	return `${atLeast ? '≥ ' : ''}${n} ${n === 1 ? singular : plural}`;
}

/** One-line summary of what happened while away; "—" when the provider has no data. */
export function awayActivityLabel(activity: AwayActivity | null | undefined): string {
	if (!activity) return '—';
	const p = activity.partial;
	const parts = [
		count(activity.filesEdited, 'file edited', 'files edited', p),
		count(activity.commandsRun, 'command', 'commands', p),
		count(activity.subagentsCompleted, 'subagent done', 'subagents done', p),
		count(activity.toolCalls, 'tool call', 'tool calls', p)
	].filter((part): part is string => part !== null);
	return parts.length > 0 ? parts.join(' · ') : 'No activity';
}

/**
 * Start of the "away" window: when the user last looked at this session
 * (overlay or FOCUS), else when the c9watch window last lost focus, else
 * when the session started.
 */
export function awaySinceMs(
	session: Pick<Session, 'startedAtMs'>,
	key: string,
	lastSeenAt: Record<string, number>,
	windowBlurAt: number | null
): number {
	return lastSeenAt[key] ?? windowBlurAt ?? session.startedAtMs ?? 0;
}
