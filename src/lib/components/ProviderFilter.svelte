<script lang="ts">
	import { onMount } from 'svelte';
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
	let dropdownOpen = $state(false);
	let activeIndex = $state(0);
	let dropdownRoot = $state<HTMLDivElement>();
	let dropdownTrigger = $state<HTMLButtonElement>();
	const selectedOption = $derived(options.find((option) => option.value === $providerFilter) ?? options[0]);

	function openDropdown() {
		activeIndex = Math.max(0, options.findIndex((option) => option.value === $providerFilter));
		dropdownOpen = true;
	}

	function selectProvider(value: ProviderFilterValue) {
		providerFilter.set(value);
		dropdownOpen = false;
		dropdownTrigger?.focus();
	}

	function handleTriggerClick() {
		if (dropdownOpen) dropdownOpen = false;
		else openDropdown();
	}

	function handleTriggerKeydown(event: KeyboardEvent) {
		if (event.key === 'Escape') {
			event.preventDefault();
			dropdownOpen = false;
			return;
		}

		if (event.key === 'Enter' || event.key === ' ') {
			event.preventDefault();
			if (dropdownOpen) selectProvider(options[activeIndex].value);
			else openDropdown();
			return;
		}

		if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
			event.preventDefault();
			if (!dropdownOpen) {
				openDropdown();
				return;
			}
			const direction = event.key === 'ArrowDown' ? 1 : -1;
			activeIndex = (activeIndex + direction + options.length) % options.length;
		}
	}

	function handleFocusOut(event: FocusEvent) {
		const nextTarget = event.relatedTarget as Node | null;
		if (!nextTarget || !dropdownRoot?.contains(nextTarget)) dropdownOpen = false;
	}

	onMount(() => {
		function handleOutsidePointer(event: PointerEvent) {
			if (dropdownOpen && dropdownRoot && !dropdownRoot.contains(event.target as Node)) dropdownOpen = false;
		}
		function handleWindowBlur() { dropdownOpen = false; }
		document.addEventListener('pointerdown', handleOutsidePointer, true);
		window.addEventListener('blur', handleWindowBlur);
		return () => {
			document.removeEventListener('pointerdown', handleOutsidePointer, true);
			window.removeEventListener('blur', handleWindowBlur);
		};
	});
</script>

{#if variant === 'select'}
	<div class="provider-select" class:compact bind:this={dropdownRoot} onfocusout={handleFocusOut}>
		<span class="select-label">SHOW</span>
		<span class="select-shell">
			<button
				type="button"
				class="dropdown-trigger"
				class:open={dropdownOpen}
				role="combobox"
				aria-label="Filter sessions by provider"
				aria-expanded={dropdownOpen}
				aria-controls="provider-filter-options"
				aria-activedescendant={dropdownOpen ? `provider-option-${options[activeIndex].value}` : undefined}
				bind:this={dropdownTrigger}
				onclick={handleTriggerClick}
				onkeydown={handleTriggerKeydown}
			>
				<span>{selectedOption.short}</span>
				<svg aria-hidden="true" class:rotated={dropdownOpen} viewBox="0 0 10 6" width="8" height="5">
				<path d="M1 1l4 4 4-4" fill="none" stroke="currentColor" stroke-width="1.5" />
				</svg>
			</button>
			{#if dropdownOpen}
				<div id="provider-filter-options" class="dropdown-menu" role="listbox" aria-label="Provider options">
					{#each options as option, index}
						<button
							type="button"
							id={`provider-option-${option.value}`}
							class="dropdown-option"
							class:active={index === activeIndex}
							class:selected={$providerFilter === option.value}
							role="option"
							aria-selected={$providerFilter === option.value}
							tabindex="-1"
							onpointerenter={() => activeIndex = index}
							onclick={() => selectProvider(option.value)}
						>
							<span class="option-marker {option.value}" aria-hidden="true"></span>
							<span>{option.short}</span>
							<span class="option-check" aria-hidden="true">{$providerFilter === option.value ? '✓' : ''}</span>
						</button>
					{/each}
				</div>
			{/if}
		</span>
	</div>
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
	.provider-filter button { border: 0; border-radius: 2px; padding: 5px 8px; background: transparent; color: var(--text-muted); font: 700 9px/1 var(--font-mono); letter-spacing: .04em; cursor: pointer; transition: color var(--transition-fast), background var(--transition-fast); }
	.provider-filter button:hover { color: var(--text-primary); }
	.provider-filter button.active { color: var(--text-primary); background: var(--bg-elevated); box-shadow: inset 0 0 0 1px var(--border-default); }
	.compact button { padding: 4px 6px; font-size: 8px; }
	.provider-select { display: inline-flex; align-items: center; gap: 6px; color: var(--text-muted); }
	.select-label { font: 700 8px/1 var(--font-mono); letter-spacing: .08em; }
	.select-shell { position: relative; display: inline-flex; align-items: center; z-index: 10; }
	.dropdown-trigger {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 8px;
		height: 26px;
		min-width: 112px;
		border: 1px solid var(--border-muted);
		border-radius: 2px;
		padding: 0 8px;
		background: var(--bg-base);
		color: var(--text-secondary);
		font: 700 9px/1 var(--font-mono);
		letter-spacing: .05em;
		cursor: pointer;
		transition: color var(--transition-fast), background var(--transition-fast);
	}
	.dropdown-trigger:hover,
	.dropdown-trigger:focus-visible,
	.dropdown-trigger.open { outline: none; color: var(--text-primary); background: var(--bg-elevated); }
	.dropdown-trigger svg { flex-shrink: 0; color: var(--text-muted); transition: transform 160ms cubic-bezier(.22, 1, .36, 1); }
	.dropdown-trigger svg.rotated { transform: rotate(180deg); }
	.dropdown-menu {
		position: absolute;
		top: calc(100% + 4px);
		left: 0;
		width: 100%;
		padding: 3px;
		border: 1px solid var(--border-muted);
		border-radius: 2px;
		background: var(--bg-base);
		box-shadow: 0 4px 8px rgba(0, 0, 0, .55);
	}
	.dropdown-option {
		display: grid;
		grid-template-columns: 6px minmax(0, 1fr) 10px;
		align-items: center;
		gap: 6px;
		width: 100%;
		min-height: 24px;
		border: 0;
		border-radius: 1px;
		padding: 0 5px;
		background: transparent;
		color: var(--text-muted);
		font: 700 8px/1 var(--font-mono);
		letter-spacing: .04em;
		text-align: left;
		cursor: pointer;
	}
	.dropdown-option.active { color: var(--text-primary); background: var(--bg-elevated); }
	.dropdown-option.selected { color: var(--text-secondary); }
	.option-marker { width: 5px; height: 5px; border: 1px solid var(--border-default); }
	.option-marker.claudeCode { border-color: var(--accent-amber); background: var(--accent-amber); }
	.option-marker.codex { border-color: var(--accent-blue); background: var(--accent-blue); }
	.option-check { color: var(--text-secondary); text-align: right; }
	.provider-select.compact { gap: 5px; }
	.compact .select-label { font-size: 7px; }
	.compact .dropdown-trigger { height: 23px; min-width: 96px; padding-inline: 7px; font-size: 8px; }
	@media (prefers-reduced-motion: reduce) { .dropdown-trigger svg { transition: none; } }
	@media (max-width: 720px) { .provider-filter:not(.compact) button { padding-inline: 6px; font-size: 8px; } }
</style>
