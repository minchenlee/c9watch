<script lang="ts">
	import { inAppNotifications } from '$lib/stores/sessions';
</script>

{#if $inAppNotifications.length > 0}
	<div class="toast-container">
		{#each $inAppNotifications as notif (notif.id)}
			<div class="toast" role="alert">
				<div class="toast-title">{notif.title}</div>
				<div class="toast-body">{notif.body}</div>
			</div>
		{/each}
	</div>
{/if}

<style>
	.toast-container {
		position: fixed;
		top: 16px;
		right: 16px;
		z-index: 10000;
		display: flex;
		flex-direction: column;
		gap: 8px;
		max-width: 360px;
	}

	.toast {
		background: var(--bg-elevated, #1a1a2e);
		border: 1px solid var(--status-input, #f5a623);
		padding: 12px 16px;
		animation: toast-in 0.3s ease-out;
	}

	@keyframes toast-in {
		from {
			opacity: 0;
			transform: translateX(100%);
		}
		to {
			opacity: 1;
			transform: translateX(0);
		}
	}

	.toast-title {
		font-family: var(--font-pixel, monospace);
		font-size: 12px;
		font-weight: 600;
		color: var(--text-primary, #fff);
		text-transform: uppercase;
		letter-spacing: 0.05em;
		margin-bottom: 4px;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.toast-body {
		font-family: var(--font-mono, monospace);
		font-size: 12px;
		color: var(--text-secondary, #aaa);
		line-height: 1.4;
	}
</style>
