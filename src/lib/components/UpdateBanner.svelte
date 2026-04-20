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
		<div class="banner-content">
			<div class="banner-icon">
				<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
					<path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
					<polyline points="7 10 12 15 17 10" />
					<line x1="12" y1="15" x2="12" y2="3" />
				</svg>
			</div>
			<div class="banner-text">
				<div class="banner-title">Version {update.version} is available</div>
				<div class="banner-description">A new release of c9watch is ready to install.</div>
			</div>
			<button class="banner-button" onclick={onViewDetails}>View details</button>
		</div>
	</div>
{/if}

<style>
	.banner {
		background: color-mix(in srgb, var(--accent-amber) 8%, transparent);
		border-bottom: 1px solid color-mix(in srgb, var(--accent-amber) 20%, transparent);
		padding: var(--space-sm) var(--space-md);
	}

	.banner-content {
		display: flex;
		align-items: center;
		gap: var(--space-md);
	}

	.banner-icon {
		color: var(--accent-amber);
		flex-shrink: 0;
		display: flex;
	}

	.banner-text {
		flex: 1;
		min-width: 0;
	}

	.banner-title {
		font-family: var(--font-mono);
		font-size: 13px;
		font-weight: 600;
		color: var(--text-primary);
		margin-bottom: 2px;
		letter-spacing: 0.02em;
	}

	.banner-description {
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--text-muted);
	}

	.banner-button {
		background: color-mix(in srgb, var(--accent-amber) 15%, transparent);
		border: 1px solid color-mix(in srgb, var(--accent-amber) 35%, transparent);
		color: var(--accent-amber);
		padding: 6px 14px;
		font-family: var(--font-mono);
		font-size: 12px;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		cursor: pointer;
		transition: all var(--transition-fast);
		white-space: nowrap;
	}

	.banner-button:hover {
		background: color-mix(in srgb, var(--accent-amber) 25%, transparent);
		border-color: color-mix(in srgb, var(--accent-amber) 55%, transparent);
	}
</style>
