<script lang="ts">
	import { providerFilter } from '$lib/stores/provider-filter';
	import type { ProviderFilter as ProviderFilterValue } from '$lib/provider';

	interface Props {
		compact?: boolean;
		variant?: 'segmented' | 'select';
	}
	let { compact = false, variant = 'segmented' }: Props = $props();
	const options: Array<{ value: ProviderFilterValue; label: string; short: string }> = [
		{ value: 'all', label: 'All providers', short: 'ALL' },
		{ value: 'claudeCode', label: 'Claude Code', short: 'CLAUDE CODE' },
		{ value: 'codex', label: 'Codex', short: 'CODEX' }
	];
</script>

{#if variant === 'select'}
	<label class="provider-select" class:compact>
		<span class="select-label">SHOW</span>
		<span class="select-shell">
			<select
				aria-label="Filter sessions by provider"
				value={$providerFilter}
				onchange={(event) => providerFilter.set(event.currentTarget.value as ProviderFilterValue)}
			>
				{#each options as option}
					<option value={option.value}>{option.short}</option>
				{/each}
			</select>
			<svg aria-hidden="true" viewBox="0 0 10 6" width="8" height="5">
				<path d="M1 1l4 4 4-4" fill="none" stroke="currentColor" stroke-width="1.5" />
			</svg>
		</span>
	</label>
{:else}
	<div class="provider-filter" class:compact role="group" aria-label="Filter sessions by provider">
		{#each options as option}
			<button type="button" class:active={$providerFilter === option.value} aria-pressed={$providerFilter === option.value} title={option.label} onclick={() => providerFilter.set(option.value)}>
				{compact ? option.short : option.label}
			</button>
		{/each}
	</div>
{/if}

<style>
	.provider-filter { display: inline-flex; padding: 2px; gap: 2px; border: 1px solid var(--border-muted); border-radius: 4px; background: var(--bg-base); }
	button { border: 0; border-radius: 2px; padding: 5px 8px; background: transparent; color: var(--text-muted); font: 700 9px/1 var(--font-mono); letter-spacing: .04em; cursor: pointer; transition: color var(--transition-fast), background var(--transition-fast); }
	button:hover { color: var(--text-primary); }
	button.active { color: var(--text-primary); background: var(--bg-elevated); box-shadow: inset 0 0 0 1px var(--border-default); }
	.compact button { padding: 4px 6px; font-size: 8px; }
	.provider-select { display: inline-flex; align-items: center; gap: 6px; color: var(--text-muted); }
	.select-label { font: 700 8px/1 var(--font-mono); letter-spacing: .08em; }
	.select-shell { position: relative; display: inline-flex; align-items: center; }
	select {
		height: 26px;
		min-width: 112px;
		appearance: none;
		border: 1px solid var(--border-muted);
		border-radius: 2px;
		padding: 0 24px 0 8px;
		background: var(--bg-base);
		color: var(--text-secondary);
		font: 700 9px/1 var(--font-mono);
		letter-spacing: .05em;
		cursor: pointer;
		transition: color var(--transition-fast), border-color var(--transition-fast), background var(--transition-fast);
	}
	select:hover { color: var(--text-primary); border-color: var(--border-default); background: var(--bg-elevated); }
	select:focus-visible { outline: none; color: var(--text-primary); border-color: var(--border-focus); }
	.select-shell svg { position: absolute; right: 8px; pointer-events: none; color: var(--text-muted); }
	.provider-select.compact { gap: 5px; }
	.compact .select-label { font-size: 7px; }
	.compact select { height: 23px; min-width: 96px; padding-inline: 7px 21px; font-size: 8px; }
	.compact .select-shell svg { right: 7px; }
	@media (max-width: 720px) { .provider-filter:not(.compact) button { padding-inline: 6px; font-size: 8px; } }
</style>
