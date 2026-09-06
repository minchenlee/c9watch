<script lang="ts">
	import { usagePreferences, saveUsagePreferences, type UsagePreferences, type UsageProvider } from '$lib/stores/usage-preferences';
	import UsageIndicator from './UsageIndicator.svelte';
	let error = $state<string | null>(null);
	function update(patch: Partial<UsagePreferences>) { error = saveUsagePreferences({ ...$usagePreferences, ...patch }); }
	const providers: { id: UsageProvider; name: string; color: string; sample: number }[] = [
		{ id: 'claudeCode', name: 'Claude Code', color: 'var(--accent-amber)', sample: 24 },
		{ id: 'codex', name: 'Codex', color: 'var(--accent-blue)', sample: 38 },
		{ id: 'cursor', name: 'Cursor', color: 'var(--accent-purple)', sample: 62 }
	];
</script>
<section class="usage-settings" aria-labelledby="usage-settings-title">
	<header><h2 id="usage-settings-title">Usage</h2><p>Choose how subscription usage appears in the toolbar and tray.</p></header>
	<div class="preview" aria-label="Usage appearance preview with sample values">
		<div class="preview-items" class:compact={$usagePreferences.percentages !== 'always'}>
			{#each providers.filter(provider => $usagePreferences.providers[provider.id]) as provider}
				<span class="preview-item" style:--usage-color={$usagePreferences.colors === 'monochrome' ? 'var(--text-primary)' : provider.color}>
					<UsageIndicator provider={provider.id} percent={provider.sample} showIcon={$usagePreferences.icons} />
					{#if $usagePreferences.percentages === 'always'}<span>{provider.sample}%</span>{/if}
				</span>
			{:else}<span class="empty-preview">No subscriptions selected</span>
			{/each}
		</div>
		<span class="preview-caption">Windowed preview · sample values</span>
	</div>
	<div class="setting-row"><div id="usage-percentages">Percentages<small>Auto shows percentages in fullscreen and in the tray.</small></div>
		<div class="options" role="group" aria-labelledby="usage-percentages">
			{#each [{ value: 'auto', label: 'Auto' }, { value: 'always', label: 'Always' }, { value: 'never', label: 'Hide' }] as option}
				<button class:active={$usagePreferences.percentages === option.value} aria-pressed={$usagePreferences.percentages === option.value} onclick={() => update({ percentages: option.value as UsagePreferences['percentages'] })}>{option.label}</button>
			{/each}
		</div>
	</div>
	<div class="setting-row"><div id="usage-colors">Indicator colors<small>Keep provider colors or use black and white.</small></div>
		<div class="options" role="group" aria-labelledby="usage-colors">
			<button class:active={$usagePreferences.colors === 'provider'} aria-pressed={$usagePreferences.colors === 'provider'} onclick={() => update({ colors: 'provider' })}>Provider</button>
			<button class:active={$usagePreferences.colors === 'monochrome'} aria-pressed={$usagePreferences.colors === 'monochrome'} onclick={() => update({ colors: 'monochrome' })}>Black &amp; white</button>
		</div>
	</div>
	<div class="setting-row"><span>Provider icons<small>Show each provider’s icon inside its indicator.</small></span>
		<button class:active={$usagePreferences.icons} aria-label="Provider icons" aria-pressed={$usagePreferences.icons} onclick={() => update({ icons: !$usagePreferences.icons })}>{$usagePreferences.icons ? 'On' : 'Off'}</button>
	</div>
	<div class="provider-group" role="group" aria-labelledby="usage-providers">
		<h3 id="usage-providers">Visible subscriptions</h3>
		{#each providers as provider}
			<div class="provider-row"><span>{provider.name}</span><button class:active={$usagePreferences.providers[provider.id]} aria-label={provider.name} aria-pressed={$usagePreferences.providers[provider.id]} onclick={() => update({ providers: { ...$usagePreferences.providers, [provider.id]: !$usagePreferences.providers[provider.id] } })}>{$usagePreferences.providers[provider.id] ? 'On' : 'Off'}</button></div>
		{/each}
	</div>
	<p class="save-note">Changes are saved automatically.</p>
	{#if error}<p class="error" role="alert">{error}</p>{/if}
</section>
<style>
	.usage-settings { width: 100%; color: var(--text-primary); font: 14px/1.5 var(--font-sans); }
	header { margin-bottom: var(--space-xl); }
	h2, h3 { margin: 0 0 var(--space-sm); font: 600 13px/1.5 var(--font-pixel); text-transform: uppercase; letter-spacing: .1em; }
	p { margin: 0; }
	header p, small, .save-note, .empty-preview { color: var(--text-description); font-size: 13px; line-height: 1.55; }
	.preview { display: flex; align-items: center; justify-content: space-between; flex-wrap: wrap; gap: var(--space-lg); min-height: 64px; padding: var(--space-lg); background: var(--bg-card); border: 1px solid var(--border-default); }
	.preview-items { display: flex; flex-wrap: wrap; align-items: center; gap: 12px; min-height: 26px; }
	.preview-items.compact { gap: 2px; }
	.preview-item { display: flex; align-items: center; gap: 5px; font: 11px var(--font-mono); }
	.preview-caption { font: 12px/1.5 var(--font-mono); color: var(--text-description); }
	.setting-row { display: flex; align-items: center; justify-content: space-between; gap: var(--space-xl); padding: var(--space-lg) 0; border-bottom: 1px solid var(--border-default); }
	small { display: block; max-width: 42ch; margin-top: var(--space-xs); }
	button { flex-shrink: 0; min-width: 42px; padding: 4px var(--space-sm); border: 1px solid var(--border-default); border-radius: 0; background: transparent; color: var(--text-description); font: 11px/1.5 var(--font-pixel); text-transform: uppercase; letter-spacing: .05em; cursor: pointer; transition: color var(--transition-fast), background var(--transition-fast); }
	button:hover { background: rgba(255,255,255,.08); color: var(--text-primary); }
	button.active { background: rgba(255,255,255,.1); color: var(--text-primary); }
	button:focus-visible { outline: 1px solid var(--border-focus); outline-offset: 0; }
	.options { display: inline-flex; flex-shrink: 0; border: 1px solid var(--border-default); }
	.options button { border: 0; }
	.provider-group { padding: var(--space-lg) 0; border-bottom: 1px solid var(--border-default); }
	.provider-row { display: flex; align-items: center; justify-content: space-between; gap: var(--space-lg); padding: var(--space-xs) 0; }
	.save-note { padding-top: var(--space-lg); }
	.error { margin-top: var(--space-md); color: var(--accent-red); font-size: 14px; overflow-wrap: anywhere; }
	@media(max-width: 640px) { .setting-row { align-items: flex-start; flex-wrap: wrap; gap: var(--space-md); } .options { flex-wrap: wrap; } }
	@media(prefers-reduced-motion: reduce) { button { transition: none; } }
</style>
