import { SessionStatus } from '$lib/types';
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
