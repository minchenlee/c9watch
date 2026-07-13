<script lang="ts">
	import { providerFilter } from '$lib/stores/provider-filter';
	import type { ProviderFilter as ProviderFilterValue } from '$lib/provider';

	interface Props { compact?: boolean; }
	let { compact = false }: Props = $props();
	const options: Array<{ value: ProviderFilterValue; label: string; short: string }> = [
		{ value: 'all', label: 'All providers', short: 'ALL' },
		{ value: 'claudeCode', label: 'Claude Code', short: 'CLAUDE' },
		{ value: 'codex', label: 'Codex', short: 'CODEX' }
	];
</script>

<div class="provider-filter" class:compact role="group" aria-label="Filter sessions by provider">
	{#each options as option}
		<button type="button" class:active={$providerFilter === option.value} aria-pressed={$providerFilter === option.value} title={option.label} onclick={() => providerFilter.set(option.value)}>
			{compact ? option.short : option.label}
		</button>
	{/each}
</div>

<style>
	.provider-filter { display: inline-flex; padding: 2px; gap: 2px; border: 1px solid var(--border-muted); border-radius: 4px; background: var(--bg-base); }
	button { border: 0; border-radius: 2px; padding: 5px 8px; background: transparent; color: var(--text-muted); font: 700 9px/1 var(--font-mono); letter-spacing: .04em; cursor: pointer; transition: color var(--transition-fast), background var(--transition-fast); }
	button:hover { color: var(--text-primary); }
	button.active { color: var(--text-primary); background: var(--bg-elevated); box-shadow: inset 0 0 0 1px var(--border-default); }
	.compact button { padding: 4px 6px; font-size: 8px; }
	@media (max-width: 720px) { .provider-filter:not(.compact) button { padding-inline: 6px; font-size: 8px; } }
</style>
