import { SessionStatus } from '$lib/types';
import type { WaitingFor } from '$lib/types';
import type { SubagentInfo } from '$lib/stores/subagents';

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
			if (pendingToolName === 'Question' || pendingToolName === 'AskUserQuestion') {
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

export function getWorkerStatusColor(status: SessionStatus): string {
	switch (status) {
		case SessionStatus.Working: return 'var(--status-working)';
		case SessionStatus.NeedsAttention: return 'var(--status-permission)';
		case SessionStatus.WaitingForInput: return 'var(--status-input)';
		case SessionStatus.Connecting: return 'var(--status-working)';
		default: return 'var(--text-muted)';
	}
}

export function getWorkerStatusLabel(status: SessionStatus): string {
	switch (status) {
		case SessionStatus.Working: return 'Working';
		case SessionStatus.NeedsAttention: return 'Attention';
		case SessionStatus.WaitingForInput: return 'Ready';
		case SessionStatus.Connecting: return 'Connecting';
		default: return 'Unknown';
	}
}
