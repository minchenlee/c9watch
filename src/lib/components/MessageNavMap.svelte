<script lang="ts">
	import type { Conversation, Message } from '$lib/types';

	interface Props {
		conversation: Conversation | null;
		scrollContainer: HTMLDivElement | null;
		showTools?: boolean;
		showThinking?: boolean;
		onExpandToIndex?: (index: number) => void;
	}

	let { conversation, scrollContainer, showTools = $bindable(true), showThinking = $bindable(true), onExpandToIndex }: Props = $props();

	// Filter for "milestone" messages - user messages, tool blocks, and thinking steps
	let items = $derived.by(() => {
		if (!conversation) return [];
		return conversation.messages
			.map((msg, index) => ({ msg, index }))
			.filter(({ msg }) =>
				msg.messageType === 'User' ||
				(showThinking && msg.messageType === 'Thinking') ||
				(showTools && msg.messageType === 'ToolUse' && msg.content?.length > 0)
			);
	});

	function getMessageIcon(message: Message): string {
		switch (message.messageType) {
			case 'User':
				return '→';
			case 'Thinking':
				return '◇';
			case 'ToolUse':
				return '⚙';
			default:
				return '•';
		}
	}

	function getMessageColor(message: Message): string {
		switch (message.messageType) {
			case 'User':
				return 'var(--text-primary)';
			case 'Thinking':
				return 'var(--status-permission)';
			case 'ToolUse':
				return 'var(--status-input)';
			default:
				return 'var(--text-muted)';
		}
	}

	async function scrollToMessage(index: number) {
		if (!scrollContainer) return;

		// Use data-msg-index attribute to find the right element.
		// children[index] doesn't work because hidden messages (tools/thinking
		// toggled off) create gaps between the array index and DOM position.
		const target = scrollContainer.querySelector(`[data-msg-index="${index}"]`) as HTMLElement | null;
		if (target) {
			target.scrollIntoView({ behavior: 'smooth', block: 'start' });
		} else if (onExpandToIndex) {
			onExpandToIndex(index);
		}
	}

	function truncateContent(content: string | undefined): string {
		if (!content) return '...';
		const clean = content.replace(/[#*`]/g, '').trim();
		return clean.length > 40 ? clean.substring(0, 40) + '...' : clean;
	}
</script>

<div class="nav-map-floating" class:hidden={!items.length}>
	<div class="nav-header">
		<span class="nav-title">Navigation</span>
		<span class="nav-count">{items.length} items</span>
	</div>
	<div class="nav-list">
		{#each items as { msg, index }}
			<!-- svelte-ignore a11y_click_events_have_key_events -->
			<!-- svelte-ignore a11y_no_static_element_interactions -->
			<div
				class="nav-item-descriptive"
				class:is-user={msg.messageType === 'User'}
				class:is-thinking={msg.messageType === 'Thinking'}
				style="--item-color: {getMessageColor(msg)}"
				onclick={() => scrollToMessage(index)}
			>
				<span class="nav-index">{index + 1}</span>
				<span class="nav-icon">{getMessageIcon(msg)}</span>
				<span class="nav-text">{truncateContent(msg.content)}</span>
				<div class="nav-indicator"></div>
			</div>
		{/each}
	</div>
</div>

<style>
	.nav-map-floating {
		width: 240px;
		height: fit-content;
		max-height: 100%;
		display: flex;
		flex-direction: column;
		gap: var(--space-md);
		padding: var(--space-lg) 0;
		background: var(--bg-card);
		border: 1px solid var(--border-default);
		pointer-events: auto;
	}

	.nav-map-floating.hidden {
		display: none;
	}

	.nav-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding-bottom: var(--space-sm);
		border-bottom: 1px solid var(--border-muted);
		margin: 0 var(--space-lg);
	}

	.nav-title {
		font-family: var(--font-pixel);
		font-size: 12px;
		text-transform: uppercase;
		letter-spacing: 0.1em;
		color: var(--text-muted);
	}

	.nav-count {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--text-muted);
		opacity: 0.5;
	}

	.nav-list {
		display: flex;
		flex-direction: column;
		gap: 2px;
		overflow-y: auto;
		padding-right: 0;
	}

	.nav-item-descriptive {
		position: relative;
		display: flex;
		align-items: baseline;
		gap: 3px;
		padding: 6px var(--space-sm) 6px var(--space-md);
		cursor: pointer;
		transition: all var(--transition-fast);
		border: 1px solid transparent;
	}

	.nav-item-descriptive:hover {
		background: rgba(255, 255, 255, 0.03);
		border-color: var(--border-muted);
	}

	.nav-index {
		font-family: var(--font-mono);
		font-size: 9px;
		color: var(--text-muted);
		opacity: 0.5;
		flex-shrink: 0;
		min-width: 1.8em;
		text-align: right;
	}

	.nav-icon {
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--item-color);
		flex-shrink: 0;
	}

	.nav-text {
		font-family: var(--font-mono);
		font-size: 13px;
		color: var(--text-secondary);
		line-height: 1.4;
		word-break: break-all;
	}

	.nav-item-descriptive:hover .nav-text {
		color: var(--text-primary);
	}

	.nav-indicator {
		position: absolute;
		left: 0;
		top: 50%;
		transform: translateY(-50%);
		width: 2px;
		height: 0;
		background: var(--item-color);
		transition: height var(--transition-fast);
	}

	.nav-item-descriptive:hover .nav-indicator {
		height: 60%;
	}

	.is-user {
		margin-bottom: 4px;
	}

	.is-thinking {
		opacity: 0.8;
		padding-left: calc(var(--space-lg) + var(--space-sm));
	}

	.is-thinking .nav-text {
		font-style: italic;
	}

	/* Scrollbar for nav list */
	.nav-list::-webkit-scrollbar {
		width: 2px;
	}
	.nav-list::-webkit-scrollbar-thumb {
		background: var(--border-default);
	}

	/* ── Mobile: inside bottom sheet ─────────────────────────── */
	@media (max-width: 768px) {
		.nav-map-floating {
			width: 100%;
			height: 100%;
			border: none;
			padding: 0 0 var(--space-lg);
		}

		.nav-header {
			margin: 0 var(--space-md);
		}

		.nav-item-descriptive {
			padding: 8px var(--space-md);
		}

		.nav-text {
			font-size: 14px;
		}
	}
</style>
