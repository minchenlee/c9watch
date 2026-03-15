<script lang="ts">
	import type { Session } from '$lib/types';
	import { SessionStatus } from '$lib/types';

	interface Props {
		session: Session;
		selected?: boolean;
		onclick?: () => void;
	}

	let { session, selected = false, onclick }: Props = $props();

	// Get status color based on session status
	function getStatusColor(status: SessionStatus): string {
		switch (status) {
			case SessionStatus.Working:
				return '#3b82f6'; // blue
			case SessionStatus.NeedsAttention:
				return '#f97316'; // orange
			case SessionStatus.WaitingForInput:
				return '#22c55e'; // green
			case SessionStatus.Connecting:
				return '#3b82f6'; // blue (same as working)
			default:
				return '#9ca3af';
		}
	}

	// Format time since last activity
	function formatTimeSince(isoTimestamp: string): string {
		const now = new Date().getTime();
		const then = new Date(isoTimestamp).getTime();
		const diffMs = now - then;
		const diffMins = Math.floor(diffMs / 60000);
		const diffHours = Math.floor(diffMs / 3600000);
		const diffDays = Math.floor(diffMs / 86400000);

		if (diffMins < 1) return 'just now';
		if (diffMins < 60) return `${diffMins}m ago`;
		if (diffHours < 24) return `${diffHours}h ago`;
		return `${diffDays}d ago`;
	}

	// Truncate first prompt to fit in list
	function truncatePrompt(text: string, maxLength: number = 60): string {
		if (text.length <= maxLength) return text;
		return text.substring(0, maxLength) + '...';
	}
</script>

<button
	class="session-item"
	class:selected
	onclick={onclick}
	type="button"
>
	<div class="status-indicator" style="background-color: {getStatusColor(session.status)}"></div>
	<div class="session-content">
		<div class="session-header">
			<span class="session-name">{session.sessionName}</span>
			{#if session.gitBranch}
				<span class="git-branch">({session.gitBranch})</span>
			{/if}
			<span class="time-since">{formatTimeSince(session.modified)}</span>
		</div>
		<div class="first-prompt">{truncatePrompt(session.summary || session.firstPrompt)}</div>
		<div class="message-count">{session.messageCount} messages</div>
	</div>
</button>

<style>
	.session-item {
		display: flex;
		align-items: flex-start;
		gap: 10px;
		padding: 12px;
		border: 1px solid #e5e7eb;
		border-radius: 6px;
		background: white;
		cursor: pointer;
		transition: all 0.2s;
		width: 100%;
		text-align: left;
	}

	.session-item:hover {
		background: #f9fafb;
		border-color: #d1d5db;
	}

	.session-item.selected {
		background: #eff6ff;
		border-color: #3b82f6;
	}

	.status-indicator {
		width: 10px;
		height: 10px;
		border-radius: 50%;
		margin-top: 4px;
		flex-shrink: 0;
	}

	.session-content {
		flex: 1;
		min-width: 0;
		display: flex;
		flex-direction: column;
		gap: 4px;
	}

	.session-header {
		display: flex;
		align-items: center;
		gap: 6px;
		font-size: 14px;
	}

	.session-name {
		font-weight: 600;
		color: #111827;
	}

	.git-branch {
		color: #6b7280;
		font-size: 12px;
	}

	.time-since {
		margin-left: auto;
		color: #9ca3af;
		font-size: 11px;
		white-space: nowrap;
	}

	.first-prompt {
		color: #4b5563;
		font-size: 13px;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.message-count {
		color: #9ca3af;
		font-size: 11px;
	}
</style>
