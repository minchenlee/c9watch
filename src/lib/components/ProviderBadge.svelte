<script lang="ts">
	import type { SessionProvider, SessionSurface } from '$lib/types';
	import { providerLabel, surfaceLabel } from '$lib/provider';

	interface Props {
		provider?: SessionProvider;
		surface?: SessionSurface;
		compact?: boolean;
	}

	let { provider = 'claudeCode', surface, compact = false }: Props = $props();
	let normalized = $derived<SessionProvider>(provider === 'codex' ? 'codex' : 'claudeCode');
	let surfaceText = $derived(normalized === 'codex' ? surfaceLabel(surface) : null);
</script>

<span class="provider-stack" class:compact aria-label={`${providerLabel(normalized)} session${surfaceText ? `, ${surfaceText}` : ''}`}>
	<span class="provider-badge" class:codex={normalized === 'codex'}>{providerLabel(normalized)}</span>
	{#if surfaceText}<span class="surface-badge">{surfaceText}</span>{/if}
</span>

<style>
	.provider-stack { display: inline-flex; align-items: center; gap: 5px; min-width: 0; }
	.provider-badge, .surface-badge {
		display: inline-flex; align-items: center; height: 18px; padding: 0 6px;
		border: 1px solid var(--border-default); border-radius: 2px;
		font-family: var(--font-mono); font-size: 8px; font-weight: 700;
		letter-spacing: .08em; white-space: nowrap; color: var(--text-muted);
		background: color-mix(in srgb, var(--bg-elevated) 86%, transparent);
	}
	.provider-badge.codex { color: var(--accent-amber); border-color: color-mix(in srgb, var(--accent-amber) 42%, var(--border-default)); }
	.surface-badge { height: 16px; padding: 0 4px; border-style: dashed; font-size: 7px; color: var(--text-secondary); }
	.compact .provider-badge { height: 16px; font-size: 7px; }
	.compact .surface-badge { display: none; }
</style>
