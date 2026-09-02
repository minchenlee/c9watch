<script module lang="ts">
	import { SvelteMap } from 'svelte/reactivity';
	import type { SubagentInfo } from '$lib/stores/subagents';

	export interface SubagentTranscript {
		id: string;
		agentType: string;
		description: string;
		parentSessionId: string;
		startedAt: string;
		completedAt: string | null;
		status: 'running' | 'completed';
		prompt: string;
		result: string | null;
		agentId: string | null;
		totalTokens: number | null;
		toolUses: number | null;
		durationMs: number | null;
	}

	interface PreviewState {
		selection: SubagentInfo | null;
		transcript: SubagentTranscript | null;
		loading: boolean;
		error: string | null;
	}

	// Per-parent-session preview state. Module-scoped so re-renders triggered
	// by store updates (new JSONL events) don't reset what the user had open.
	const previewStateBySession = new SvelteMap<string, PreviewState>();
</script>

<script lang="ts">
	import { onMount, tick } from 'svelte';
	import { fade, scale } from 'svelte/transition';
	import { quintOut } from 'svelte/easing';
	import { flyIn, flyInX, fadeIn } from '$lib/transitions';
	import { invoke } from '@tauri-apps/api/core';
	import type { Session, Conversation } from '$lib/types';
	import { SessionStatus } from '$lib/types';
	import MessageBubble from './MessageBubble.svelte';
	import MessageNavMap from './MessageNavMap.svelte';
	import TodoPanel from './TodoPanel.svelte';
	import { createSlidingWindow, BATCH_SIZE } from '$lib/slidingWindow.svelte';
	import { sessionCostMap, costMode } from '$lib/stores/cost';
	import { workersByPm, expandedSessionId, codexSubagentsByParent, codexVisibleParentByChild } from '$lib/stores/sessions';
	import { visibleSubagentsBySession } from '$lib/stores/subagents';
	import { PM_ORCHESTRATION_ENABLED } from '$lib/feature-flags';
	import { formatCost, formatTokens, formatCostOrTokens, modelDisplayName } from '$lib/cost-utils';
	import { isCostAvailable } from '$lib/cost-semantics';
	import { formatTimeSince, formatDurationMs } from '$lib/time-utils';
	import { getSessionStatusColor, getSessionStatusLabel, getSubagentStatusColor, getSubagentStatusLabel, getWorkerStatusColor, getWorkerStatusLabel } from '$lib/status-utils';
	import { isTauri } from '$lib/ws';
	import ProviderBadge from './ProviderBadge.svelte';
	import { canSessionAction, providerOf, providerSessionKey, sessionKeyOf } from '$lib/provider';

	interface Props {
		session: Session;
		conversation: Conversation | null;
		onclose?: () => void;
		onstop?: () => void;
		onopen?: () => void;
	}

	let { session, conversation, onclose, onstop, onopen }: Props = $props();

	let messagesContainer = $state<HTMLDivElement>(undefined!);
	let isInitialLoad = $state(true);
	let hasScrolledToBottom = $state(false);
	let showTools = $state(true);
	let showThinking = $state(true);
	let navSheetOpen = $state(false);
	let idCopied = $state(false);
	let tooltipText = $state('');
	let tooltipX = $state(0);
	let tooltipY = $state(0);
	let navCollapsed = $state(false);
	let tasksCollapsed = $state(false);
	let workersCollapsed = $state(false);
	// Only stagger the first batch of messages when the overlay mounts;
	// subsequent streaming/polling of new messages should NOT animate.
	let initialStaggerDone = $state(false);

	function tipEnter(text: string) { tooltipText = text; }
	function tipLeave() { tooltipText = ''; }
	function tipMove(e: MouseEvent) { tooltipX = e.clientX + 12; tooltipY = e.clientY + 12; }

	const sw = createSlidingWindow();

	let visibleMessages = $derived.by(() => {
		if (!conversation) return [];
		return sw.sliceMessages(conversation.messages);
	});

	let navItemCount = $derived.by(() => {
		if (!conversation) return 0;
		return conversation.messages.filter(msg =>
			msg.messageType === 'User' ||
			msg.messageType === 'System' ||
			(showThinking && msg.messageType === 'Thinking') ||
			(showTools && msg.messageType === 'ToolUse' && (msg.content?.length ?? 0) > 0)
		).length;
	});

	// Stagger messages only on the overlay's first render. After that,
	// streaming/polling appends new messages — those should appear instantly.
	function messageIn(node: Element, params: { index: number }) {
		if (initialStaggerDone) return fade(node, { duration: 0 });
		return flyIn(node, { index: params.index, y: 6, stride: 20, duration: 200 });
	}

	function handleNavItemClick() {
		// Close the bottom sheet on mobile after navigating
		navSheetOpen = false;
	}

	onMount(() => {
		isInitialLoad = false;

		// Give the first paint of visible messages time to stagger in,
		// then disable the animation so new streamed messages just appear.
		setTimeout(() => { initialStaggerDone = true; }, 600);

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
				sw.reset(conversation.messages.length);
				tick().then(() => {
					messagesContainer.scrollTop = messagesContainer.scrollHeight;
					hasScrolledToBottom = true;
				});
			} else {
				// Auto-scroll logic for new messages
				const isAtBottom =
					messagesContainer.scrollHeight - messagesContainer.scrollTop - messagesContainer.clientHeight < 150;
				if (isAtBottom) {
					tick().then(() => {
						messagesContainer.scrollTop = messagesContainer.scrollHeight;
					});
				}
			}
		}
	});

	let costRecord = $derived($sessionCostMap.get(sessionKeyOf(session)));
	let primaryCostLabel = $derived(
		costRecord ? formatCostOrTokens(costRecord.cost, costRecord.totalTokens, $costMode, isCostAvailable(costRecord)) : null
	);

	let isPermission = $derived(session.status === SessionStatus.NeedsAttention);
	let isWaitingInput = $derived(session.status === SessionStatus.WaitingForInput);
	let isWorking = $derived(session.status === SessionStatus.Working);

	// resolvedWorkers works for both PM (views own workers) and a worker
	// (views siblings under the same PM). Highlights the current session in the list.
	// When the session is a PM, workerOf is null, so we fall back to its
	// provider-scoped key
	// and resolvedWorkers IS myWorkers; the PM badge shows when length > 0 and
	// workerOf is null (i.e. this session is itself the PM).
	let resolvedWorkers = $derived(
		$workersByPm.get(session.workerOf ? providerSessionKey('claudeCode', session.workerOf) : sessionKeyOf(session)) ?? []
	);
	let isPm = $derived(
		PM_ORCHESTRATION_ENABLED && !session.workerOf && resolvedWorkers.length > 0
	);
	let showWorkersPanel = $derived(
		PM_ORCHESTRATION_ENABLED && resolvedWorkers.length > 0
	);

	function subagentKey(subagent: SubagentInfo): string {
		return providerSessionKey(subagent.provider, subagent.id);
	}

	let mySubagents = $derived([
		// The Claude subagent endpoint is Claude-only and still keys its response
		// by the raw Claude session ID. Never consult it for another provider.
		...(providerOf(session) === 'claudeCode' ? ($visibleSubagentsBySession.get(providerSessionKey('claudeCode', session.id)) ?? []) : []),
		...($codexSubagentsByParent.get(sessionKeyOf(session)) ?? []).map((child): SubagentInfo => ({
			id: child.id,
			sessionId: child.id,
			provider: providerOf(child),
			agentType: child.agentRole || child.agentNickname || 'subagent',
				description: child.agentNickname || child.codexTitle || child.cursorTitle || child.summary || child.firstPrompt || child.agentRole || (providerOf(child) === 'cursor' ? 'Cursor subagent' : 'Codex subagent'),
			startedAt: child.startedAtMs ? new Date(child.startedAtMs).toISOString() : child.modified,
			completedAt: child.status === SessionStatus.Working ? null : child.modified,
			parentSessionId: session.id,
			status: child.status === SessionStatus.Working || child.status === SessionStatus.Connecting ? 'running' : 'completed'
		}))
	]);
	let hasSubagents = $derived(mySubagents.length > 0);
	let visibleCodexParentId = $derived($codexVisibleParentByChild.get(sessionKeyOf(session)) ?? null);
	let subagentsCollapsed = $state(false);

	// Subagent preview: when set, the conversation area renders a synthesized
	// transcript (prompt + result) for the clicked subagent instead of the
	// parent session's messages.
	//
	// State is keyed by the parent provider-scoped key in a module-level Svelte rune map so
	// that re-renders triggered by store updates (new JSONL events arriving)
	// do NOT reset the preview. Switching sessions naturally shows a fresh
	// (empty) preview because the key changes.
	let previewState = $derived(
		previewStateBySession.get(sessionKeyOf(session)) ?? {
			selection: null,
			transcript: null,
			loading: false,
			error: null,
		}
	);
	let previewedSubagent = $derived(previewState.selection);
	let previewTranscript = $derived(previewState.transcript);
	let previewLoading = $derived(previewState.loading);
	let previewError = $derived(previewState.error);

	async function openSubagentPreview(sa: SubagentInfo) {
		if ((sa.provider === 'codex' || sa.provider === 'cursor') && sa.sessionId) {
			expandedSessionId.set(providerSessionKey(sa.provider, sa.sessionId));
			return;
		}
		const sid = sessionKeyOf(session);
		const rawParentSessionId = session.id;
		const initialState: PreviewState = {
			selection: sa,
			transcript: null,
			loading: true,
			error: null,
		};
		previewStateBySession.set(sid, initialState);

		if (!isTauri()) {
			previewStateBySession.set(sid, {
				...initialState,
				loading: false,
				error: 'Subagent preview is only available in the desktop app.',
			});
			return;
		}

		try {
			const tr = await invoke<SubagentTranscript>('get_subagent_transcript', {
				parentSessionId: rawParentSessionId,
				subagentId: sa.id,
			});
			// Guard: user may have switched to a different selection before
			// this resolved. Only commit if the selection is still this one.
			const current = previewStateBySession.get(sid);
			if (current?.selection && subagentKey(current.selection) === subagentKey(sa)) {
				previewStateBySession.set(sid, {
					...current,
					transcript: tr,
				});
			}
		} catch (err) {
			const current = previewStateBySession.get(sid);
			if (current?.selection && subagentKey(current.selection) === subagentKey(sa)) {
				previewStateBySession.set(sid, {
					...current,
					error: String(err),
				});
			}
		} finally {
			const current = previewStateBySession.get(sid);
			if (current?.selection && subagentKey(current.selection) === subagentKey(sa)) {
				previewStateBySession.set(sid, {
					...current,
					loading: false,
				});
			}
		}
	}

	function closeSubagentPreview() {
		const sid = sessionKeyOf(session);
		previewStateBySession.delete(sid);
	}

	function openWorker(worker: Session) {
		expandedSessionId.set(sessionKeyOf(worker));
	}


	async function handleClose() {
		await sw.clearBeforeClose(conversation?.messages.length ?? 0);
		onclose?.();
	}

	async function copySessionId() {
		try {
			await navigator.clipboard.writeText(session.id);
			idCopied = true;
			tooltipText = 'Copied!';
			setTimeout(() => { idCopied = false; tooltipText = ''; }, 1500);
		} catch { /* clipboard API may fail */ }
	}

	function handleBackdropClick(e: MouseEvent) {
		if (e.target === e.currentTarget) {
			handleClose();
		}
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
			class:permission={isPermission}
			class:waiting={isWaitingInput}
			in:scale={{ start: 0.95, duration: 300, easing: quintOut }}
		>
			<!-- Header -->
			<header class="overlay-header" data-tauri-drag-region>
				<div class="header-left" data-tauri-drag-region>

					<div class="header-info">
						<div class="header-title">
							<ProviderBadge provider={session.provider} surface={session.surface} />
							<!-- svelte-ignore a11y_no_static_element_interactions -->
							<h2
								id="overlay-title"
								class="project-name"
								onmouseenter={() => tipEnter(session.id)}
								onmouseleave={tipLeave}
								onmousemove={tipMove}
							>{session.customTitle || session.officialName || session.codexTitle || session.cursorTitle || session.summary || session.firstPrompt || 'New Session'}</h2>
							{#if PM_ORCHESTRATION_ENABLED && session.workerOf}
								<!-- svelte-ignore a11y_no_static_element_interactions -->
								<span
									class="worker-badge"
									onmouseenter={() => tipEnter(`Worker of ${session.workerOf}`)}
									onmouseleave={tipLeave}
									onmousemove={tipMove}
								>WORKER</span>
							{/if}
							{#if isPm}
								<!-- svelte-ignore a11y_no_static_element_interactions -->
								<span
									class="pm-badge"
									onmouseenter={() => tipEnter(`Managing ${resolvedWorkers.length} worker${resolvedWorkers.length === 1 ? '' : 's'}`)}
									onmouseleave={tipLeave}
									onmousemove={tipMove}
								>PM · {resolvedWorkers.length}</span>
							{/if}
							<!-- svelte-ignore a11y_click_events_have_key_events -->
							<!-- svelte-ignore a11y_no_static_element_interactions -->
							<span
								class="copy-id-btn"
								class:copied={idCopied}
								onclick={copySessionId}
								onmouseenter={() => tipEnter('Copy session ID')}
								onmouseleave={tipLeave}
								onmousemove={tipMove}
							>
								{#if idCopied}
									<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><polyline points="20 6 9 17 4 12" /></svg>
								{:else}
									<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="9" y="9" width="13" height="13" rx="2" ry="2" /><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" /></svg>
								{/if}
							</span>
						</div>
						<div class="header-meta">
							<span class="status-label" style="color: {getSessionStatusColor(session.status)}">{getSessionStatusLabel(session.status, session.pendingToolName)}</span>
							<span class="separator">·</span>
							<span class="session-name-badge">{session.sessionName}</span>
							<span class="separator">·</span>
							<span class="message-count">{#if conversation && conversation.messages.length > BATCH_SIZE}{sw.startIndex + 1}–{sw.endIndex} / {/if}{conversation?.messages.length ?? 0} messages</span>
							{#if costRecord}
								<span class="separator">·</span>
								<span class="cost-breakdown" title="{isCostAvailable(costRecord) ? `Total cost: ${formatCost(costRecord.cost)}` : 'USD pricing unavailable'} · {formatTokens(costRecord.totalTokens)} tokens · {modelDisplayName(costRecord.model)}">
									<span class="cost-primary">{primaryCostLabel}</span>
									<span class="cost-secondary">· {$costMode === 'usd' ? formatTokens(costRecord.totalTokens) + ' tok' : formatCostOrTokens(costRecord.cost, costRecord.totalTokens, 'usd', isCostAvailable(costRecord))}</span>
									<span class="cost-secondary">· {modelDisplayName(costRecord.model)}</span>
								</span>
							{/if}
							{#if session.gitBranch}
								<span class="separator">·</span>
								<div class="git-info">
									<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
										<line x1="6" y1="3" x2="6" y2="15" />
										<circle cx="18" cy="6" r="3" />
										<circle cx="6" cy="18" r="3" />
										<path d="M18 9a9 9 0 0 1-9 9" />
									</svg>
									<span class="branch-name" title={session.gitBranch}>{session.gitBranch}</span>
								</div>
							{/if}
						</div>
					</div>
				</div>
				<div class="header-actions">
						{#if visibleCodexParentId}
							<button type="button" class="header-button" onclick={() => expandedSessionId.set(visibleCodexParentId)} title="Back to parent session" aria-label="Back to parent session">←</button>
						{/if}
						{#if canSessionAction(session, 'stop')}
							<button type="button" class="header-button" onclick={() => onstop?.()} title="Stop Session">
						<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
							<rect x="6" y="6" width="12" height="12" rx="1" />
						</svg>
							</button>
						{/if}
						{#if canSessionAction(session, 'open')}
							<button type="button" class="header-button" onclick={() => onopen?.()} title="Open in IDE">
						<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
						<polyline points="12 6 18 6 18 12" />
							<line x1="7" y1="17" x2="18" y2="6" />
						</svg>
							</button>
						{/if}
					<div class="header-divider"></div>
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

			{#if tooltipText}
				<div class="id-tooltip" style="left: {tooltipX}px; top: {tooltipY}px;">
					{tooltipText}
				</div>
			{/if}

			<!-- Conversation Area -->
			<div class="conversation-area" bind:this={messagesContainer} onscroll={handleScroll}>
				{#if previewedSubagent}
					<div class="subagent-preview">
						<div class="subagent-preview-header">
							<div class="subagent-preview-title">
								{previewedSubagent.description || previewedSubagent.agentType}
							</div>
							<div class="subagent-preview-meta">
								<span class="subagent-type">{previewedSubagent.agentType}</span>
								<span class="subagent-sep">·</span>
								<span class="subagent-status" style="color: {getSubagentStatusColor(previewedSubagent.status)}">
									{getSubagentStatusLabel(previewedSubagent.status)}
								</span>
								<span class="subagent-sep">·</span>
								<span class="subagent-time">{formatTimeSince(previewedSubagent.startedAt)}</span>
							</div>
						</div>
						{#if previewLoading}
							<div class="loading-state"><p>Loading subagent transcript...</p></div>
						{:else if previewError}
							<div class="empty-state"><p>{previewError}</p></div>
						{:else if previewTranscript}
							<div class="subagent-messages">
								<div class="subagent-msg subagent-msg-user">
									<div class="subagent-msg-role">Prompt</div>
									<pre class="subagent-msg-body">{previewTranscript.prompt || '(no prompt recorded)'}</pre>
								</div>
								{#if previewTranscript.result}
									<div class="subagent-msg subagent-msg-assistant">
										<div class="subagent-msg-role">Result</div>
										<pre class="subagent-msg-body">{previewTranscript.result}</pre>
									</div>
								{:else}
									<div class="subagent-msg subagent-msg-assistant pending">
										<div class="subagent-msg-role">Result</div>
										<p class="subagent-msg-pending">Still running — no result yet.</p>
									</div>
								{/if}
								{#if previewTranscript.totalTokens != null || previewTranscript.toolUses != null || previewTranscript.durationMs != null}
									<div class="subagent-stats">
										{#if previewTranscript.totalTokens != null}
											<span>{formatTokens(previewTranscript.totalTokens)} tokens</span>
										{/if}
										{#if previewTranscript.toolUses != null}
											<span class="subagent-sep">·</span>
											<span>{previewTranscript.toolUses} tool uses</span>
										{/if}
										{#if previewTranscript.durationMs != null}
											<span class="subagent-sep">·</span>
											<span>{formatDurationMs(previewTranscript.durationMs)}</span>
										{/if}
										{#if previewTranscript.agentId}
											<span class="subagent-sep">·</span>
											<span class="subagent-agent-id" title="Agent handle">{previewTranscript.agentId}</span>
										{/if}
									</div>
								{/if}
							</div>
						{/if}
					</div>
				{:else if !conversation}
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
						<p>No messages yet</p>
						<p class="empty-hint">Send a message to start the conversation</p>
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
								<div data-msg-index={sw.startIndex + i} in:messageIn={{ index: Math.min(i, 15) }}>
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

		<!-- Desktop: right column (nav + workers stacked) -->
		<div class="right-column nav-desktop">
			<!-- Navigation panel -->
			<div class="nav-map-side" class:collapsed={navCollapsed} in:flyInX|global={{ index: 0 }}>
				<button
					type="button"
					class="panel-header"
					onclick={() => navCollapsed = !navCollapsed}
					aria-expanded={!navCollapsed}
				>
					<span class="panel-chevron" class:rotated={navCollapsed}>▾</span>
					<span class="panel-title">Navigation</span>
					<span class="panel-count">{navItemCount}</span>
				</button>
				{#if !navCollapsed}
					<div class="panel-body">
						<MessageNavMap {conversation} scrollContainer={messagesContainer} bind:showTools bind:showThinking {onExpandToIndex} hideHeader />
					</div>
				{/if}
			</div>

			<!-- TODOs panel (only when the session has tasks) -->
			<TodoPanel
				{session}
				collapsed={tasksCollapsed}
				onToggle={() => (tasksCollapsed = !tasksCollapsed)}
				transitionIndex={1}

			/>

			<!-- Workers panel (only when relevant) -->
			{#if showWorkersPanel}
				<div class="workers-side-panel" class:collapsed={workersCollapsed} in:flyInX|global={{ index: 2 }}>
					<button
						type="button"
						class="panel-header"
						onclick={() => workersCollapsed = !workersCollapsed}
						aria-expanded={!workersCollapsed}
					>
						<span class="panel-chevron" class:rotated={workersCollapsed}>▾</span>
						<span class="panel-title">Workers</span>
						<span class="panel-count">{resolvedWorkers.length}</span>
					</button>
					{#if !workersCollapsed}
						{#if session.workerOf}
							<button class="back-to-pm" onclick={() => expandedSessionId.set(providerSessionKey('claudeCode', session.workerOf!))}>← Back to PM</button>
						{/if}
						<div class="workers-list">
							{#each resolvedWorkers as w (sessionKeyOf(w))}
								<button
									type="button"
									class="worker-row"
									class:active={sessionKeyOf(w) === sessionKeyOf(session)}
									onclick={() => openWorker(w)}
								>
								<span class="worker-name">{w.customTitle || w.codexTitle || w.cursorTitle || w.summary || w.sessionName}</span>
									<span class="worker-status" style="color: {getWorkerStatusColor(w.status)}">
										{getWorkerStatusLabel(w.status)}
									</span>
									<span class="worker-time">{formatTimeSince(w.modified)}</span>
								</button>
							{/each}
						</div>
					{/if}
				</div>
			{/if}

			<!-- Subagents panel (CC internal Agent tool calls; separate from c9watch workers) -->
			{#if hasSubagents}
				<div class="subagents-side-panel" class:collapsed={subagentsCollapsed}>
					<button
						type="button"
						class="panel-header"
						onclick={() => subagentsCollapsed = !subagentsCollapsed}
						aria-expanded={!subagentsCollapsed}
					>
						<span class="panel-chevron" class:rotated={subagentsCollapsed}>▾</span>
						<span class="panel-title">Subagents</span>
						<span class="panel-count">{mySubagents.length}</span>
					</button>
					{#if !subagentsCollapsed}
						{#if previewedSubagent}
							<button class="back-to-pm" onclick={closeSubagentPreview}>← Back to conversation</button>
						{/if}
						<div class="subagents-list">
							{#each mySubagents as sa (subagentKey(sa))}
								<button
									type="button"
									class="subagent-row"
									class:active={previewedSubagent ? subagentKey(previewedSubagent) === subagentKey(sa) : false}
									onclick={() => openSubagentPreview(sa)}
									title={sa.description || sa.agentType}
								>
									<span class="subagent-row-title">
										{sa.description || sa.agentType}
									</span>
									<span class="subagent-row-meta">
										<span class="subagent-type">{sa.agentType}</span>
										<span class="subagent-sep">·</span>
										<span class="subagent-status" style="color: {getSubagentStatusColor(sa.status)}">
											{getSubagentStatusLabel(sa.status)}
										</span>
										<span class="subagent-sep">·</span>
										<span class="subagent-time">{formatTimeSince(sa.startedAt)}</span>
									</span>
								</button>
							{/each}
						</div>
					{/if}
				</div>
			{/if}
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
		max-width: 1400px;
		height: 85vh;
		max-height: 900px;
		pointer-events: none; /* Allow clicks through empty layout area */
	}

	.overlay-card {
		position: relative;
		flex: 1; /* Take up remaining space */
		min-width: 0; /* Allow flex to shrink below content width */
		height: 100%;
		background: var(--bg-card);
		border: 1px solid var(--border-default);
		display: flex;
		flex-direction: column;
		overflow: hidden;
		pointer-events: auto; /* Enable clicks on the card */
		box-shadow: 0 20px 50px rgba(0, 0, 0, 0.5);
	}

	/* Right column: nav + workers stacked */
	.right-column.nav-desktop {
		flex-shrink: 0;
		width: 240px;
		height: 100%;
		display: flex;
		flex-direction: column;
		gap: var(--space-md);
		pointer-events: auto;
		overflow-y: auto;
	}

	/* Shared panel card style */
	.nav-map-side,
	.workers-side-panel {
		background: var(--bg-card);
		border: 1px solid var(--border-default);
		display: flex;
		flex-direction: column;
		flex-shrink: 0;
	}

	/* Nav panel takes remaining height when not collapsed */
	.nav-map-side {
		flex: 1;
		min-height: 0;
		overflow: hidden;
	}

	.nav-map-side.collapsed {
		flex: none;
	}

	.nav-map-side .panel-body {
		flex: 1;
		min-height: 0;
		overflow: hidden;
		display: flex;
		flex-direction: column;
	}

	/* Collapsible panel header (shared) */
	.panel-header {
		display: flex;
		align-items: center;
		gap: var(--space-sm);
		width: 100%;
		background: none;
		border: none;
		border-bottom: 1px solid var(--border-muted);
		padding: var(--space-sm) var(--space-lg);
		cursor: pointer;
		text-align: left;
		transition: background var(--transition-fast);
	}

	.panel-header:hover {
		background: color-mix(in srgb, var(--text-muted) 6%, transparent);
	}

	.panel-chevron {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--text-muted);
		transition: transform 0.2s ease;
		display: inline-block;
		flex-shrink: 0;
	}

	.panel-chevron.rotated {
		transform: rotate(-90deg);
	}

	.panel-title {
		font-family: var(--font-pixel);
		font-size: 12px;
		text-transform: uppercase;
		letter-spacing: 0.1em;
		color: var(--text-muted);
		flex: 1;
	}

	.panel-count {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--text-muted);
		opacity: 0.5;
	}

	/* Collapsed panel: no bottom border on header since there's nothing below */
	.nav-map-side.collapsed .panel-header,
	.workers-side-panel.collapsed .panel-header,
	.subagents-side-panel.collapsed .panel-header {
		border-bottom: none;
	}

	.workers-side-panel .workers-list {
		display: flex;
		flex-direction: column;
		gap: 2px;
		padding: var(--space-sm) 0;
	}

	.back-to-pm {
		display: block;
		width: 100%;
		background: none;
		border: none;
		border-bottom: 1px solid var(--border-muted);
		padding: var(--space-sm) var(--space-lg);
		text-align: left;
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--text-muted);
		cursor: pointer;
		transition: color var(--transition-fast);
	}

	.back-to-pm:hover {
		color: var(--text-primary, #fff);
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
		padding: var(--space-lg) var(--space-xl);
		border-bottom: 1px solid var(--border-default);
	}

	.header-left {
		display: flex;
		align-items: center;
		gap: var(--space-md);
	}

	.header-info {
		display: flex;
		flex-direction: column;
		gap: 4px;
	}

	.header-title {
		display: flex;
		align-items: center;
		gap: var(--space-sm);
	}

	.copy-id-btn {
		display: flex;
		align-items: center;
		justify-content: center;
		flex-shrink: 0;
		width: 20px;
		height: 20px;
		color: var(--text-muted);
		cursor: pointer;
		opacity: 0;
		transition: opacity var(--transition-fast), color var(--transition-fast);
	}

	.header-title:hover .copy-id-btn {
		opacity: 0.6;
	}

	.copy-id-btn:hover {
		opacity: 1 !important;
		color: var(--text-primary);
	}

	.copy-id-btn.copied {
		opacity: 1 !important;
		color: var(--status-input);
	}

	/* Cursor-following tooltip */
	.id-tooltip {
		position: fixed;
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--text-primary);
		background: var(--bg-elevated);
		border: 1px solid var(--border-default);
		padding: 4px 8px;
		white-space: nowrap;
		pointer-events: none;
		z-index: 9999;
		letter-spacing: 0.02em;
		text-transform: none;
		font-weight: 400;
		box-shadow: 0 4px 12px rgba(0, 0, 0, 0.5);
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
		max-width: 500px;
	}

	.session-name-badge {
		font-family: var(--font-mono);
		font-size: 11px;
		font-weight: 500;
		color: var(--text-muted);
		background: var(--bg-elevated);
		padding: 2px 6px;
		border: 1px solid var(--border-default);
		text-transform: uppercase;
		letter-spacing: 0.1em;
	}

	.cost-breakdown {
		display: inline-flex;
		align-items: center;
		gap: 4px;
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--text-muted);
		letter-spacing: 0.05em;
	}

	.worker-badge {
		font-family: var(--font-pixel);
		font-size: 10px;
		font-weight: 500;
		color: var(--accent-pink);
		background: color-mix(in srgb, var(--accent-pink) 12%, transparent);
		padding: 2px 6px;
		border: 1px solid color-mix(in srgb, var(--accent-pink) 40%, transparent);
		text-transform: uppercase;
		letter-spacing: 0.1em;
	}

	.pm-badge {
		font-family: var(--font-pixel);
		font-size: 10px;
		font-weight: 500;
		color: var(--accent-pink);
		background: color-mix(in srgb, var(--accent-pink) 12%, transparent);
		padding: 2px 6px;
		border: 1px solid color-mix(in srgb, var(--accent-pink) 40%, transparent);
		text-transform: uppercase;
		letter-spacing: 0.1em;
		white-space: nowrap;
	}

	.worker-row {
		display: grid;
		grid-template-columns: 1fr auto auto;
		align-items: center;
		gap: var(--space-sm);
		padding: 6px var(--space-md);
		background: transparent;
		border: none;
		border-left: 2px solid transparent;
		color: var(--text-primary);
		text-align: left;
		font-family: var(--font-mono);
		font-size: 12px;
		cursor: pointer;
		transition: all 0.15s ease;
		width: 100%;
	}

	.worker-row:hover {
		background: color-mix(in srgb, var(--text-muted) 6%, transparent);
		border-left-color: var(--border-muted);
	}

	.worker-row.active {
		border-left-color: var(--accent-green);
		background: color-mix(in srgb, var(--accent-green) 10%, transparent);
	}

	.worker-name {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		min-width: 0;
	}

	.worker-status {
		text-transform: uppercase;
		letter-spacing: 0.1em;
		font-weight: 500;
	}

	.worker-time {
		color: var(--text-muted);
		text-transform: uppercase;
		letter-spacing: 0.05em;
	}

	.subagents-side-panel {
		background: var(--bg-card);
		border: 1px solid var(--border-default);
		display: flex;
		flex-direction: column;
		flex-shrink: 0;
	}

	.subagents-side-panel .subagents-list {
		display: flex;
		flex-direction: column;
		gap: 2px;
		padding: var(--space-sm) 0;
	}

	.subagent-row {
		display: flex;
		flex-direction: column;
		gap: 2px;
		padding: 6px var(--space-md);
		background: transparent;
		border: none;
		border-left: 2px solid transparent;
		color: var(--text-primary);
		text-align: left;
		font-family: var(--font-mono);
		font-size: 12px;
		cursor: pointer;
		transition: all 0.15s ease;
		width: 100%;
	}

	.subagent-row:hover {
		background: color-mix(in srgb, var(--text-muted) 6%, transparent);
		border-left-color: var(--border-muted);
	}

	.subagent-row.active {
		border-left-color: var(--accent-purple, #b98cff);
		background: color-mix(in srgb, var(--accent-purple, #b98cff) 10%, transparent);
	}

	.subagent-row-title {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		color: var(--text-primary);
	}

	.subagent-row-meta {
		display: flex;
		align-items: center;
		gap: 6px;
		font-size: 10px;
		color: var(--text-muted);
	}

	.subagent-type {
		color: var(--accent-purple, #b98cff);
		text-transform: uppercase;
		letter-spacing: 0.1em;
	}

	.subagent-status {
		text-transform: uppercase;
		letter-spacing: 0.1em;
		font-weight: 500;
	}

	.subagent-time {
		color: var(--text-muted);
		text-transform: uppercase;
		letter-spacing: 0.05em;
	}

	.subagent-sep {
		color: var(--text-muted);
		opacity: 0.5;
	}

	.subagent-preview {
		display: flex;
		flex-direction: column;
		gap: var(--space-lg);
		padding: var(--space-lg);
	}

	.subagent-preview-header {
		border-left: 2px solid var(--accent-purple, #b98cff);
		padding-left: var(--space-md);
		display: flex;
		flex-direction: column;
		gap: 4px;
	}

	.subagent-preview-title {
		font-family: var(--font-pixel);
		font-size: 18px;
		text-transform: uppercase;
		letter-spacing: 0.08em;
		color: var(--text-primary);
	}

	.subagent-preview-meta {
		display: flex;
		align-items: center;
		gap: 6px;
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--text-muted);
	}

	.subagent-messages {
		display: flex;
		flex-direction: column;
		gap: var(--space-md);
	}

	.subagent-msg {
		background: var(--bg-card);
		border: 1px solid var(--border-default);
		padding: var(--space-md);
		display: flex;
		flex-direction: column;
		gap: 8px;
	}

	.subagent-msg-assistant {
		border-left: 2px solid var(--accent-purple, #b98cff);
	}

	.subagent-msg-user {
		border-left: 2px solid var(--text-muted);
	}

	.subagent-msg-role {
		font-family: var(--font-mono);
		font-size: 10px;
		text-transform: uppercase;
		letter-spacing: 0.15em;
		color: var(--text-muted);
	}

	.subagent-msg-body {
		margin: 0;
		font-family: var(--font-mono);
		font-size: 12px;
		line-height: 1.55;
		color: var(--text-primary);
		white-space: pre-wrap;
		word-break: break-word;
	}

	.subagent-msg-pending {
		margin: 0;
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--text-muted);
		font-style: italic;
	}

	.subagent-stats {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 6px;
		padding: var(--space-sm) var(--space-md);
		background: var(--bg-elevated);
		border: 1px solid var(--border-default);
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--text-muted);
		text-transform: uppercase;
		letter-spacing: 0.05em;
	}

	.subagent-agent-id {
		color: var(--accent-purple, #b98cff);
	}

	.git-info {
		display: flex;
		align-items: center;
		gap: 6px;
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--text-muted);
		min-width: 0;
	}

	.branch-name {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		max-width: 200px;
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

	.status-label {
		font-weight: 500;
	}

	.separator {
		color: var(--text-muted);
	}

	.message-count {
		color: var(--text-muted);
	}

	.header-actions {
		display: flex;
		align-items: center;
		gap: var(--space-xs);
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

	.empty-hint {
		font-family: var(--font-mono);
		font-size: 13px;
		color: var(--text-muted);
		text-transform: uppercase;
		letter-spacing: 0.05em;
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

		/* Hide the desktop right column on mobile */
		.right-column.nav-desktop {
			display: none;
		}

		.overlay-card {
			border: none;
			box-shadow: none;
		}

		.overlay-header {
			padding: var(--space-md) var(--space-md);
			gap: var(--space-sm);
		}

		.header-left {
			min-width: 0;
			flex: 1;
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
