<script lang="ts">
	import type { SessionProvider } from '$lib/types';
	import {
		conversationLoad,
		conversationLoadPercent,
		isSessionLoading
	} from '$lib/stores/conversation-loader';

	interface Props {
		sessionId: string;
		provider?: SessionProvider;
	}

	let { sessionId, provider }: Props = $props();

	let active = $derived(isSessionLoading(sessionId, provider, $conversationLoad));
	let percent = $derived(conversationLoadPercent($conversationLoad));
</script>

{#if active}
	<div
		class="load-track"
		role="progressbar"
		aria-valuemin={0}
		aria-valuemax={100}
		aria-valuenow={percent ?? undefined}
		aria-label="Loading conversation"
	>
		<div
			class="load-fill"
			class:indeterminate={percent == null}
			style={percent == null ? undefined : `width: ${percent}%`}
		></div>
	</div>
{/if}

<style>
	.load-track {
		height: 2px;
		background: var(--border-muted);
		overflow: hidden;
		flex-shrink: 0;
	}

	.load-fill {
		height: 100%;
		background: var(--status-working);
		transition: width 80ms linear;
	}

	.load-fill.indeterminate {
		width: 30%;
		animation: load-slide 1s linear infinite;
	}

	@keyframes load-slide {
		from {
			transform: translateX(-100%);
		}
		to {
			transform: translateX(350%);
		}
	}
</style>
