<script lang="ts">
	import { onMount } from 'svelte';
	import { openUrl } from '@tauri-apps/plugin-opener';
	import { isTauri } from '$lib/ws';

	interface Props {
		fdaLikelyNeeded: boolean;
	}

	let { fdaLikelyNeeded }: Props = $props();
	let dismissed = $state(false);

	onMount(() => {
		dismissed = localStorage.getItem('fdaBannerDismissed') === 'true';
	});

	function dismiss() {
		dismissed = true;
		localStorage.setItem('fdaBannerDismissed', 'true');
	}

	async function openSettings() {
		try {
			await openUrl('x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles');
		} catch {
			await openUrl('x-apple.systempreferences:com.apple.preference.security');
		}
	}

	let isMac = $derived(
		typeof navigator !== 'undefined' && /Mac/.test(navigator.platform)
	);
	let visible = $derived(isTauri() && isMac && fdaLikelyNeeded && !dismissed);
</script>

{#if visible}
	<div class="banner">
		<div class="banner-content">
			<div class="banner-icon">
				<svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
					<rect x="3" y="11" width="18" height="11" rx="2" ry="2" />
					<path d="M7 11V7a5 5 0 0 1 10 0v4" />
				</svg>
			</div>
			<div class="banner-text">
				<div class="banner-title">Full Disk Access Required</div>
				<div class="banner-description">
					c9watch found Claude sessions but can't read their working directories.
					Grant Full Disk Access in System Settings.
					<button class="hint-link" onclick={dismiss}>Dismiss</button>
				</div>
			</div>
			<button class="banner-button" onclick={openSettings}>
				Open Settings
			</button>
		</div>
	</div>
{/if}

<style>
	.banner {
		background: rgba(255, 102, 0, 0.08);
		border-bottom: 1px solid rgba(255, 102, 0, 0.2);
		padding: 12px 16px;
	}

	.banner-content {
		display: flex;
		align-items: center;
		gap: 12px;
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
		font-size: 14px;
		font-weight: 500;
		color: rgba(255, 255, 255, 0.9);
		margin-bottom: 2px;
	}

	.banner-description {
		font-size: 12px;
		color: rgba(255, 255, 255, 0.6);
	}

	.hint-link {
		background: none;
		border: none;
		color: rgba(255, 255, 255, 0.4);
		font-size: 12px;
		cursor: pointer;
		text-decoration: underline;
		padding: 0;
		margin-left: 4px;
	}

	.hint-link:hover {
		color: rgba(255, 255, 255, 0.6);
	}

	.banner-button {
		background: rgba(255, 102, 0, 0.15);
		border: 1px solid rgba(255, 102, 0, 0.3);
		color: var(--accent-amber);
		padding: 6px 16px;
		border-radius: 6px;
		font-size: 13px;
		font-weight: 500;
		cursor: pointer;
		transition: all 0.2s ease;
		white-space: nowrap;
	}

	.banner-button:hover {
		background: rgba(255, 102, 0, 0.25);
		border-color: rgba(255, 102, 0, 0.5);
	}
</style>
