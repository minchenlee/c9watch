<script lang="ts">
	import { monitorClock } from '$lib/stores/monitor-clock';

	let { startedAt, compact = false }: { startedAt: number; compact?: boolean } = $props();
	let seconds = $derived(Math.max(0, Math.floor(($monitorClock - startedAt) / 1000)));
	let minutes = $derived(Math.floor(seconds / 60));
	let elapsed = $derived(
		minutes >= 60 ? `${Math.floor(minutes / 60)}h ${minutes % 60}m`
			: minutes > 0 ? `${minutes}m ${seconds % 60}s` : `${seconds}s`
	);
</script>

<span title={`Working since ${new Date(startedAt).toLocaleString()}`}>
	{compact ? elapsed : `Working ${elapsed}`}
</span>

<style>
	span {
		flex-shrink: 0;
		white-space: nowrap;
		font: 0.9em var(--font-mono);
		color: var(--text-secondary);
		font-variant-numeric: tabular-nums;
	}
</style>
