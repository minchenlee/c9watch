<script lang="ts">
	import { bannerVisible, updateAvailable } from '$lib/stores/updater';

	interface Props {
		onViewDetails: () => void;
	}

	let { onViewDetails }: Props = $props();

	let visible = $derived($bannerVisible);
	let update = $derived($updateAvailable);
</script>

{#if visible && update}
	<div class="banner" role="status">
		<div class="banner-left">
			<span class="banner-label">Update</span>
			<span class="banner-version">
				<span class="banner-old">{update.currentVersion}</span>
				<span class="banner-arrow">→</span>
				<span class="banner-new">{update.version}</span>
			</span>
		</div>
		<button class="banner-button" onclick={onViewDetails}>View details</button>
	</div>
{/if}

<style>
	.banner {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--space-md);
		padding: var(--space-xs) var(--space-md);
		background: color-mix(in srgb, var(--accent-amber) 6%, transparent);
		border-bottom: 1px solid color-mix(in srgb, var(--accent-amber) 25%, transparent);
	}

	.banner-left {
		display: inline-flex;
		align-items: center;
		gap: var(--space-md);
		min-width: 0;
	}

	.banner-label {
		font-family: var(--font-pixel);
		font-size: 11px;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.1em;
		color: var(--accent-amber);
	}

	.banner-version {
		display: inline-flex;
		align-items: center;
		gap: var(--space-xs);
		font-family: var(--font-mono);
		font-size: 12px;
	}

	.banner-old {
		color: var(--text-muted);
		text-decoration: line-through;
	}

	.banner-arrow {
		color: var(--text-muted);
	}

	.banner-new {
		color: var(--accent-amber);
		font-weight: 600;
	}

	.banner-button {
		background: transparent;
		border: 1px solid color-mix(in srgb, var(--accent-amber) 35%, transparent);
		color: var(--accent-amber);
		padding: 4px 10px;
		font-family: var(--font-pixel);
		font-size: 10px;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.1em;
		cursor: pointer;
		transition: all var(--transition-fast);
		white-space: nowrap;
	}

	.banner-button:hover {
		background: color-mix(in srgb, var(--accent-amber) 15%, transparent);
		border-color: color-mix(in srgb, var(--accent-amber) 55%, transparent);
	}
</style>
