<script lang="ts">
	import { onMount } from 'svelte';
	import UsageIndicator from './UsageIndicator.svelte';
	import { usagePreferences, type UsageProvider } from '$lib/stores/usage-preferences';
	import { getSubscriptionUsage } from '$lib/api';
	import { isDemoMode } from '$lib/demo/mode';
	import { isTauri } from '$lib/ws';
	import type { SubscriptionUsage } from '$lib/subscription-usage';

	let { showPercentage = true, placement = 'bottom' }: { showPercentage?: boolean; placement?: 'top' | 'bottom' } = $props();
	let tooltipBottom = $state(8);
	let tooltipMaxHeight = $state(400);

	let usage = $state<SubscriptionUsage[]>([]);
	let loading = $state(true);
	let active = $state<string | null>(null);
	let dismissed = $state(false);
	let tooltipLeft = $state(8);
	let tooltipTop = $state(40);
	let now = $state(Date.now());
	let root: HTMLDivElement;
	const placeholders: SubscriptionUsage[] = [['claudeCode', 'Claude Code'], ['codex', 'Codex'], ['cursor', 'Cursor']].map(([provider, name]) => ({
		provider, name, plan: null, windows: [], updatedAt: null, message: null
	}));
	const percentagesVisible = $derived($usagePreferences.percentages === 'always' || ($usagePreferences.percentages === 'auto' && showPercentage));
	const rows = $derived((usage.length ? usage : placeholders).filter(item => $usagePreferences.providers[item.provider as UsageProvider] !== false));
	const selected = $derived(rows.find(item => item.provider === active));

	function expired(item: SubscriptionUsage) {
		return !!item.message || item.windows.some(w => w.resetsAt !== null && w.resetsAt * 1000 <= now)
			|| (item.updatedAt !== null && now - item.updatedAt * 1000 > 180_000);
	}
	function percentage(item: SubscriptionUsage) {
		return item.windows.length ? Math.max(...item.windows.map(w => w.usedPercent)) : null;
	}
	function open(provider: string, element: HTMLElement) {
		const rect = element.getBoundingClientRect();
		tooltipLeft = Math.max(8, Math.min(rect.right - 288, window.innerWidth - 296));
		tooltipTop = rect.bottom + 8;
		tooltipBottom = window.innerHeight - rect.top + 8;
		tooltipMaxHeight = Math.max(0, placement === 'top' ? rect.top - 16 : window.innerHeight - tooltipTop - 8);
		active = provider;
	}
	function resetLabel(timestamp: number | null) {
		if (timestamp === null) return 'Reset time unavailable';
		if (timestamp * 1000 <= now) return 'Reset passed · awaiting refresh';
		return `Resets ${new Date(timestamp * 1000).toLocaleString([], { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' })}`;
	}
	onMount(() => {
		let disposed = false;
		let generation = 0;
		let busy = false;
		async function refresh(modeChanged = false) {
			if (busy && !modeChanged) return;
			const request = ++generation;
			busy = true;
			if (modeChanged) { usage = []; loading = true; }
			try {
				const next = await getSubscriptionUsage();
				if (!disposed && request === generation) usage = next;
			} catch {
				if (!disposed && request === generation) usage = (usage.length ? usage : placeholders).map(item => ({
					...item, message: item.windows.length ? 'Last known usage · Connection failed. Retrying automatically.' : 'Usage unavailable. Check the desktop connection. Retrying automatically.'
				}));
			} finally {
				if (!disposed && request === generation) { loading = false; busy = false; now = Date.now(); }
			}
		}
		const unsubscribe = isDemoMode.subscribe(() => { void refresh(true); });
		const timer = setInterval(() => { now = Date.now(); if (isTauri() || !document.hidden) void refresh(); }, 60_000);
		const visibility = () => { if (!document.hidden) { now = Date.now(); void refresh(); } };
		document.addEventListener('visibilitychange', visibility);
		const focus = () => { now = Date.now(); void refresh(); };
		window.addEventListener('focus', focus);
		return () => { disposed = true; generation++; unsubscribe(); clearInterval(timer); document.removeEventListener('visibilitychange', visibility); window.removeEventListener('focus', focus); };
	});
</script>

<svelte:window onkeydown={(event) => { if (event.key === 'Escape') { active = null; dismissed = true; } }}
	onresize={() => active = null}
	onpointerdown={(event) => { if (root && !root.contains(event.target as Node)) active = null; }} />

<div class="subscription-usage" class:compact={!percentagesVisible} class:monochrome={$usagePreferences.colors === 'monochrome'} bind:this={root} role="group" aria-label="Subscription usage"
	onmouseleave={() => { active = null; dismissed = false; }}
	onfocusout={(event) => { if (!event.currentTarget.contains(event.relatedTarget as Node)) active = null; }}>
	{#each rows as item (item.provider)}
		{@const percent = percentage(item)}
		{@const stale = expired(item)}
		<button class="usage-button" data-provider={item.provider} class:warning={percent !== null && percent >= 80}
			class:exhausted={percent !== null && percent >= 100} class:stale class:open={active === item.provider}
			onkeydown={(event) => {
				// Keep dashboard shortcuts from consuming Tab while inspecting quotas.
				event.stopPropagation();
				if (event.key === 'Escape') { active = null; dismissed = true; }
			}}
			aria-label={`${item.name} subscription: ${loading ? 'loading' : percent === null ? 'usage unavailable' : `${Math.round(percent)}% used${stale ? ', outdated' : ''}`}`}
			aria-describedby={active === item.provider ? `usage-tooltip-${item.provider}` : undefined}
			onmouseenter={(event) => { if (!dismissed) open(item.provider, event.currentTarget); }}
			onfocus={(event) => { dismissed = false; open(item.provider, event.currentTarget); }}
			onclick={(event) => { dismissed = false; open(item.provider, event.currentTarget); }}>
			<UsageIndicator provider={item.provider} {percent} showIcon={$usagePreferences.icons} />
			{#if percentagesVisible}<span class="usage-value">{loading ? '…' : percent === null ? '—' : `${Math.round(percent)}%`}</span>{/if}
		</button>
	{/each}
	{#if selected}
		<div class="tooltip-position" class:above={placement === 'top'} data-provider={selected.provider} style:left={`${tooltipLeft}px`} style:top={placement === 'bottom' ? `${tooltipTop}px` : undefined} style:bottom={placement === 'top' ? `${tooltipBottom}px` : undefined}>
		<div class="usage-tooltip" id={`usage-tooltip-${selected.provider}`} role="tooltip"
			style:max-height={`${tooltipMaxHeight}px`}>
			<div class="tooltip-heading"><strong>{selected.name}</strong><span>{$isDemoMode ? 'DEMO' : selected.plan ?? 'SUBSCRIPTION'}</span></div>
			{#if loading}
				<p>Reading subscription usage…</p>
			{:else if selected.windows.length}
				{#each selected.windows as window}
					<div class="window">
						<div class="window-heading"><span>{window.label}</span><strong>{Math.round(window.usedPercent)}% used</strong></div>
						<div class="meter" class:warning={window.usedPercent >= 80} class:exhausted={window.usedPercent >= 100} aria-hidden="true">
							{#each Array(32) as _, index}
								<span class="meter-cell"><span style:width={`${Math.max(0, Math.min(1, window.usedPercent / 100 * 32 - index)) * 100}%`}></span></span>
							{/each}
						</div>
						<div class="reset">{resetLabel(window.resetsAt)}</div>
					</div>
				{/each}
				<p class="footnote">{selected.message ?? (expired(selected) ? 'Outdated snapshot · refreshing automatically' : selected.provider === 'claudeCode' ? 'Last reported by Claude Code. Outline shows the most-used limit.' : 'Outline shows the most-used limit.')}</p>
				{#if selected.updatedAt}<p class="updated">{$isDemoMode ? 'Sample usage' : 'Updated'} {new Date(selected.updatedAt * 1000).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}</p>{/if}
			{:else}
				<p class="unavailable">Usage unavailable</p>
				<p>{selected.message}</p>
				{#if selected.provider === 'claudeCode' && selected.updatedAt === null && !$isDemoMode}
					<p class="setup">Enable once in Terminal:<br /><code>c9watch usage-bridge --install</code></p>
				{/if}
			{/if}
		</div>
		</div>
	{/if}
</div>

<style>
	.subscription-usage { display: flex; align-items: center; gap: 10px; padding: 0 10px; -webkit-app-region: no-drag; }
	.subscription-usage.compact { gap: 2px; padding: 0 6px; }
	.usage-button { display: flex; align-items: center; gap: 4px; padding: 0; border: 0; background: transparent; color: var(--text-primary); cursor: pointer; font-family: var(--font-mono); }
	[data-provider="claudeCode"] { --usage-color: var(--accent-amber); }
	[data-provider="codex"] { --usage-color: var(--accent-blue); }
	[data-provider="cursor"] { --usage-color: var(--accent-purple); }
	.monochrome [data-provider] { --usage-color: var(--text-primary); }
	.usage-value { font-size: 10px; font-variant-numeric: tabular-nums; min-width: 26px; }
	.warning { color: #ffb547; }
	.exhausted { color: #ff6369; }
	.stale { --usage-opacity: 0.45; }
	.usage-button:hover, .usage-button.open { background: var(--bg-card-hover); }
	.usage-button:focus-visible { outline: 2px solid var(--text-primary); outline-offset: 3px; }
	.tooltip-position { position: fixed; z-index: 1100; width: 288px; max-width: calc(100vw - 16px); }
	.usage-tooltip { overflow-y: auto; padding: 16px; background: var(--bg-card); border: 1px solid var(--border-default, #333); color: var(--text-primary); font-family: var(--font-mono); -webkit-app-region: no-drag; }
	.tooltip-position::before { content: ''; position: absolute; left: 0; right: 0; top: -12px; height: 12px; }
	.tooltip-position.above::before { top: auto; bottom: -12px; }
	.tooltip-heading, .window-heading { display: flex; justify-content: space-between; align-items: baseline; gap: 12px; }
	.tooltip-heading strong { font-size: 14px; }
	.tooltip-heading > span { color: #aaa; font-size: 10px; text-transform: uppercase; }
	.window { margin-top: 18px; }
	.window-heading { font-size: 12px; }
	.window-heading strong { font-weight: 500; }
	.meter { display: grid; grid-template-columns: repeat(32, minmax(0, 1fr)); gap: 2px; margin: 8px 0; padding: 2px; border: 1px solid var(--border-default, #333); color: var(--usage-color, var(--text-primary)); }
	.meter.warning { color: #ffb547; }
	.meter.exhausted { color: #ff6369; }
	.meter-cell { display: block; height: 6px; background: var(--border-default, #333); }
	.meter-cell > span { display: block; height: 100%; background: currentColor; }
	.reset, p { color: #aaa; font-size: 11px; line-height: 1.6; }
	p { margin: 12px 0 0; }
	.footnote { border-top: 1px solid var(--border-default, #333); padding-top: 12px; }
	.updated { margin-top: 4px; font-size: 10px; }
	.setup code { font: inherit; color: var(--text-primary); user-select: text; -webkit-user-select: text; overflow-wrap: anywhere; }
	.unavailable { color: var(--text-primary); }
	@media (max-width: 800px) { .subscription-usage { gap: 6px; padding: 0 6px; } }
</style>
