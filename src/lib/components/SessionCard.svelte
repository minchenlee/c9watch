<script lang="ts">
	import type { Session } from '$lib/types';
	import { SessionStatus } from '$lib/types';
	import { renameSession } from '$lib/api';
	import { invoke } from '@tauri-apps/api/core';
	import { isTauri } from '$lib/ws';


	interface Props {
		session: Session;
		compact?: boolean;
		onexpand?: () => void;
		onstop?: () => void;
		onopen?: () => void;
	}

	let { session, compact = false, onexpand, onstop, onopen }: Props = $props();

	let needsAttention = $derived(
		session.status === SessionStatus.NeedsAttention ||
			session.status === SessionStatus.WaitingForInput
	);

	let isPermission = $derived(session.status === SessionStatus.NeedsAttention);
	let isWaitingInput = $derived(session.status === SessionStatus.WaitingForInput);
	let isWorking = $derived(session.status === SessionStatus.Working);

	let isEditingTitle = $state(false);
	let tempTitle = $state('');
	let terminalTitleHint = $state<string | null>(null);
	let optimisticTitle = $state<string | null>(null);

	let cardTitle = $derived(optimisticTitle || session.customTitle || session.summary || session.firstPrompt);

	// Clear optimistic title once polling delivers the real update
	$effect(() => {
		if (optimisticTitle && session.customTitle === optimisticTitle) {
			optimisticTitle = null;
		}
	});

	function getStatusColor(): string {
		switch (session.status) {
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

	function getStatusLabel(): string {
		switch (session.status) {
			case SessionStatus.Working:
				return 'Working';
			case SessionStatus.NeedsAttention:
				if (session.pendingToolName === 'Question' || session.pendingToolName === 'AskUserQuestion') {
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

	function formatTimeSince(isoTimestamp: string): string {
		const now = new Date().getTime();
		const then = new Date(isoTimestamp).getTime();
		const diffMs = now - then;
		const diffMins = Math.floor(diffMs / 60000);
		const diffHours = Math.floor(diffMs / 3600000);
		const diffDays = Math.floor(diffMs / 86400000);

		if (diffMins < 1) return 'now';
		if (diffMins < 60) return `${diffMins}m`;
		if (diffHours < 24) return `${diffHours}h`;
		return `${diffDays}d`;
	}


	function handleCardClick(e: MouseEvent) {
		const target = e.target as HTMLElement;
		if (
			target.closest('.action-btn') ||
			target.closest('.project-name-input') ||
			target.closest('.title-input')
		) {
			return;
		}
		onexpand?.();
	}

	function handleCardKeydown(e: KeyboardEvent) {
		if (e.key === 'Enter' || e.key === ' ') {
			const target = e.target as HTMLElement;
			if (target.classList.contains('session-card')) {
				e.preventDefault();
				onexpand?.();
			}
		}
	}


	function handleStop(e: MouseEvent) {
		e.stopPropagation();
		onstop?.();
	}

	function handleOpen(e: MouseEvent) {
		e.stopPropagation();
		onopen?.();
	}

	let saving = false;
	async function saveTitle() {
		if (!isEditingTitle || saving) return;
		saving = true;
		isEditingTitle = false;
		const currentTitle = session.customTitle || session.summary || session.firstPrompt;
		if (tempTitle.trim() && tempTitle !== currentTitle) {
			try {
				await renameSession(session.id, tempTitle.trim());
				optimisticTitle = tempTitle.trim();
			} catch (err) {
				console.error('Failed to rename session:', err);
				optimisticTitle = null;
			}
		}
		saving = false;
	}

	function handleTitleKeydown(e: KeyboardEvent) {
		if (e.key === 'Enter') {
			saveTitle();
		} else if (e.key === 'Escape') {
			tempTitle = session.customTitle || session.summary || session.firstPrompt;
			isEditingTitle = false;
		} else if (e.key === 'Tab' && terminalTitleHint && !tempTitle.trim()) {
			e.preventDefault();
			e.stopPropagation();
			tempTitle = terminalTitleHint;
			// Re-focus the input after Svelte re-renders
			const input = e.target as HTMLInputElement;
			requestAnimationFrame(() => input.focus());
		}
	}

	async function startEditing(e?: MouseEvent) {
		e?.stopPropagation();
		tempTitle = session.customTitle || session.summary || session.firstPrompt;
		isEditingTitle = true;
		// Fetch terminal title (iTerm2) as rename hint (Tauri only)
		if (isTauri()) {
			try {
				const title = await invoke<string | null>('get_terminal_title', { pid: session.pid });
				terminalTitleHint = title;
			} catch {
				terminalTitleHint = null;
			}
		}
	}

	function autofocus(node: HTMLElement) {
		node.focus();
		if (node instanceof HTMLInputElement) {
			node.select();
		}
	}
</script>

<div
	class="session-card"
	class:compact
	class:attention={needsAttention}
	class:permission={isPermission}
	class:waiting={isWaitingInput}
	class:working={isWorking}
	onclick={handleCardClick}
	onkeydown={handleCardKeydown}
	role="button"
	tabindex="0"
>
	<!-- Card Content -->
	<div class="card-body">
		<!-- Header (Summary as Title) -->
		<div class="card-header">
			{#if isEditingTitle}
				<input
					type="text"
					class="title-input"
					bind:value={tempTitle}
					placeholder={terminalTitleHint ?? ''}
					onkeydown={handleTitleKeydown}
					onblur={saveTitle}
					use:autofocus
					onclick={(e) => e.stopPropagation()}
				/>
			{:else}
				<h3 class="card-main-title" ondblclick={() => startEditing()}>{cardTitle}</h3>
			{/if}
		</div>

		<!-- Project & Stats Row -->
		<div class="stats-row">
			<span class="session-name-badge">{session.sessionName}</span>
			
			{#if !compact}
				<div class="stats-group">
					<span class="message-count">
						<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
							<path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z" />
						</svg>
						{session.messageCount}
					</span>
					<span class="time-badge">{formatTimeSince(session.modified)}</span>
				</div>
			{/if}
			
			{#if compact}
				<div class="status-label" style="color: {getStatusColor()}">
					{getStatusLabel()}
				</div>
			{/if}
		</div>

		{#if !compact}
			<!-- Git Branch -->
			{#if session.gitBranch}
				<div class="git-branch">
					<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
						<line x1="6" y1="3" x2="6" y2="15" />
						<circle cx="18" cy="6" r="3" />
						<circle cx="6" cy="18" r="3" />
						<path d="M18 9a9 9 0 0 1-9 9" />
					</svg>
					<span class="branch-name">{session.gitBranch}</span>
				</div>
			{/if}

			<!-- Status Label -->
			<div class="status-label" style="color: {getStatusColor()}">
				{getStatusLabel()}
			</div>
		{/if}

		{#if !compact}
			<!-- Message Preview -->
			<p class="task-preview">{session.latestMessage || session.firstPrompt}</p>

			<!-- Bottom Actions -->
			<div class="card-actions-container">
				<div class="card-actions">
					<button type="button" class="action-btn" onclick={(e) => startEditing(e)} title="Rename">
						<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
							<path d="M11 4H4a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-7" />
							<path d="M18.5 2.5a2.121 2.121 0 0 1 3 3L12 15l-4 1 1-4 9.5-9.5z" />
						</svg>
						RENAME
					</button>
					<button type="button" class="action-btn danger" onclick={handleStop} title="Stop">
						<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
							<rect x="6" y="6" width="12" height="12" rx="1" />
						</svg>
						STOP
					</button>
					<button type="button" class="action-btn primary" onclick={handleOpen} title="Open">
						<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
							<path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6" />
							<polyline points="15 3 21 3 21 9" />
							<line x1="10" y1="14" x2="21" y2="3" />
						</svg>
						OPEN
					</button>
				</div>
			</div>
		{:else}
			<div class="compact-actions">
				<button type="button" class="action-btn icon-only" onclick={handleOpen} title="Open">
					<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
						<path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6" />
						<polyline points="15 3 21 3 21 9" />
						<line x1="10" y1="14" x2="21" y2="3" />
					</svg>
				</button>
			</div>
		{/if}
	</div>
</div>

<style>
	.session-card {
		position: relative;
		display: flex;
		gap: var(--space-lg);
		padding: var(--space-lg);
		background: var(--bg-card);
		border: 1px solid var(--border-default);
		cursor: pointer;
		transition: all 0.2s cubic-bezier(0.16, 1, 0.3, 1);
		text-align: left;
		width: 100%;
		height: 235px;
	}


	.session-card:hover {
		border-color: var(--text-muted);
		box-shadow: 0 4px 12px rgba(0, 0, 0, 0.5);
		background: var(--bg-card-hover);
	}

	/* Card Body */
	.card-body {
		flex: 1;
		min-width: 0;
		display: flex;
		flex-direction: column;
		gap: var(--space-sm);
	}

	.card-header {
		display: flex;
		align-items: center;
		gap: var(--space-sm);
	}

	.card-main-title {
		font-family: var(--font-pixel);
		font-size: 15px;
		font-weight: 600;
		color: var(--text-primary);
		margin: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		letter-spacing: 0.05em;
		text-transform: uppercase;
		display: -webkit-box;
		-webkit-line-clamp: 2;
		line-clamp: 2;
		-webkit-box-orient: vertical;
		cursor: text;
	}

	.title-input {
		font-family: var(--font-pixel);
		font-size: 15px;
		font-weight: 600;
		color: var(--text-primary);
		background: var(--bg-base);
		border: 1px solid var(--status-input);
		padding: 4px 8px;
		margin: -4px -8px;
		letter-spacing: 0.05em;
		text-transform: uppercase;
		width: 100%;
		outline: none;
	}

	.title-input::placeholder {
		color: var(--text-muted);
		opacity: 0.5;
	}

	.session-name-badge {
		font-family: var(--font-mono);
		font-size: 11px;
		font-weight: 500;
		color: var(--text-muted);
		background: var(--bg-elevated);
		padding: 2px 6px;
		border: 1px solid var(--border-default);
		text-transform: uppercase;
		letter-spacing: 0.1em;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		display: inline-block;
		vertical-align: middle;
		max-width: 100%;
	}

	.git-branch {
		display: flex;
		align-items: center;
		gap: 4px;
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--text-muted);
		text-transform: lowercase;
		min-width: 0;
	}

	.git-branch svg {
		flex-shrink: 0;
	}

	.branch-name {
		overflow: hidden;
		white-space: nowrap;
		text-overflow: ellipsis;
		min-width: 0;
		max-width: 200px;
	}

	.time-badge {
		font-family: var(--font-mono);
		font-size: 12px;
		font-weight: 500;
		color: var(--text-muted);
		text-transform: uppercase;
		letter-spacing: 0.05em;
	}


	/* Status Label */
	.status-label {
		font-family: var(--font-mono);
		font-size: 12px;
		font-weight: 500;
		text-transform: uppercase;
		letter-spacing: 0.1em;
	}

	/* Task Preview */
	.task-preview {
		font-size: 14px;
		color: var(--text-secondary);
		line-height: 1.5;
		display: -webkit-box;
		-webkit-line-clamp: 2;
		line-clamp: 2;
		-webkit-box-orient: vertical;
		overflow: hidden;
		margin: var(--space-xs) 0;
	}

	.message-count {
		display: inline-flex;
		align-items: center;
		gap: 4px;
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--text-muted);
		text-transform: uppercase;
		letter-spacing: 0.05em;
	}

	/* Card Actions */
	.stats-row {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--space-md);
	}

	.stats-group {
		display: flex;
		align-items: center;
		gap: var(--space-md);
	}

	.card-actions-container {
		margin-top: auto;
		display: flex;
		justify-content: flex-end;
		padding-top: var(--space-sm);
	}

	.card-actions {
		display: flex;
		gap: var(--space-xs);
	}

	.action-btn {
		display: flex;
		align-items: center;
		gap: 6px;
		padding: 4px 8px;
		background: var(--bg-base);
		border: 1px solid var(--border-default);
		color: var(--text-muted);
		font-family: var(--font-mono);
		font-size: 10px;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.1em;
		transition: all 0.2s ease;
		cursor: pointer;
	}

	.action-btn:hover {
		background: var(--bg-card-hover);
		color: var(--text-primary);
		border-color: var(--text-muted);
	}

	.action-btn.danger:hover {
		color: var(--status-permission);
		border-color: var(--status-permission);
	}

	.action-btn.primary {
		background: var(--text-primary);
		color: var(--bg-base);
		border-color: var(--text-primary);
	}

	.action-btn.primary:hover {
		background: var(--text-secondary);
		border-color: var(--text-secondary);
	}

	.action-btn svg {
		flex-shrink: 0;
	}

	/* Compact Mode Styles */
	.session-card.compact {
		height: auto;
		min-height: auto;
		padding: var(--space-md);
		gap: var(--space-md);
		align-items: center;
	}

	.session-card.compact .session-name-badge {
		max-width: 150px;
	}

	.session-card.compact .card-body {
		gap: 4px;
		justify-content: center;
		padding-right: 32px;
	}

	.session-card.compact .card-main-title {
		font-size: 13px;
		-webkit-line-clamp: 1;
		line-clamp: 1;
		margin-bottom: 2px;
	}

	.session-card.compact .stats-row {
		flex-direction: column;
		align-items: flex-start;
		gap: 2px;
	}

	.session-card.compact .status-label {
		font-size: 10px;
		margin-top: 0;
	}

	.session-card.compact .stats-row {
		justify-content: flex-start;
		gap: var(--space-md);
	}

	.compact-actions {
		position: absolute;
		right: var(--space-md);
		top: 50%;
		transform: translateY(-50%);
	}

	.compact-actions .action-btn {
		background: transparent;
		border-color: transparent;
	}

	.compact-actions .action-btn:hover {
		background: var(--bg-elevated);
		border-color: var(--border-default);
		color: var(--text-primary);
	}

	.action-btn.icon-only {
		padding: 0;
		width: 28px;
		height: 28px;
		justify-content: center;
		border-radius: 4px;
	}

	/* ── Mobile Responsive ─────────────────────────────────────── */
	@media (max-width: 768px) {
		.session-card {
			height: auto;
			min-height: auto;
			padding: var(--space-md);
		}

		.card-main-title {
			font-size: 13px;
		}

		.stats-row {
			flex-wrap: wrap;
			gap: var(--space-xs);
		}

		.session-name-badge {
			max-width: 60%;
		}

		.branch-name {
			max-width: 150px;
		}

		.task-preview {
			font-size: 13px;
			-webkit-line-clamp: 2;
			line-clamp: 2;
		}

		.card-actions {
			flex-wrap: wrap;
		}
	}

</style>
