<script lang="ts">
	import { onMount, untrack } from 'svelte';
	import { sortedSessions, statusSummary, sessions as sessionsStore, initializeSessionListeners } from '$lib/stores/sessions';
	import { openSession, getSessions } from '$lib/api';
	import { SessionStatus } from '$lib/types';
	import type { Session } from '$lib/types';
	import { invoke } from '@tauri-apps/api/core';
	import { listen } from '@tauri-apps/api/event';
	import { isDemoMode, loadDemoDataIfActive } from '$lib/demo';

	let sessions = $derived($sortedSessions);
	let summary = $derived($statusSummary);
	let totalSessions = $derived(sessions.length);

	// Pixel grid state
	let trackWidth = $state(0);
	let isSweeping = $state(false);
	let prevSummaryKey = $state('');
	let summaryKey = $derived(`${summary.working}-${summary.permission}-${summary.input}`);

	$effect(() => {
		const currentKey = summaryKey;
		const previousKey = untrack(() => prevSummaryKey);
		if (previousKey !== '' && currentKey !== previousKey) {
			isSweeping = false;
			// Force a microtask gap so Svelte re-applies the class before setting it back,
			// allowing the CSS animation to replay even for rapid back-to-back changes.
			setTimeout(() => {
				isSweeping = true;
				// Last block starts at (columns-1)*20ms, animation runs 2s → ~2500ms max.
				setTimeout(() => { isSweeping = false; }, 2500);
			}, 0);
		}
		prevSummaryKey = currentKey;
	});

	let columns = $derived(Math.max(1, Math.floor((trackWidth - 6) / 10)));
	let statusArray = $derived.by(() => {
		const total = totalSessions;
		if (total === 0) return Array(columns).fill('empty');
		const counts = [summary.working, summary.permission, summary.input];
		const percentages = counts.map((c) => (c / total) * columns);
		const integerParts = percentages.map((p) => Math.floor(p));
		const remainders = percentages.map((p, i) => p - integerParts[i]);
		let allocated = integerParts.reduce((a, b) => a + b, 0);
		const result = [...integerParts];
		while (allocated < columns) {
			let maxIdx = remainders.indexOf(Math.max(...remainders));
			if (maxIdx === -1) break;
			result[maxIdx]++;
			remainders[maxIdx] = -1;
			allocated++;
		}
		const arr: string[] = [];
		for (let i = 0; i < result[0]; i++) arr.push('working');
		for (let i = 0; i < result[1]; i++) arr.push('permission');
		for (let i = 0; i < result[2]; i++) arr.push('input');
		while (arr.length < columns) arr.push('empty');
		return arr;
	});

	onMount(() => {
		let unlistenFocus: (() => void) | null = null;
		let cancelled = false;

		const init = async () => {
			const demoActive = loadDemoDataIfActive();
			await initializeSessionListeners();

			if (!demoActive) {
				try {
					const initialSessions = await getSessions();
					sessionsStore.set(initialSessions);
				} catch (error) {
					console.error('Failed to fetch sessions:', error);
				}
			}

			const ul1 = await listen('tauri://focus', () => {
				// Re-sync demo mode state on every focus — user may have toggled it
				// in the main window since the last time this panel was shown.
				const demo = loadDemoDataIfActive();
				if (demo) return;

				// Demo is off — ensure isDemoMode store is cleared.
				// Don't fetch sessions independently; the poller is the single source
				// of truth and will emit sessions-updated within 3.5s.
				isDemoMode.set(false);
			});

			if (cancelled) {
				ul1();
			} else {
				unlistenFocus = ul1;
			}
		};

		init();

		return () => {
			cancelled = true;
			if (unlistenFocus) unlistenFocus();
		};
	});

	function getStatusColor(status: SessionStatus): string {
		switch (status) {
			case SessionStatus.NeedsAttention:
				return 'var(--status-permission)';
			case SessionStatus.WaitingForInput:
				return 'var(--status-input)';
			case SessionStatus.Working:
				return 'var(--status-working)';
			default:
				return 'var(--status-connecting)';
		}
	}

	async function handleOpen(session: Session) {
		try {
			await openSession(session.pid, session.projectPath);
		} catch (error) {
			console.error('Failed to open:', error);
		}
	}

	async function openMainWindow() {
		try {
			await invoke('show_main_window');
		} catch (error) {
			console.error('Failed to open main window:', error);
		}
	}
</script>

<div class="popover">
	<header class="popover-header">
		<span class="total-count">{totalSessions} session{totalSessions !== 1 ? 's' : ''}</span>
		<button class="dashboard-btn" onclick={openMainWindow}>
			Open Dashboard
			<svg aria-hidden="true" width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
				<path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6" />
				<polyline points="15 3 21 3 21 9" />
				<line x1="10" y1="14" x2="21" y2="3" />
			</svg>
		</button>
	</header>

	<div class="pixel-grid" class:empty={totalSessions === 0} bind:clientWidth={trackWidth}>
		<div class="grid-inner" style="grid-template-columns: repeat({columns}, 1fr);">
			{#each statusArray as status, i}
				<div class="block {status}" class:sweeping={isSweeping} style="animation-delay: {i * 20}ms; transition-delay: {i * 50}ms"></div>
			{/each}
		</div>
	</div>

	<main class="popover-content">
		{#if sessions.length === 0}
			<div class="empty-state">
				<p>No active sessions</p>
			</div>
		{:else}
			<div class="session-list">
				{#each sessions as session (session.id)}
					<button class="session-card" onclick={() => handleOpen(session)}>
						<div class="card-top">
							<span class="session-dot" style="background: {getStatusColor(session.status)}"></span>
							<span class="session-project">{session.sessionName}</span>
							<svg aria-hidden="true" class="open-icon" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
								<path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6" />
								<polyline points="15 3 21 3 21 9" />
								<line x1="10" y1="14" x2="21" y2="3" />
							</svg>
						</div>
						<div class="card-title">{session.customTitle || session.firstPrompt}</div>
						{#if session.latestMessage}
							<div class="card-latest">{session.latestMessage}</div>
						{/if}
					</button>
				{/each}
			</div>
		{/if}
	</main>
</div>

<style>
	.popover {
		display: flex;
		flex-direction: column;
		height: 100vh;
		background: var(--bg-base);
		border: 1px solid var(--border-default);
		border-radius: 10px;
		overflow: hidden;
		font-family: var(--font-mono);
		user-select: none;
		-webkit-user-select: none;
	}

	.popover-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding: 10px 16px;
		flex-shrink: 0;
	}

	.total-count {
		font-size: 11px;
		color: var(--text-muted);
	}

	.dashboard-btn {
		display: flex;
		align-items: center;
		gap: 5px;
		padding: 4px 0;
		border: none;
		background: transparent;
		color: var(--text-muted);
		font-family: var(--font-mono);
		font-size: 10px;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		cursor: pointer;
		transition: color var(--transition-fast);
	}

	.dashboard-btn:hover {
		color: var(--text-primary);
	}

	.dashboard-btn:focus-visible {
		outline: none;
	}

	.pixel-grid {
		height: 16px;
		background: var(--bg-elevated);
		border-top: 1px solid var(--border-muted);
		border-bottom: 1px solid var(--border-muted);
		padding: 3px 16px;
		flex-shrink: 0;
	}

	.pixel-grid.empty {
		opacity: 0.4;
	}

	.grid-inner {
		display: grid;
		grid-template-rows: 1fr;
		gap: 2px;
		height: 100%;
	}

	.block {
		background: rgba(255, 255, 255, 0.06);
		border-radius: 1px;
		transition: background-color 0.4s, box-shadow 0.4s;
	}

	.block.sweeping {
		animation: monitor-sweep 2s ease-out forwards;
	}

	@keyframes monitor-sweep {
		0%   { transform: scale(1);    filter: brightness(1); }
		20%  { transform: scale(0.95); filter: brightness(1.1); }
		40%  { transform: scale(1.1);  filter: brightness(1.4) drop-shadow(0 0 2px currentColor); }
		100% { transform: scale(1);    filter: brightness(1); }
	}

	.block.working    { background-color: var(--status-working);    color: var(--status-working);    box-shadow: 0 0 3px var(--status-working-glow); }
	.block.permission { background-color: var(--status-permission); color: var(--status-permission); box-shadow: 0 0 3px var(--status-permission-glow); }
	.block.input      { background-color: var(--status-input);      color: var(--status-input);      box-shadow: 0 0 3px var(--status-input-glow); }

	.popover-content {
		flex: 1;
		overflow-y: auto;
	}

	.empty-state {
		display: flex;
		align-items: center;
		justify-content: center;
		height: 100%;
		color: var(--text-muted);
		font-size: 11px;
		text-transform: uppercase;
		letter-spacing: 0.05em;
	}

	.empty-state p {
		margin: 0;
	}

	.session-list {
		display: flex;
		flex-direction: column;
	}

	.session-card {
		display: flex;
		flex-direction: column;
		gap: 3px;
		padding: 9px 16px;
		border: none;
		border-bottom: 1px solid var(--border-muted);
		background: transparent;
		color: var(--text-primary);
		font-family: var(--font-mono);
		cursor: pointer;
		transition: background var(--transition-fast);
		text-align: left;
		width: 100%;
	}

	.session-card:last-child {
		border-bottom: none;
	}

	.session-card:hover {
		background: var(--bg-elevated);
	}

	.session-card:focus-visible {
		outline: none;
	}

	.card-top {
		display: flex;
		align-items: center;
		gap: 8px;
	}

	.session-dot {
		width: 6px;
		height: 6px;
		border-radius: 0;
		flex-shrink: 0;
	}

	.session-project {
		flex: 1;
		font-size: 12px;
		font-weight: 600;
		color: var(--text-primary);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.open-icon {
		flex-shrink: 0;
		opacity: 0;
		color: var(--text-muted);
		transition: opacity var(--transition-fast);
	}

	.session-card:hover .open-icon {
		opacity: 1;
	}

	.card-title {
		font-size: 11px;
		color: var(--text-secondary);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		padding-left: 14px;
	}

	.card-latest {
		font-size: 10px;
		color: var(--text-muted);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		padding-left: 14px;
	}

</style>
