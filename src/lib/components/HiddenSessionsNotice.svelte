<script lang="ts">
	import type { HiddenClaudeSession } from '$lib/types';
	import { hiddenSessionsSummary } from '$lib/session-utils';

	interface Props {
		hidden: HiddenClaudeSession[];
	}

	let { hidden }: Props = $props();
	let summary = $derived(hiddenSessionsSummary(hidden));
</script>

{#if summary}
	<details class="hidden-notice">
		<summary>
			<svg class="notice-icon" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
				<path d="M17.94 17.94A10.07 10.07 0 0 1 12 20c-7 0-11-8-11-8a18.45 18.45 0 0 1 5.06-5.94" />
				<path d="M9.9 4.24A9.12 9.12 0 0 1 12 4c7 0 11 8 11 8a18.5 18.5 0 0 1-2.16 3.19" />
				<line x1="1" y1="1" x2="23" y2="23" />
			</svg>
			<span class="notice-text">{summary}</span>
		</summary>
		<ul class="hidden-list">
			{#each hidden as session (session.pid)}
				<li>
					<span class="hidden-pid">PID {session.pid}</span>
					<span class="hidden-cwd">{session.cwd ?? 'folder unknown'}</span>
					{#if session.parentClaudePid}
						<span class="hidden-parent">opened by PID {session.parentClaudePid}</span>
					{/if}
				</li>
			{/each}
		</ul>
	</details>
{/if}

<style>
	.hidden-notice {
		margin-top: var(--space-md);
		border: 1px solid var(--border-default);
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--text-muted);
	}

	summary {
		display: flex;
		align-items: center;
		gap: var(--space-sm);
		padding: var(--space-sm) var(--space-md);
		cursor: pointer;
		list-style: none;
	}

	summary::-webkit-details-marker {
		display: none;
	}

	summary:hover {
		color: var(--text-primary);
	}

	.notice-icon {
		flex-shrink: 0;
		color: var(--accent-amber);
	}

	.notice-text {
		line-height: 1.4;
	}

	.hidden-list {
		list-style: none;
		margin: 0;
		padding: 0 var(--space-md) var(--space-sm) calc(var(--space-md) + 14px + var(--space-sm));
		display: flex;
		flex-direction: column;
		gap: 4px;
	}

	.hidden-list li {
		display: flex;
		flex-wrap: wrap;
		gap: 2px var(--space-md);
		min-width: 0;
	}

	.hidden-pid {
		color: var(--text-primary);
	}

	.hidden-cwd {
		overflow-wrap: anywhere;
	}
</style>
