<script lang="ts">
	import { tasksBySession } from '$lib/stores/tasks';
	import { flyInX } from '$lib/transitions';
	import type { Session, Task } from '$lib/types';
	import { providerOf, providerSessionKey } from '$lib/provider';

	interface Props {
		session: Session;
		collapsed?: boolean;
		onToggle?: () => void;
		/** Cascade index for the entry transition (parent owns the order). */
		transitionIndex?: number;
	}

	let { session, collapsed = false, onToggle, transitionIndex = 0 }: Props = $props();

	let tasks = $derived(providerOf(session) === 'claudeCode' ? ($tasksBySession.get(providerSessionKey('claudeCode', session.id)) ?? []) : []);
	let total = $derived(tasks.length);
	let completed = $derived(tasks.filter((t) => t.status === 'completed').length);

	function statusIcon(status: Task['status']): string {
		if (status === 'completed') return '✓';
		if (status === 'in_progress') return '⟳';
		return '◯';
	}
</script>

{#if total > 0}
	<div class="todo-side-panel" class:collapsed in:flyInX|global={{ index: transitionIndex }}>
		<button
			type="button"
			class="panel-header"
			onclick={onToggle}
			aria-expanded={!collapsed}
		>
			<span class="panel-chevron" class:rotated={collapsed}>▾</span>
			<span class="panel-title">Todos</span>
			<span class="panel-count">{completed}/{total}</span>
		</button>
		{#if !collapsed}
			<div class="todo-list">
				{#each tasks as t (t.id)}
					<div class="todo-row" class:completed={t.status === 'completed'}>
						<span class="todo-icon todo-icon-{t.status}">{statusIcon(t.status)}</span>
						<span class="todo-content">{t.status === 'in_progress' ? t.activeForm : t.subject}</span>
					</div>
				{/each}
			</div>
		{/if}
	</div>
{/if}

<style>
	.todo-side-panel {
		background: var(--bg-card);
		border: 0;
		border-top: 1px solid var(--border-default);
		display: flex;
		flex-direction: column;
		flex-shrink: 0;
	}

	.panel-header {
		display: flex;
		align-items: center;
		gap: var(--space-sm);
		width: 100%;
		background: none;
		border: none;
		border-bottom: 1px solid var(--border-muted);
		padding: var(--space-sm) var(--space-lg);
		cursor: pointer;
		text-align: left;
		transition: background var(--transition-fast);
	}

	.panel-header:hover {
		background: color-mix(in srgb, var(--text-muted) 6%, transparent);
	}

	.todo-side-panel.collapsed .panel-header {
		border-bottom: none;
	}

	.panel-chevron {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--text-muted);
		transition: transform 0.2s ease;
		display: inline-block;
		flex-shrink: 0;
	}

	.panel-chevron.rotated {
		transform: rotate(-90deg);
	}

	.panel-title {
		font-family: var(--font-pixel);
		font-size: 12px;
		text-transform: uppercase;
		letter-spacing: 0.1em;
		color: var(--text-muted);
		flex: 1;
	}

	.panel-count {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--text-muted);
		opacity: 0.5;
	}

	.todo-list {
		display: flex;
		flex-direction: column;
		gap: 2px;
		padding: var(--space-sm) 0;
	}

	.todo-row {
		display: flex;
		align-items: flex-start;
		gap: var(--space-sm);
		padding: 4px var(--space-lg);
		font-family: var(--font-mono);
		font-size: 13px;
		color: var(--text-secondary);
		word-break: break-word;
	}

	.todo-row.completed {
		text-decoration: line-through;
		opacity: 0.6;
	}

	.todo-icon {
		flex-shrink: 0;
		display: inline-block;
		width: 14px;
		text-align: center;
		font-family: var(--font-mono);
		font-size: 13px;
		line-height: 1.4;
	}

	.todo-icon-pending {
		color: var(--text-muted);
	}

	.todo-icon-in_progress {
		color: var(--status-input);
	}

	.todo-icon-completed {
		color: var(--accent-green);
	}

	.todo-content {
		flex: 1;
		min-width: 0;
		line-height: 1.4;
	}
</style>
