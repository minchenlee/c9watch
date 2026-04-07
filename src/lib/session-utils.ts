import { SessionStatus, type Session } from '$lib/types';

/**
 * Whether a session has a pending tool-use prompt that can be approved/rejected
 * from the dashboard (excludes questions, which are not tool approvals).
 */
export function canApproveReject(session: Session): boolean {
	return (
		session.status === SessionStatus.NeedsAttention &&
		!!session.pendingToolName &&
		session.pendingToolName !== 'Question' &&
		session.pendingToolName !== 'AskUserQuestion'
	);
}

/**
 * One-line summary of the pending tool and its input for display next to status.
 * Returns null if no meaningful summary is available.
 */
export function getToolSummary(session: Session): string | null {
	if (!canApproveReject(session) || !session.pendingToolName) return null;
	const input = session.pendingToolInput;
	const tool = session.pendingToolName;

	if (tool === 'Bash' && input?.command) {
		const cmd = String(input.command);
		return `Bash: ${cmd.length > 60 ? cmd.slice(0, 57) + '...' : cmd}`;
	}
	if ((tool === 'Write' || tool === 'Edit') && input?.file_path) {
		const fp = String(input.file_path);
		return `${tool}: ${fp.length > 55 ? '...' + fp.slice(-52) : fp}`;
	}
	return tool;
}
