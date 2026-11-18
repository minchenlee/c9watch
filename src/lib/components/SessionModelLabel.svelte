<script lang="ts">
	interface Props {
		model?: string | null;
		reasoningEffort?: string | null;
	}

	let { model, reasoningEffort }: Props = $props();
</script>

{#if model || reasoningEffort}
	<span class="model-label" title={[model, reasoningEffort].filter(Boolean).join(' · ')}>
		{#if model}<span class="model">{model}</span>{/if}
		{#if model && reasoningEffort}<span aria-hidden="true">·</span>{/if}
		{#if reasoningEffort}<span>{reasoningEffort}</span>{/if}
	</span>
{/if}

<style>
	.model-label {
		display: inline-flex;
		align-items: center;
		gap: 0.35em;
		min-width: 0;
		font-family: var(--font-mono);
		font-size: 0.9em;
		color: var(--text-secondary);
		white-space: nowrap;
	}

	.model {
		overflow: hidden;
		text-overflow: ellipsis;
	}
</style>
