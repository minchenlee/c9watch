<script lang="ts">
	import { onMount, tick } from 'svelte';
	import { fade, scale } from 'svelte/transition';
	import { quintOut } from 'svelte/easing';
	import type { HistoryEntry, Conversation } from '$lib/types';
	import MessageBubble from './MessageBubble.svelte';
	import MessageNavMap from './MessageNavMap.svelte';
	import { createSlidingWindow, BATCH_SIZE, MAX_VISIBLE } from '$lib/slidingWindow.svelte';

	interface Props {
		entry: HistoryEntry;
		conversation: Conversation | null;
		searchQuery?: string;
		onclose: () => void;
	}

	let { entry, conversation, searchQuery, onclose }: Props = $props();

	let messagesContainer = $state<HTMLDivElement>(undefined!);
	let hasScrolledToBottom = $state(false);
	let showTools = $state(true);
	let showThinking = $state(true);
	let navSheetOpen = $state(false);
	let copied = $state(false);

	const sw = createSlidingWindow();

	let visibleMessages = $derived.by(() => {
		if (!conversation) return [];
		return sw.sliceMessages(conversation.messages);
	});

	function handleNavItemClick() {
		// Close the bottom sheet on mobile after navigating
		navSheetOpen = false;
	}

	onMount(() => {
		const handleKeydown = (e: KeyboardEvent) => {
			if (e.key === 'Escape') {
				handleClose();
			}
		};
		window.addEventListener('keydown', handleKeydown);
		return () => window.removeEventListener('keydown', handleKeydown);
	});

	function handleScroll() {
		if (!messagesContainer || !conversation) return;
		sw.handleScroll(messagesContainer, conversation.messages.length);
	}

	async function onExpandToIndex(index: number) {
		if (!messagesContainer || !conversation) return;
		await sw.scrollToIndex(index, messagesContainer, conversation.messages.length);
	}

	$effect(() => {
		if (conversation && conversation.messages.length > 0 && messagesContainer) {
			if (!hasScrolledToBottom) {
				if (searchQuery) {
					const queryLower = searchQuery.toLowerCase();
					const matchIndex = conversation!.messages.findIndex(
						(m) => (m.messageType === 'User' || m.messageType === 'Assistant') &&
							m.content.toLowerCase().includes(queryLower)
					);
					if (matchIndex >= 0) {
						sw.reset(conversation.messages.length, Math.max(0, matchIndex - 10));
					} else {
						sw.reset(conversation.messages.length);
					}
				} else {
					sw.reset(conversation.messages.length);
				}
				tick().then(() => {
					if (searchQuery) {
						// Find the first user/assistant message matching all query words.
						// Uses multi-word AND to stay consistent with Rust deep_search.
						const words = searchQuery.toLowerCase().split(/\s+/).filter(Boolean);
						const matchIndex = conversation!.messages.findIndex(
							(m) => {
								if (m.messageType !== 'User' && m.messageType !== 'Assistant') return false;
								const lower = m.content.toLowerCase();
								return words.some((w) => lower.includes(w));
							}
						);
						if (matchIndex >= 0) {
							scrollToMessageIndex(matchIndex);
						} else {
							messagesContainer.scrollTop = messagesContainer.scrollHeight;
						}
					} else {
						messagesContainer.scrollTop = messagesContainer.scrollHeight;
					}
					hasScrolledToBottom = true;
				});
			}
		}
	});

	async function scrollToMessageIndex(index: number) {
		if (!messagesContainer || !conversation) return;
		sw.expandToIndex(index, conversation.messages.length);
		await tick();
		const target = messagesContainer.querySelector(`[data-msg-index="${index}"]`) as HTMLElement | null;
		if (target) {
			target.scrollIntoView({ block: 'center' });
			target.classList.add('search-highlight');
			// Don't remove the class — animation-fill-mode: forwards holds the
			// end state (no box-shadow), so removing it would cause a re-render blink.
		} else {
			// Fallback: the matched message might be hidden (tool/thinking toggle off)
			messagesContainer.scrollTop = messagesContainer.scrollHeight;
		}
	}

	async function handleClose() {
		await sw.clearBeforeClose(conversation?.messages.length ?? 0);
		onclose();
	}

	function handleBackdropClick(e: MouseEvent) {
		if (e.target === e.currentTarget) {
			handleClose();
		}
	}

	async function copyResumeCommand() {
		const cmd = `cd "${entry.project}" && claude --resume ${entry.sessionId}`;
		await navigator.clipboard.writeText(cmd);
		copied = true;
		setTimeout(() => {
			copied = false;
		}, 1500);
	}
</script>

<!-- svelte-ignore a11y_click_events_have_key_events -->
<div
	class="overlay-backdrop"
	onclick={handleBackdropClick}
	role="dialog"
	aria-modal="true"
	aria-labelledby="overlay-title"
	tabindex="-1"
	transition:fade={{ duration: 200 }}
>
	<div class="overlay-layout">
		<div
			class="overlay-card"
			in:scale={{ start: 0.95, duration: 300, easing: quintOut }}
		>
			<!-- Header -->
			<header class="overlay-header" data-tauri-drag-region>
				<div class="header-left" data-tauri-drag-region>
					<div class="header-info">
						<div class="header-title">
							<h2 id="overlay-title" class="project-name">{entry.projectName}</h2>
						</div>
						<div class="header-meta">
							<span class="message-count">{#if conversation && conversation.messages.length > BATCH_SIZE}{sw.startIndex + 1}–{sw.endIndex} / {/if}{conversation?.messages.length ?? 0} messages</span>
						</div>
					</div>
				</div>

				<button
					type="button"
					class="resume-chip"
					class:copied
					onclick={copyResumeCommand}
					title="Click to copy resume command"
				>
					<span class="resume-label">{copied ? 'COPIED!' : 'RESUME'}</span>
					<code class="resume-cmd">cd "{entry.project}" && claude --resume {entry.sessionId}</code>
				</button>

				<div class="header-actions">
					<button
						type="button"
						class="header-button toggle-thinking"
						class:active={showThinking}
						onclick={() => showThinking = !showThinking}
						title={showThinking ? "Hide Thinking" : "Show Thinking"}
					>
						<span>◇</span>
					</button>
					<button
						type="button"
						class="header-button toggle-tools"
						class:active={showTools}
						onclick={() => showTools = !showTools}
						title={showTools ? "Hide Tools" : "Show Tools"}
					>
						<span>⚙</span>
					</button>
					<div class="header-divider"></div>
					<button type="button" class="close-button" onclick={handleClose} aria-label="Close">
						<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
							<line x1="18" y1="6" x2="6" y2="18" />
							<line x1="6" y1="6" x2="18" y2="18" />
						</svg>
					</button>
				</div>
			</header>

			<!-- Conversation Area -->
			<div class="conversation-area" bind:this={messagesContainer} onscroll={handleScroll}>
				{#if !conversation}
					<div class="loading-state">
						<p>Loading conversation...</p>
					</div>
				{:else if conversation.messages.length === 0}
					<div class="empty-state">
						<div class="empty-icon">
							<svg width="48" height="48" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5">
								<path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z" />
							</svg>
						</div>
						<p>No messages found</p>
					</div>
				{:else}
					<div class="messages">
						{#if sw.startIndex > 0}
							<div class="load-more-indicator">
								{sw.startIndex} earlier messages
							</div>
						{/if}
						{#each visibleMessages as message, i (sw.startIndex + i)}
							{#if (showTools || (message.messageType !== 'ToolUse' && message.messageType !== 'ToolResult')) && (showThinking || message.messageType !== 'Thinking')}
								<div data-msg-index={sw.startIndex + i}>
									<MessageBubble {message} />
								</div>
							{/if}
						{/each}
					</div>
				{/if}
			</div>

			<!-- Mobile: FAB to open nav sheet -->
			<button
				type="button"
				class="nav-fab"
				class:open={navSheetOpen}
				onclick={() => navSheetOpen = !navSheetOpen}
				title="Navigation"
			>
				<svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
					<line x1="3" y1="6" x2="21" y2="6" />
					<line x1="3" y1="12" x2="21" y2="12" />
					<line x1="3" y1="18" x2="21" y2="18" />
				</svg>
			</button>
		</div>

		<!-- Desktop: sidebar nav -->
		<div class="nav-map-side nav-desktop" in:scale={{ start: 0.95, duration: 300, easing: quintOut }}>
			<MessageNavMap {conversation} scrollContainer={messagesContainer} bind:showTools bind:showThinking {onExpandToIndex} />
		</div>

		<!-- Mobile: bottom sheet nav -->
		<!-- svelte-ignore a11y_click_events_have_key_events -->
		<!-- svelte-ignore a11y_no_static_element_interactions -->
		<div class="nav-sheet-backdrop" class:open={navSheetOpen} onclick={() => navSheetOpen = false}></div>
		<!-- svelte-ignore a11y_click_events_have_key_events -->
		<!-- svelte-ignore a11y_no_static_element_interactions -->
		<div class="nav-sheet" class:open={navSheetOpen} onclick={handleNavItemClick}>
			<div class="nav-sheet-handle">
				<div class="handle-bar"></div>
			</div>
			<MessageNavMap {conversation} scrollContainer={messagesContainer} bind:showTools bind:showThinking {onExpandToIndex} />
		</div>
	</div>
</div>

<style>
	.overlay-backdrop {
		position: fixed;
		inset: 0;
		background: var(--bg-overlay);
		display: flex;
		align-items: center;
		justify-content: center;
		z-index: 1000;
		padding: var(--space-2xl);
	}

	.overlay-layout {
		display: flex;
		align-items: flex-start;
		gap: var(--space-xl);
		width: 100%;
		max-width: 1100px;
		height: 85vh;
		max-height: 900px;
		pointer-events: none; /* Allow clicks through empty layout area */
	}

	.overlay-card {
		position: relative;
		flex: 1; /* Take up remaining space */
		height: 100%;
		background: var(--bg-card);
		border: 1px solid var(--border-default);
		display: flex;
		flex-direction: column;
		overflow: hidden;
		pointer-events: auto; /* Enable clicks on the card */
		box-shadow: 0 20px 50px rgba(0, 0, 0, 0.5);
	}

	.nav-map-side.nav-desktop {
		flex-shrink: 0;
		height: 100%;
		display: flex;
		flex-direction: column;
		pointer-events: auto;
	}

	/* Mobile bottom sheet elements — hidden on desktop */
	.nav-fab,
	.nav-sheet-backdrop,
	.nav-sheet {
		display: none;
	}

	/* Header */
	.overlay-header {
		display: flex;
		justify-content: space-between;
		align-items: center;
		gap: var(--space-md);
		padding: var(--space-lg) var(--space-xl);
		border-bottom: 1px solid var(--border-default);
	}

	.header-left {
		display: flex;
		align-items: center;
		gap: var(--space-md);
		flex-shrink: 0;
	}

	.header-info {
		display: flex;
		flex-direction: column;
		gap: 4px;
	}

	.header-title {
		display: flex;
		align-items: center;
		gap: var(--space-md);
	}

	.project-name {
		font-family: var(--font-sans);
		font-size: 16px;
		font-weight: 600;
		color: var(--text-primary);
		margin: 0;
		letter-spacing: 0.05em;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		max-width: 300px;
	}

	.header-meta {
		display: flex;
		align-items: center;
		gap: var(--space-sm);
		font-family: var(--font-mono);
		font-size: 12px;
		text-transform: uppercase;
		letter-spacing: 0.05em;
	}

	.message-count {
		color: var(--text-muted);
	}

	/* Resume chip */
	.resume-chip {
		flex: 1;
		display: flex;
		align-items: center;
		gap: var(--space-sm);
		background: rgba(255, 255, 255, 0.04);
		border: 1px solid var(--border-default);
		padding: var(--space-xs) var(--space-md);
		cursor: pointer;
		text-align: left;
		min-width: 0;
		transition: border-color var(--transition-fast);
		pointer-events: auto;
	}

	.resume-chip:hover {
		border-color: var(--accent-green);
	}

	.resume-chip.copied {
		border-color: var(--accent-green);
	}

	.resume-label {
		font-family: var(--font-pixel);
		font-size: 9px;
		color: var(--accent-green);
		letter-spacing: 0.1em;
		flex-shrink: 0;
	}

	.resume-cmd {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--text-secondary);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.header-actions {
		display: flex;
		align-items: center;
		gap: var(--space-xs);
		flex-shrink: 0;
	}

	.header-button {
		display: flex;
		align-items: center;
		justify-content: center;
		width: 32px;
		height: 32px;
		color: var(--text-muted);
		transition: color var(--transition-fast);
	}

	.header-button:hover {
		color: var(--text-primary);
	}

	.header-button span {
		font-family: var(--font-mono);
		font-size: 14px;
	}

	.header-button.active.toggle-thinking {
		color: var(--status-permission);
		opacity: 1;
	}

	.header-button.active.toggle-tools {
		color: var(--status-input);
		opacity: 1;
	}

	.header-divider {
		width: 1px;
		height: 16px;
		background: var(--border-default);
		margin: 0 var(--space-sm);
	}

	.close-button {
		display: flex;
		align-items: center;
		justify-content: center;
		width: 32px;
		height: 32px;
		color: var(--text-muted);
		transition: color var(--transition-fast);
	}

	.close-button:hover {
		color: var(--accent-red);
	}

	.conversation-area {
		flex: 1;
		overflow-y: auto;
		padding: var(--space-xl);
	}

	.messages {
		display: flex;
		flex-direction: column;
	}

	.load-more-indicator {
		text-align: center;
		padding: var(--space-md) 0;
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--text-muted);
		text-transform: uppercase;
		letter-spacing: 0.05em;
		opacity: 0.6;
	}

	/* Flash highlight for the matched search result message.
	   animation-fill-mode: forwards keeps the end state (no box-shadow)
	   so removing the class isn't needed and avoids a re-render blink. */
	.messages :global([data-msg-index].search-highlight .message-bubble) {
		animation: search-flash 2s ease-out forwards;
	}

	@keyframes search-flash {
		0%, 15% {
			box-shadow: inset 0 0 0 1px var(--accent-amber), 0 0 12px rgba(255, 191, 0, 0.25);
		}
		100% {
			box-shadow: none;
		}
	}

	.loading-state,
	.empty-state {
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		height: 100%;
		gap: var(--space-md);
		color: var(--text-muted);
	}

	.empty-icon {
		opacity: 0.3;
		margin-bottom: var(--space-sm);
	}

	/* ── Mobile Responsive ─────────────────────────────────────── */
	@media (max-width: 768px) {
		.overlay-backdrop {
			padding: 0;
		}

		.overlay-layout {
			max-width: 100%;
			height: 100vh;
			max-height: 100vh;
		}

		/* Hide the desktop sidebar nav on mobile */
		.nav-map-side.nav-desktop {
			display: none;
		}

		.overlay-card {
			border: none;
			box-shadow: none;
		}

		.overlay-header {
			padding: var(--space-md) var(--space-md);
			gap: var(--space-sm);
			flex-wrap: wrap;
		}

		.header-left {
			min-width: 0;
		}

		.project-name {
			font-size: 13px;
			max-width: none;
		}

		.header-meta {
			flex-wrap: wrap;
			font-size: 11px;
			gap: var(--space-xs);
		}

		.resume-chip {
			order: 3;
			width: 100%;
			flex-basis: 100%;
		}

		.header-actions {
			flex-shrink: 0;
		}

		.header-divider {
			display: none;
		}

		.header-button {
			width: 28px;
			height: 28px;
		}

		.close-button {
			width: 28px;
			height: 28px;
		}

		.conversation-area {
			padding: var(--space-md);
			padding-bottom: 72px; /* Space for FAB */
		}

		/* ── FAB (Floating Action Button) ────────────── */
		.nav-fab {
			display: flex;
			align-items: center;
			justify-content: center;
			position: fixed;
			bottom: 20px;
			right: 20px;
			width: 48px;
			height: 48px;
			background: var(--bg-card);
			border: 1px solid var(--border-default);
			color: var(--text-secondary);
			z-index: 1010;
			pointer-events: auto;
			box-shadow: 0 4px 16px rgba(0, 0, 0, 0.6);
			transition: all 0.2s ease;
		}

		.nav-fab:active {
			transform: scale(0.95);
		}

		.nav-fab.open {
			background: var(--text-primary);
			color: var(--bg-base);
			border-color: var(--text-primary);
		}

		/* ── Bottom Sheet Backdrop ────────────────────── */
		.nav-sheet-backdrop {
			display: block;
			position: fixed;
			inset: 0;
			background: rgba(0, 0, 0, 0.5);
			z-index: 1020;
			pointer-events: none;
			opacity: 0;
			transition: opacity 0.25s ease;
		}

		.nav-sheet-backdrop.open {
			pointer-events: auto;
			opacity: 1;
		}

		/* ── Bottom Sheet ─────────────────────────────── */
		.nav-sheet {
			display: flex;
			flex-direction: column;
			position: fixed;
			left: 0;
			right: 0;
			bottom: 0;
			height: 55vh;
			background: var(--bg-card);
			border-top: 1px solid var(--border-default);
			z-index: 1030;
			pointer-events: auto;
			transform: translateY(100%);
			transition: transform 0.3s cubic-bezier(0.32, 0.72, 0, 1);
			overflow: hidden;
		}

		.nav-sheet.open {
			transform: translateY(0);
		}

		.nav-sheet-handle {
			display: flex;
			justify-content: center;
			padding: var(--space-md) 0 var(--space-sm);
			flex-shrink: 0;
		}

		.handle-bar {
			width: 36px;
			height: 4px;
			background: var(--border-default);
			border-radius: 2px;
		}
	}
</style>
