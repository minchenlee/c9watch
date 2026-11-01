import { SessionStatus } from '$lib/types';
import type { WaitingFor } from '$lib/types';
import type { SubagentInfo } from '$lib/stores/subagents';

/** True for the pending-tool names that mean Claude is asking the user a question. */
export function isQuestionTool(name: string | null | undefined): boolean {
	return name === 'Question' || name === 'AskUserQuestion';
}

export function getSessionStatusColor(status: SessionStatus): string {
	switch (status) {
		case SessionStatus.Working:
			return 'var(--status-working)';
		case SessionStatus.NeedsAttention:
			return 'var(--status-permission)';
		case SessionStatus.WaitingForInput:
			return 'var(--status-input)';
		case SessionStatus.Connecting:
			return 'var(--status-working)';
		default:
			return 'var(--status-working)';
	}
}

export function getSessionStatusLabel(status: SessionStatus, pendingToolName?: string | null): string {
	switch (status) {
		case SessionStatus.Working:
			return 'Working';
		case SessionStatus.NeedsAttention:
			if (isQuestionTool(pendingToolName)) {
				return 'Waiting for Response';
			}
			return 'Approval Required';
		case SessionStatus.WaitingForInput:
			return 'Ready';
		case SessionStatus.Connecting:
			return 'Connecting';
		default:
			return 'Unknown';
	}
}

/** One word that fits a fixed-width chip in the dense view. */
export function getSessionStatusShortLabel(
	status: SessionStatus,
	waitingFor?: WaitingFor | null,
	pendingToolName?: string | null
): string {
	switch (waitingFor?.kind) {
		case 'approval': return 'APPROVE';
		case 'question': return 'QUESTION';
		case 'attention': return 'ATTN';
		case 'input': return 'IDLE';
	}
	switch (status) {
		case SessionStatus.Working: return 'WORKING';
		case SessionStatus.NeedsAttention:
			return pendingToolName === 'Question' || pendingToolName === 'AskUserQuestion'
				? 'QUESTION'
				: 'APPROVE';
		case SessionStatus.WaitingForInput: return 'IDLE';
		case SessionStatus.Connecting: return 'STARTING';
		default: return 'UNKNOWN';
	}
}

export function getSubagentStatusColor(status: SubagentInfo['status']): string {
	return status === 'running' ? 'var(--status-working)' : 'var(--text-muted)';
}

export function getSubagentStatusLabel(status: SubagentInfo['status']): string {
	return status === 'running' ? 'Running' : 'Completed';
}

/** The three dashboard groups a session status falls into. */
export type StatusBucket = 'attention' | 'idle' | 'working';

/** Bucket order and labels, as the monitor grid shows them. */
export const STATUS_BUCKETS: ReadonlyArray<{ id: StatusBucket; label: string }> = [
	{ id: 'attention', label: 'Needs Attention' },
	{ id: 'idle', label: 'Idle' },
	{ id: 'working', label: 'Working' }
];

/** Bucket for a session status. Unknown statuses return null and are not shown. */
export function statusBucket(status: SessionStatus): StatusBucket | null {
	switch (status) {
		case SessionStatus.NeedsAttention:
			return 'attention';
		case SessionStatus.WaitingForInput:
			return 'idle';
		case SessionStatus.Working:
		case SessionStatus.Connecting:
			return 'working';
		default:
			return null;
	}
}

/** Per-bucket counts in the shape StatusBar and the popover read. */
export function countStatusBuckets(sessions: ReadonlyArray<{ status: SessionStatus }>): {
	working: number;
	permission: number;
	input: number;
} {
	const counts = { working: 0, permission: 0, input: 0 };
	for (const session of sessions) {
		switch (statusBucket(session.status)) {
			case 'working':
				counts.working++;
				break;
			case 'attention':
				counts.permission++;
				break;
			case 'idle':
				counts.input++;
				break;
		}
	}
	return counts;
}
