<script lang="ts">
	let { provider, percent, showIcon = true }: { provider: string; percent: number | null; showIcon?: boolean } = $props();
</script>
<svg viewBox="0 0 32 32" aria-hidden="true">
	<path class="track" d="M16 3.5 H28.5 V28.5 H3.5 V3.5 Z" />
	{#if percent !== null}
		<path class="fill" d="M16 3.5 H28.5 V28.5 H3.5 V3.5 Z" pathLength="100" stroke-dasharray={`${Math.max(0, Math.min(100, percent))} 100`} />
	{/if}
	{#if showIcon}
		<image href={provider === 'claudeCode' ? '/providers/claude.svg' : provider === 'codex' ? '/providers/openai.svg' : '/providers/cursor.svg'} x="8.5" y="8.5" width="15" height="15" />
	{/if}
</svg>
<style>
	svg { width: 26px; height: 26px; flex-shrink: 0; overflow: visible; }
	.track, .fill { fill: none; stroke-width: 2.5; }
	.track { stroke: var(--border-default, #333); }
	.fill { stroke: var(--usage-color, var(--text-primary)); opacity: var(--usage-opacity, 1); }
</style>
