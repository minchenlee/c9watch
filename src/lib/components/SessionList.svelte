<script lang="ts">
	import { sessions, selectedSessionId } from '$lib/stores/sessions';
	import SessionItem from './SessionItem.svelte';

	// Subscribe to stores
	let currentSessions = $derived($sessions);
	let currentSelectedId = $derived($selectedSessionId);

	function selectSession(sessionId: string) {
		selectedSessionId.set(sessionId);
	}
</script>

<div class="session-list">
	<div class="list-header">
		<h2>Active Sessions</h2>
		<span class="session-count">{currentSessions.length}</span>
	</div>

	<div class="list-content">
		{#if currentSessions.length === 0}
			<div class="empty-state">
				<p>No active sessions detected</p>
				<p class="empty-hint">Start a Claude Code, Codex, or Cursor Agent session to see it here</p>
			</div>
		{:else}
			<div class="session-items">
				{#each currentSessions as session (session.id)}
					<SessionItem
						{session}
						selected={session.id === currentSelectedId}
						onclick={() => selectSession(session.id)}
					/>
				{/each}
			</div>
		{/if}
	</div>
</div>

<style>
	.session-list {
		display: flex;
		flex-direction: column;
		height: 100%;
		background: #fafafa;
	}

	.list-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding: 16px 20px;
		border-bottom: 1px solid #e5e7eb;
		background: white;
	}

	.list-header h2 {
		margin: 0;
		font-size: 18px;
		font-weight: 600;
		color: #111827;
	}

	.session-count {
		background: #e5e7eb;
		color: #6b7280;
		font-size: 12px;
		font-weight: 600;
		padding: 2px 8px;
		border-radius: 12px;
	}

	.list-content {
		flex: 1;
		overflow-y: auto;
		padding: 12px;
	}

	.session-items {
		display: flex;
		flex-direction: column;
		gap: 8px;
	}

	.empty-state {
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		height: 100%;
		text-align: center;
		padding: 40px 20px;
	}

	.empty-state p {
		margin: 0;
		color: #6b7280;
		font-size: 14px;
	}

	.empty-hint {
		margin-top: 8px;
		font-size: 12px;
		color: #9ca3af;
	}
</style>
