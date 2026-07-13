<script lang="ts">
	import { fade } from 'svelte/transition';
	import { flip } from 'svelte/animate';
	import { fadeIn, flyIn } from '$lib/transitions';
	import { onMount } from 'svelte';
	import { browser } from '$app/environment';
	import {
		sortedSessions,
		expandedSessionId,
		currentConversation,
		statusSummary
	} from '$lib/stores/sessions';
	import { getConversation, stopSession, openSession } from '$lib/api';
	import { isDemoMode, toggleDemoMode } from '$lib/demo';
	import { PM_ORCHESTRATION_ENABLED } from '$lib/feature-flags';
	import { isTauri } from '$lib/ws';
	import StatusBar from '$lib/components/StatusBar.svelte';
	import SessionCard from '$lib/components/SessionCard.svelte';
	import ExpandedCardOverlay from '$lib/components/ExpandedCardOverlay.svelte';
	import ToastNotifications from '$lib/components/ToastNotifications.svelte';
	import QRCodeModal from '$lib/components/QRCodeModal.svelte';
	import ConnectionScreen from '$lib/components/ConnectionScreen.svelte';
	import type { Session } from '$lib/types';
	import { SessionStatus } from '$lib/types';
	import SessionHistory from '$lib/components/SessionHistory.svelte';
	import CostTracker from '$lib/components/CostTracker.svelte';
	import { refreshCostData } from '$lib/stores/cost';
	import { refreshSessionHistory } from '$lib/stores/history';
	import MemoryViewer from '$lib/components/MemoryViewer.svelte';
	import SettingsTab from '$lib/components/SettingsTab.svelte';
	import FdaBanner from '$lib/components/FdaBanner.svelte';
	import UpdateBanner from '$lib/components/UpdateBanner.svelte';
	import DebugConsole from '$lib/components/DebugConsole.svelte';
	import type { DetectionDiagnostics } from '$lib/types';
	import ProviderFilter from '$lib/components/ProviderFilter.svelte';
	import { providerFilter } from '$lib/stores/provider-filter';
	import { isTopLevelSession, matchesProvider, providerFilterLabel } from '$lib/provider';

	let demoActive = $derived($isDemoMode);
	let showQRModal = $state(false);

	let needsConnection = $state(!isTauri());

	let sessions = $derived($sortedSessions);
	let activeSessionIds = $derived(new Set(sessions.map((s) => s.id)));
	let summary = $derived($statusSummary);
	let expandedId = $derived($expandedSessionId);
	let conversation = $derived($currentConversation);

	let viewMode = $state<'project' | 'all'>('project');
	let sessionFilter = $state<'humans' | 'workers'>('humans');

	let isCompact = $state(false);

	let activeTab = $state<'monitor' | 'history' | 'cost' | 'memory' | 'settings'>('monitor');
	let fdaLikelyNeeded = $state(false);
	let showDebugConsole = $state(false);
	let showRenameHint = $state(false);

	// Detect macOS native fullscreen to switch tab-bar padding.
	// CSS `display-mode: fullscreen` does NOT fire for native macOS fullscreen.
	// We use Tauri's window resize event + a short delay so isFullscreen()
	// is queried after the transition has settled (avoids stale values).
	let isFullscreen = $state(false);

	onMount(() => {
		if (browser) {
			const saved = localStorage.getItem('sessionViewMode');
			if (saved === 'project' || saved === 'all') {
				viewMode = saved;
			}
			const savedCompact = localStorage.getItem('sessionViewCompact');
			if (savedCompact === 'true') {
				isCompact = true;
			}
			const savedFilter = localStorage.getItem('sessionFilter');
			if (
				PM_ORCHESTRATION_ENABLED &&
				(savedFilter === 'humans' || savedFilter === 'workers')
			) {
				sessionFilter = savedFilter;
			}
		}

		refreshCostData();
		refreshSessionHistory();

		if (!isTauri()) return;

		let unlisten: (() => void) | null = null;
		let timer: ReturnType<typeof setTimeout> | null = null;

		(async () => {
			const { getCurrentWindow } = await import('@tauri-apps/api/window');
			const win = getCurrentWindow();

			// Check once on mount
			isFullscreen = await win.isFullscreen();

			// Re-check after every resize with a delay so the transition settles
			unlisten = await win.onResized(async () => {
				if (timer) clearTimeout(timer);
				timer = setTimeout(async () => {
					isFullscreen = await win.isFullscreen();
				}, 150);
			});
		})();

		// Listen for detection diagnostics (FDA banner)
		if (isTauri()) {
			import('@tauri-apps/api/event').then(({ listen }) => {
				listen<DetectionDiagnostics>('diagnostic-update', (event) => {
					fdaLikelyNeeded = event.payload.fdaLikelyNeeded;
				});
			});
		}

		return () => {
			unlisten?.();
			if (timer) clearTimeout(timer);
		};
	});

	$effect(() => {
		if (browser) {
			localStorage.setItem('sessionViewCompact', String(isCompact));
		}
	});

	$effect(() => {
		if (browser) {
			localStorage.setItem('sessionFilter', sessionFilter);
		}
	});

	$effect(() => {
		if (browser) {
			localStorage.setItem('sessionViewMode', viewMode);
		}
	});

	$effect(() => {
		if (!browser) return;

		const tabByKey: Record<string, typeof activeTab> = {
			'1': 'monitor',
			'2': 'history',
			'3': 'cost',
			'4': 'memory',
			'5': 'settings'
		};

		const handler = (e: KeyboardEvent) => {
			if (!(e.metaKey || e.ctrlKey)) return;
			if (e.altKey || e.shiftKey) return;
			const tab = tabByKey[e.key];
			if (!tab) return;

			const target = e.target as HTMLElement | null;
			if (target) {
				const tag = target.tagName;
				if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT') return;
				if (target.isContentEditable) return;
			}

			e.preventDefault();
			activeTab = tab;
		};

		window.addEventListener('keydown', handler);
		return () => window.removeEventListener('keydown', handler);
	});

	// Helper function to group sessions by project path, then by status
	function groupByProjectAndStatus(sessions: Session[]) {
		const groups: Array<{
			path: string;
			displayName: string;
			attention: Session[];
			idle: Session[];
			working: Session[];
			lastModified: number;
		}> = [];

		sessions.forEach(session => {
			let group = groups.find(g => g.path === session.projectPath);
			if (!group) {
				const parts = session.projectPath.split(/[/\\]/);
				const folderName = parts.filter(Boolean).pop() || session.projectPath;
				group = {
					path: session.projectPath,
					displayName: folderName,
					attention: [],
					idle: [],
					working: [],
					lastModified: 0
				};
				groups.push(group);
			}

			addToGroup(group, session);
		});

		return sortGroups(groups);
	}

	function groupSessionsByStatus(sessions: Session[]) {
		const groups = {
			attention: [] as Session[],
			idle: [] as Session[],
			working: [] as Session[]
		};

		sessions.forEach(session => {
			if (session.status === SessionStatus.NeedsAttention) {
				groups.attention.push(session);
			} else if (session.status === SessionStatus.WaitingForInput) {
				groups.idle.push(session);
			} else if (session.status === SessionStatus.Working || session.status === SessionStatus.Connecting) {
				groups.working.push(session);
			}
		});

		const sortSessions = (a: Session, b: Session) => new Date(a.modified).getTime() - new Date(b.modified).getTime();

		return [
			{ id: 'attention', label: 'Needs Attention', sessions: groups.attention.sort(sortSessions), type: 'attention' },
			{ id: 'idle', label: 'Idle', sessions: groups.idle.sort(sortSessions), type: 'idle' },
			{ id: 'working', label: 'Working', sessions: groups.working.sort(sortSessions), type: 'working' }
		].filter(g => g.sessions.length > 0);
	}

	function addToGroup(group: any, session: Session) {
		const modified = new Date(session.modified).getTime();
		if (modified > group.lastModified) {
			group.lastModified = modified;
		}

		if (session.status === SessionStatus.NeedsAttention) {
			group.attention.push(session);
		} else if (session.status === SessionStatus.WaitingForInput) {
			group.idle.push(session);
		} else if (session.status === SessionStatus.Working || session.status === SessionStatus.Connecting) {
			group.working.push(session);
		}
	}

	function sortGroups(groups: any[]) {
		// Sort groups: priority to those needing attention, then by modification time
		return groups.sort((a, b) => {
			const aNeedsAttention = a.attention.length > 0;
			const bNeedsAttention = b.attention.length > 0;
			if (aNeedsAttention !== bNeedsAttention) return aNeedsAttention ? -1 : 1;

			const aNeedsIdle = a.idle.length > 0;
			const bNeedsIdle = b.idle.length > 0;
			if (aNeedsIdle !== bNeedsIdle) return aNeedsIdle ? -1 : 1;

			return b.lastModified - a.lastModified;
		});
	}

	// With PM orchestration disabled, show every session — legacy `workerOf`
	// records render as ordinary sessions and the HUMANS/WORKERS split is gone.
	let filteredSessions = $derived.by(() => {
		const providerSessions = sessions
			.filter(isTopLevelSession)
			.filter((session) => matchesProvider(session, $providerFilter));
		return !PM_ORCHESTRATION_ENABLED
			? providerSessions
			: providerSessions.filter((session) =>
					sessionFilter === 'workers' ? !!session.workerOf : !session.workerOf
				);
	});

	let projectGroups = $derived(groupByProjectAndStatus(filteredSessions));
	let allStatusGroups = $derived(groupSessionsByStatus(filteredSessions));

	// Running-offset arrays so entry animations share a single index namespace
	// per tab (like CostTracker). Each group's starting index is the cumulative
	// count of everything that came before it. Inside a group the header takes
	// `offset + 0` and inner items continue from `offset + 1`.
	let projectOffsets = $derived.by(() => {
		const offsets: number[] = [];
		let acc = 0;
		for (const g of projectGroups) {
			offsets.push(acc);
			acc += 1 + g.attention.length + g.idle.length + g.working.length;
		}
		return offsets;
	});
	let allGroupOffsets = $derived.by(() => {
		const offsets: number[] = [];
		let acc = 0;
		for (const g of allStatusGroups) {
			offsets.push(acc);
			acc += 1 + g.sessions.length;
		}
		return offsets;
	});

	let filteredSummary = $derived({
		working: filteredSessions.filter(
			(s) =>
				s.status === SessionStatus.Working ||
				s.status === SessionStatus.Connecting
		).length,
		permission: filteredSessions.filter(
			(s) => s.status === SessionStatus.NeedsAttention
		).length,
		input: filteredSessions.filter(
			(s) => s.status === SessionStatus.WaitingForInput
		).length
	});

	let expandedSession = $derived(sessions.find((s) => s.id === expandedId) || null);

	$effect(() => {
		if (expandedSession && !matchesProvider(expandedSession, $providerFilter)) {
			expandedSessionId.set(null);
		}
	});

	$effect(() => {
		if (expandedId) {
			getConversation(expandedId)
				.then((conv) => {
					currentConversation.set(conv);
				})
				.catch((error) => {
					console.error('Failed to fetch conversation:', error);
					currentConversation.set(null);
				});
		} else {
			currentConversation.set(null);
		}
	});

	function handleExpand(session: Session) {
		expandedSessionId.set(session.id);
	}

	function handleClose() {
		expandedSessionId.set(null);
	}

	async function handleStop(pid: number) {
		try {
			await stopSession(pid);
		} catch (error) {
			console.error('Failed to stop session:', error);
		}
	}

	async function handleOpen(pid: number, projectPath: string) {
		try {
			await openSession(pid, projectPath);
		} catch (error) {
			console.error('Failed to open session:', error);
		}
	}

	function handleKeydown(e: KeyboardEvent) {
		const tag = (e.target as HTMLElement)?.tagName;
		if (tag === 'INPUT' || tag === 'TEXTAREA') return;

		// Cmd+Shift+D → debug console (check shift first to avoid triggering demo toggle)
		// On macOS, e.key is lowercase 'd' even with Shift held when Cmd is pressed
		if ((e.key === 'd' || e.key === 'D') && e.shiftKey && (e.metaKey || e.ctrlKey)) {
			e.preventDefault();
			showDebugConsole = !showDebugConsole;
			return;
		}

		if (e.key === 'd' && (e.metaKey || e.ctrlKey)) {
			e.preventDefault();
			toggleDemoMode();
			return;
		}
		if (e.key >= '1' && e.key <= '9' && !expandedId && !e.metaKey && !e.ctrlKey) {
			const index = parseInt(e.key) - 1;
			if (index < filteredSessions.length) {
				handleExpand(filteredSessions[index]);
			}
		}
		if (e.key === 'Tab' && !expandedId) {
			// Find first session needing attention across all projects
			const needsAction = filteredSessions.filter(s =>
				s.status === SessionStatus.NeedsAttention ||
				s.status === SessionStatus.WaitingForInput
			);
			if (needsAction.length > 0) {
				e.preventDefault();
				handleExpand(needsAction[0]);
			}
		}
	}


</script>

<svelte:window on:keydown={handleKeydown} />

{#if needsConnection}
	<ConnectionScreen onconnected={() => (needsConnection = false)} />
{:else}
<div class="dashboard">
	<FdaBanner {fdaLikelyNeeded} />
	<div class="tab-bar" class:fullscreen={isFullscreen} data-tauri-drag-region>
		<button
			class="tab-btn"
			class:active={activeTab === 'monitor'}
			onclick={() => (activeTab = 'monitor')}
		>
			<span class="tab-icon">■</span>
			<span class="tab-label">MONITOR</span>
		</button>
		<button
			class="tab-btn"
			class:active={activeTab === 'history'}
			onclick={() => (activeTab = 'history')}
		>
			<span class="tab-icon">⌕</span>
			<span class="tab-label">HISTORY</span>
		</button>
		<button
			class="tab-btn"
			class:active={activeTab === 'cost'}
			onclick={() => (activeTab = 'cost')}
		>
			<span class="tab-icon">$</span>
			<span class="tab-label">COST</span>
		</button>
		<button
			class="tab-btn"
			class:active={activeTab === 'memory'}
			onclick={() => (activeTab = 'memory')}
		>
			<span class="tab-icon">◆</span>
			<span class="tab-label">MEMORY</span>
		</button>
		<!-- Drag handle: fills remaining space. The grip dots are absolutely
		     centered in the whole tab bar so they appear at the window midpoint.
		     Hidden in fullscreen where window dragging is unavailable. -->
		<div class="tab-drag-region" data-tauri-drag-region>
			{#if !isFullscreen}
				<span class="drag-dots" transition:fade={{ duration: 250 }}>⠿ ⠿ ⠿</span>
			{/if}
		</div>
		<div class="global-provider-filter"><ProviderFilter compact /></div>
		<button
			class="tab-btn"
			class:active={activeTab === 'settings'}
			onclick={() => (activeTab = 'settings')}
		>
			<span class="tab-icon">⚙</span>
			<span class="tab-label">SETTINGS</span>
		</button>
	</div>

	<UpdateBanner onViewDetails={() => (activeTab = 'settings')} />

	{#if activeTab === 'history'}
	<main class="grid-container history-main" in:fadeIn>
		<SessionHistory {activeSessionIds} />
	</main>
	{:else if activeTab === 'cost'}
	<main class="grid-container history-main" in:fadeIn>
		<CostTracker />
	</main>
	{:else if activeTab === 'memory'}
	<main class="grid-container history-main" in:fadeIn>
		<MemoryViewer />
	</main>
	{:else if activeTab === 'settings'}
	<main class="grid-container history-main" in:fadeIn>
		<SettingsTab />
	</main>
	{:else}
	<main class="grid-container" in:fadeIn>
		<div class="sections-container">
			<section class="system-section" in:flyIn|global={{ index: 0 }}>
				<div class="project-header">
					<span class="project-name">System status</span>
					<span class="project-count">{filteredSessions.length}</span>
					<button
						class="toggle-btn demo-toggle"
						class:active={demoActive}
						onclick={() => toggleDemoMode()}
						title="Try with Sample Data (Cmd+D)"
					>
						<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
							<path d="M10 2v7.31" />
							<path d="M14 2v7.31" />
							<path d="M8.5 2h7" />
							<path d="M14 9.3c.7.4 1.3.9 1.8 1.5l3.8 4.4a3 3 0 0 1-2.3 4.8H6.7a3 3 0 0 1-2.3-4.8l3.8-4.4c.5-.6 1.1-1.1 1.8-1.5" />
						</svg>
						<span class="demo-label">DEMO</span>
					</button>
					{#if isTauri()}
						<button
							class="toggle-btn mobile-connect-btn"
							onclick={() => (showQRModal = true)}
							title="Connect Mobile Device"
						>
							<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
								<rect x="5" y="2" width="14" height="20" rx="2" ry="2" />
								<line x1="12" y1="18" x2="12.01" y2="18" />
							</svg>
							<span class="mobile-label">MOBILE</span>
						</button>
					{/if}
					<div class="header-spacer"></div>
					{#if PM_ORCHESTRATION_ENABLED}
					<div class="view-toggle">
						<button
							type="button"
							class="toggle-btn"
							class:active={sessionFilter === 'humans'}
							onclick={() => sessionFilter = 'humans'}
							title="Show human sessions"
						>
							<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
								<path d="M20 21v-2a4 4 0 0 0-4-4H8a4 4 0 0 0-4 4v2" />
								<circle cx="12" cy="7" r="4" />
							</svg>
						</button>
						<button
							type="button"
							class="toggle-btn"
							class:active={sessionFilter === 'workers'}
							onclick={() => sessionFilter = 'workers'}
							title="Show PM workers"
						>
							<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
								<rect x="4" y="6" width="16" height="12" rx="2" />
								<circle cx="9" cy="12" r="1.5" />
								<circle cx="15" cy="12" r="1.5" />
								<path d="M12 2v4" />
							</svg>
						</button>
					</div>
					{/if}
					<div class="view-toggle">
						<button
							class="toggle-btn"
							class:active={isCompact}
							onclick={() => isCompact = !isCompact}
							title="Compact View"
						>
							<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
								<polyline points="4 14 10 14 10 20" />
								<polyline points="20 10 14 10 14 4" />
								<line x1="14" y1="10" x2="21" y2="3" />
								<line x1="3" y1="21" x2="10" y2="14" />
							</svg>
						</button>
					</div>
					<div class="view-toggle">
						<button 
							class="toggle-btn" 
							class:active={viewMode === 'project'} 
							onclick={() => viewMode = 'project'}
							title="Group by Project"
						>
							<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3" stroke-linecap="round" stroke-linejoin="round">
								<line x1="8" y1="6" x2="21" y2="6"></line>
								<line x1="8" y1="12" x2="21" y2="12"></line>
								<line x1="8" y1="18" x2="21" y2="18"></line>
								<line x1="3" y1="6" x2="3.01" y2="6"></line>
								<line x1="3" y1="12" x2="3.01" y2="12"></line>
								<line x1="3" y1="18" x2="3.01" y2="18"></line>
							</svg>
						</button>
						<button 
							class="toggle-btn" 
							class:active={viewMode === 'all'} 
							onclick={() => viewMode = 'all'}
							title="Show All"
						>
							<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
								<rect x="3" y="3" width="7" height="7" rx="1" />
								<rect x="14" y="3" width="7" height="7" rx="1" />
								<rect x="14" y="14" width="7" height="7" rx="1" />
								<rect x="3" y="14" width="7" height="7" rx="1" />
							</svg>
						</button>
					</div>
				</div>
				
				{#if filteredSessions.length > 0}
					<div class="system-status-container">
						<StatusBar total={filteredSessions.length} summary={filteredSummary} />
					</div>
				{/if}
			</section>

			{#if filteredSessions.length === 0}
				<div class="empty-state">
					<div class="empty-visual">
						<div class="empty-orb">
							<div class="orb-core"></div>
							<div class="orb-ring ring-1"></div>
							<div class="orb-ring ring-2"></div>
							<div class="orb-ring ring-3"></div>
						</div>
					</div>
					<div class="empty-content">
						{#if PM_ORCHESTRATION_ENABLED && sessionFilter === 'workers'}
							<h2>No Workers Spawned</h2>
							<p>Spawn a worker from a Claude Code session with <code>c9watch spawn</code></p>
							<div class="empty-hint">
								<span class="hint-icon">
									<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
										<circle cx="12" cy="12" r="10" />
										<path d="M12 16v-4" />
										<path d="M12 8h.01" />
									</svg>
								</span>
								Workers appear here once they're spawned by a PM session
							</div>
						{:else}
							<h2>No {providerFilterLabel($providerFilter)} Sessions</h2>
							<p>{ $providerFilter === 'all' ? 'Start a Claude Code or Codex session' : `Start a ${providerFilterLabel($providerFilter)} session` }</p>
							<div class="empty-hint">
								<span class="hint-icon">
									<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
										<circle cx="12" cy="12" r="10" />
										<path d="M12 16v-4" />
										<path d="M12 8h.01" />
									</svg>
								</span>
								Sessions are detected automatically
							</div>
						{/if}
					</div>
				</div>
			{:else}

				{#if viewMode === 'project'}
					{#each projectGroups as group, gi (group.path)}
						{@const baseIdx = projectOffsets[gi] ?? 0}
						<section class="project-section" in:flyIn|global={{ index: baseIdx }} animate:flip={{ duration: 400 }}>
							<div class="project-header">
								<span class="project-name">{group.displayName}</span>
								<span class="project-count">
									{group.attention.length + group.idle.length + group.working.length}
								</span>
							</div>

							<div class="status-groups">
								<div class="status-group" class:empty={group.attention.length === 0} class:compact={isCompact}>
									<div class="status-header attention">
										<span class="status-indicator attention"></span>
										<span class="status-title">Needs Attention</span>
										<span class="status-count">{group.attention.length}</span>
									</div>
									<div class="session-grid">
										{#each group.attention as session, i (session.id)}
											<div
												class="card-wrapper"
												in:flyIn|global={{ index: baseIdx + 1 + i }}
												animate:flip={{ duration: 400 }}
											>
												<SessionCard
													{session}
													compact={isCompact}
													workersView={sessionFilter === 'workers'}
													onexpand={() => handleExpand(session)}
													onstop={() => handleStop(session.pid)}
													onopen={() => handleOpen(session.pid, session.projectPath)}
													onrename={() => showRenameHint = true}
												/>
											</div>
										{/each}
									</div>
								</div>

								<div class="status-group" class:empty={group.idle.length === 0} class:compact={isCompact}>
									<div class="status-header idle">
										<span class="status-indicator idle"></span>
										<span class="status-title">Idle</span>
										<span class="status-count">{group.idle.length}</span>
									</div>
									<div class="session-grid">
										{#each group.idle as session, i (session.id)}
											<div
												class="card-wrapper"
												in:flyIn|global={{ index: baseIdx + 1 + group.attention.length + i }}
												animate:flip={{ duration: 400 }}
											>
												<SessionCard
													{session}
													compact={isCompact}
													workersView={sessionFilter === 'workers'}
													onexpand={() => handleExpand(session)}
													onstop={() => handleStop(session.pid)}
													onopen={() => handleOpen(session.pid, session.projectPath)}
													onrename={() => showRenameHint = true}
												/>
											</div>
										{/each}
									</div>
								</div>

								<div class="status-group" class:empty={group.working.length === 0} class:compact={isCompact}>
									<div class="status-header working">
										<span class="status-indicator working"></span>
										<span class="status-title">Working</span>
										<span class="status-count">{group.working.length}</span>
									</div>
									<div class="session-grid">
										{#each group.working as session, i (session.id)}
											<div
												class="card-wrapper"
												in:flyIn|global={{ index: baseIdx + 1 + group.attention.length + group.idle.length + i }}
												animate:flip={{ duration: 400 }}
											>
												<SessionCard
													{session}
													compact={isCompact}
													workersView={sessionFilter === 'workers'}
													onexpand={() => handleExpand(session)}
													onstop={() => handleStop(session.pid)}
													onopen={() => handleOpen(session.pid, session.projectPath)}
													onrename={() => showRenameHint = true}
												/>
											</div>
										{/each}
									</div>
								</div>

							</div>
						</section>
					{/each}
				{:else}
					{#each allStatusGroups as group, gi (group.id)}
						{@const baseIdx = allGroupOffsets[gi] ?? 0}
						<section class="project-section" in:flyIn|global={{ index: baseIdx }} animate:flip={{ duration: 400 }}>
							<div class="status-header all-view {group.type}">
								<span class="status-indicator {group.type}" style="width: 8px; height: 8px;"></span>
								<span class="project-name" style="font-size: 16px;">{group.label}</span>
								<span class="project-count">{group.sessions.length}</span>
							</div>

							<div class="all-sessions-grid" class:compact={isCompact}>
								{#each group.sessions as session, i (session.id)}
									<div
										class="card-wrapper"
										in:flyIn|global={{ index: baseIdx + 1 + i }}
										animate:flip={{ duration: 400 }}
									>
										<SessionCard
											{session}
											compact={isCompact}
											workersView={sessionFilter === 'workers'}
											onexpand={() => handleExpand(session)}
											onstop={() => handleStop(session.pid)}
											onopen={() => handleOpen(session.pid, session.projectPath)}
											onrename={() => showRenameHint = true}
										/>
									</div>
								{/each}
							</div>
						</section>
					{/each}
				{/if}
			{/if}
		</div>
	</main>
	{/if}

	{#if expandedSession}
		<ExpandedCardOverlay
			session={expandedSession}
			{conversation}
			onclose={handleClose}
			onstop={() => handleStop(expandedSession.pid)}
			onopen={() => handleOpen(expandedSession.pid, expandedSession.projectPath)}
		/>
	{/if}

	{#if showQRModal}
		<QRCodeModal onclose={() => (showQRModal = false)} />
	{/if}

	<ToastNotifications />
	<DebugConsole visible={showDebugConsole} onclose={() => (showDebugConsole = false)} />

	{#if showRenameHint}
		<!-- svelte-ignore a11y_click_events_have_key_events -->
		<!-- svelte-ignore a11y_no_static_element_interactions -->
		<div class="rename-hint-backdrop" transition:fade={{ duration: 150 }} onclick={() => showRenameHint = false}>
			<div class="rename-hint-modal" onclick={(e) => e.stopPropagation()}>
				<div class="rename-hint-header">
					<span class="rename-hint-icon">
						<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
							<path d="M11 4H4a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-7" />
							<path d="M18.5 2.5a2.121 2.121 0 0 1 3 3L12 15l-4 1 1-4 9.5-9.5z" />
						</svg>
					</span>
					<span class="rename-hint-title">Rename Session</span>
				</div>
				<p class="rename-hint-text">Use the <code>/rename</code> command in your Claude Code session to rename it.</p>
				<div class="rename-hint-example">/rename my-task-name</div>
				<p class="rename-hint-sub">The new name will automatically appear in c9watch.</p>
				<button type="button" class="rename-hint-close" onclick={() => showRenameHint = false}>GOT IT</button>
			</div>
		</div>
	{/if}
</div>
{/if}

<style>
	.dashboard {
		display: flex;
		flex-direction: column;
		height: 100vh;
		width: 100vw;
		overflow: hidden;
		background: var(--bg-base);
	}

	.tab-bar {
		height: 28px;
		width: 100%;
		flex-shrink: 0;
		display: flex;
		align-items: stretch;
		background: transparent;
		z-index: 1000;
		position: relative;
		/* Left padding clears the macOS traffic light buttons (~80px) on
		   titleBarStyle: Overlay windows. Right padding matches. */
		padding: 0 var(--space-md) 0 80px;
		transition: padding-left 0.35s ease;
		/* Make the whole bar draggable (including the 80px padding over the
		   traffic lights). Child buttons opt out via -webkit-app-region: no-drag. */
		-webkit-app-region: drag;
	}

	/* Fills the right portion of the tab bar — draggable window handle area.
	   Must use -webkit-app-region: drag (not just data-tauri-drag-region attr)
	   for Tauri to actually recognize the drag zone. */
	.tab-drag-region {
		flex: 1;
		display: flex;
		align-items: center;
		justify-content: center;
		-webkit-app-region: drag;
		cursor: grab;
	}

	.global-provider-filter { display: flex; align-items: center; padding: 0 var(--space-sm); -webkit-app-region: no-drag; }

	/* Grip indicator — absolutely centered in the full window width.
	   pointer-events: none so it never blocks tab button clicks. */
	.drag-dots {
		position: absolute;
		left: 50%;
		top: 50%;
		transform: translate(-50%, -50%);
		font-size: 14px;
		letter-spacing: 3px;
		color: var(--text-muted);
		opacity: 0.5;
		user-select: none;
		line-height: 1;
		pointer-events: none;
		transition: opacity var(--transition-fast);
	}

	.tab-drag-region:hover .drag-dots {
		opacity: 0.85;
	}

	.tab-btn {
		display: flex;
		align-items: center;
		gap: var(--space-xs);
		padding: 0 var(--space-md);
		background: transparent;
		border: none;
		border-bottom: 2px solid transparent;
		color: var(--text-muted);
		cursor: pointer;
		font-family: var(--font-pixel);
		font-size: 10px;
		letter-spacing: 0.08em;
		transition: color 80ms ease-out, border-bottom-color 80ms ease-out;
		-webkit-app-region: no-drag;
	}

	.tab-btn:hover {
		color: var(--text-secondary);
	}

	.tab-btn.active {
		color: var(--text-primary);
		border-bottom-color: var(--text-primary);
	}

	.tab-icon {
		font-size: 8px;
	}

	.tab-label {
		font-size: 10px;
	}

	.grid-container {
		flex: 1;
		overflow-y: auto;
		padding: var(--space-xl);
	}

	.history-main {
		overflow: hidden;
		display: flex;
		flex-direction: column;
	}

	.sections-container {
		display: flex;
		flex-direction: column;
		gap: var(--space-3xl);
		margin: 0 auto;
		width: 100%;
	}

	.project-section {
		display: flex;
		flex-direction: column;
		gap: var(--space-xl);
	}

	.project-header {
		display: flex;
		align-items: center;
		gap: var(--space-md);
		padding-bottom: var(--space-md);
		border-bottom: 1px solid var(--text-primary);
		margin-bottom: var(--space-md);
	}

	.project-name {
		font-family: var(--font-pixel);
		font-size: 22px;
		font-weight: 600;
		color: var(--text-primary);
		text-transform: uppercase;
		letter-spacing: 0.1em;
		line-height: 1;
	}

	.project-count {
		font-family: var(--font-pixel);
		font-size: 18px;
		font-weight: 500;
		line-height: 1;
		color: var(--text-secondary);
	}

	.status-groups {
		display: flex;
		flex-direction: row;
		gap: var(--space-xl);
		overflow-x: auto;
		padding-bottom: var(--space-lg);
	}

	.status-group {
		display: flex;
		flex-direction: column;
		gap: var(--space-md);
		min-width: 350px;
		max-width: 400px;
		flex: 1;
	}

	.status-header {
		display: flex;
		align-items: center;
		padding: var(--space-sm) var(--space-md);
		background: rgba(255, 255, 255, 0.03);
		border-left: 3px solid var(--border-default);
		gap: var(--space-sm);
	}

	.status-header.attention { border-left-color: var(--status-permission); }
	.status-header.idle { border-left-color: var(--status-input); }
	.status-header.working { border-left-color: var(--status-working); }
	.status-header.connecting { border-left-color: var(--status-connecting); }

	.status-header.all-view {
		background: transparent;
		padding-left: 0;
		margin-bottom: var(--space-md);
		border-left: none;
	}

	.status-group.empty {
		opacity: 0.5;
	}

	.status-group.empty .status-header {
		background: transparent;
		border-left-style: dashed;
	}

	.status-indicator {
		width: 6px;
		height: 6px;
	}

	.status-indicator.attention {
		background: var(--status-permission);
	}

	.status-indicator.idle {
		background: var(--status-input);
	}

	.status-indicator.working {
		background: var(--status-working);
	}

	.status-indicator.connecting {
		background: var(--status-connecting);
	}

	.status-title {
		font-family: var(--font-mono);
		font-size: 12px;
		font-weight: 500;
		color: var(--text-secondary);
		text-transform: uppercase;
		letter-spacing: 0.1em;
	}

	.status-count {
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--text-muted);
	}

	.session-grid {
		display: flex;
		flex-direction: column;
		gap: var(--space-lg);
	}

	/* Empty State */
	.empty-state {
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		height: 100%;
		text-align: center;
		gap: var(--space-3xl);
		max-width: 1200px;
		margin: 0 auto;
		padding: var(--space-3xl) 0;
	}

	.empty-visual {
		position: relative;
		width: 80px;
		height: 80px;
		border: 1px solid var(--border-default);
		display: flex;
		align-items: center;
		justify-content: center;
	}

	.empty-orb {
		position: relative;
		width: 100%;
		height: 100%;
		display: flex;
		align-items: center;
		justify-content: center;
	}

	.orb-core {
		width: 8px;
		height: 8px;
		background: var(--text-muted);
		animation: pulse-glow 2s linear infinite;
	}

	.orb-ring {
		display: none;
	}

	.empty-content {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: var(--space-md);
	}

	.empty-content h2 {
		font-family: var(--font-pixel-grid);
		font-size: 18px;
		font-weight: 500;
		color: var(--text-primary);
		text-transform: uppercase;
		letter-spacing: 0.15em;
	}

	.empty-content p {
		font-family: var(--font-mono);
		font-size: 14px;
		color: var(--text-muted);
		text-transform: uppercase;
		letter-spacing: 0.05em;
	}

	.empty-hint {
		display: inline-flex;
		align-items: center;
		gap: var(--space-sm);
		margin-top: var(--space-md);
		padding: var(--space-sm) var(--space-lg);
		border: 1px solid var(--border-default);
		font-family: var(--font-mono);
		font-size: 13px;
		color: var(--text-muted);
		text-transform: uppercase;
		letter-spacing: 0.05em;
	}

	.hint-icon {
		display: flex;
		color: var(--text-muted);
	}



	.header-spacer {
		flex: 1;
	}

	.view-toggle {
		display: flex;
		gap: var(--space-xs);
		background: rgba(255, 255, 255, 0.03);
		padding: 2px;
		border: 1px solid var(--border-default);
	}

	.toggle-btn {
		display: flex;
		align-items: center;
		justify-content: center;
		width: 28px;
		height: 28px;
		border: 1px solid transparent;
		background: transparent;
		color: var(--text-muted);
		cursor: pointer;
		transition: all 0.2s ease;
	}

	.toggle-btn:hover {
		color: var(--text-secondary);
		background: rgba(255, 255, 255, 0.05);
	}

	.toggle-btn.active {
		color: var(--text-primary);
		background: rgba(255, 255, 255, 0.1);
		border-color: var(--border-default);
	}

	/* Workers-filter active state gets a pink tint to reinforce the
	   agent visual language used by the WORKER/PM badges on cards. */
	.toggle-btn.active[title="Show PM workers"] {
		color: var(--accent-pink);
		background: color-mix(in srgb, var(--accent-pink) 12%, transparent);
		border-color: color-mix(in srgb, var(--accent-pink) 40%, transparent);
	}

	.all-sessions-grid {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(350px, 1fr));
		gap: var(--space-lg);
	}

	.all-sessions-grid.compact {
		grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
		gap: var(--space-md);
	}

	.toggle-divider {
		width: 1px;
		background: var(--border-default);
		margin: 2px 4px;
	}

	.demo-toggle {
		width: auto;
		padding: 0 var(--space-sm);
		gap: var(--space-xs);
		color: var(--accent-amber);
	}

	.demo-toggle:hover {
		color: var(--accent-amber);
		background: rgba(255, 102, 0, 0.1);
	}

	.demo-toggle.active {
		background: var(--accent-amber);
		color: #000;
		border-color: var(--accent-amber);
	}

	.demo-label {
		font-family: var(--font-pixel);
		font-size: 10px;
		font-weight: 700;
		letter-spacing: 0.05em;
	}

	.mobile-connect-btn {
		width: auto;
		padding: 0 var(--space-sm);
		gap: var(--space-xs);
		color: var(--accent-blue);
	}

	.mobile-connect-btn:hover {
		color: var(--accent-blue);
		background: rgba(0, 112, 243, 0.1);
	}

	.mobile-label {
		font-family: var(--font-pixel);
		font-size: 10px;
		font-weight: 700;
		letter-spacing: 0.05em;
	}

	.demo-badge {
		font-family: var(--font-mono);
		font-size: 10px;
		font-weight: 700;
		color: var(--accent-amber);
		background: rgba(255, 102, 0, 0.12);
		padding: 2px 6px;
		border: 1px solid var(--accent-amber);
		text-transform: uppercase;
		letter-spacing: 0.1em;
		line-height: 1;
	}

	.empty-header-row {
		display: flex;
		align-items: center;
		gap: var(--space-md);
		width: 100%;
	}

	.empty-header-row :global(.status-bar) {
		flex: 1;
	}

	/* ── Fullscreen: align tab padding with content padding ────── */
	/* In macOS native fullscreen the traffic lights are gone. Replace the
	   80px clearance with var(--space-xl) to match the grid-container padding. */
	.tab-bar.fullscreen {
		padding-left: var(--space-xl);
	}

	/* ── Mobile Responsive ─────────────────────────────────────── */
	@media (max-width: 768px) {
		.tab-bar {
			height: 28px;
		}

		.tab-btn {
			display: none;
		}

		.grid-container {
			padding: var(--space-md);
		}

		.sections-container {
			gap: var(--space-xl);
		}

		.project-header {
			flex-wrap: wrap;
			gap: var(--space-sm);
		}

		.project-name {
			font-size: 16px;
		}

		.project-count {
			font-size: 14px;
		}

		/* Stack status groups vertically on mobile */
		.status-groups {
			flex-direction: column;
			overflow-x: visible;
			padding-bottom: 0;
		}

		.status-group {
			min-width: 0;
			max-width: 100%;
		}

		/* Single column grid */
		.all-sessions-grid {
			grid-template-columns: 1fr;
		}

		.all-sessions-grid.compact {
			grid-template-columns: 1fr;
		}

		.view-toggle {
			padding: 1px;
		}

		.toggle-btn {
			width: 32px;
			height: 32px;
		}

		.demo-toggle {
			padding: 0 var(--space-xs);
		}
	}

	/* ── Rename Hint Modal ───────────────────────────────────────── */
	.rename-hint-backdrop {
		position: fixed;
		inset: 0;
		background: var(--bg-overlay);
		display: flex;
		align-items: center;
		justify-content: center;
		z-index: 2000;
	}

	.rename-hint-modal {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: var(--space-md);
		padding: var(--space-2xl) var(--space-2xl);
		background: var(--bg-card);
		border: 1px solid var(--border-default);
		box-shadow: 0 20px 50px rgba(0, 0, 0, 0.5);
		max-width: 380px;
		width: 100%;
	}

	.rename-hint-header {
		display: flex;
		align-items: center;
		gap: var(--space-sm);
	}

	.rename-hint-icon {
		color: var(--text-muted);
	}

	.rename-hint-title {
		font-family: var(--font-pixel);
		font-size: 16px;
		font-weight: 600;
		color: var(--text-primary);
		text-transform: uppercase;
		letter-spacing: 0.1em;
	}

	.rename-hint-text {
		font-family: var(--font-mono);
		font-size: 13px;
		color: var(--text-secondary);
		text-align: center;
		margin: 0;
		line-height: 1.5;
	}

	.rename-hint-text code {
		font-family: var(--font-mono);
		color: var(--text-primary);
		background: var(--bg-elevated);
		padding: 2px 6px;
		border: 1px solid var(--border-default);
	}

	.rename-hint-example {
		font-family: var(--font-mono);
		font-size: 14px;
		color: var(--status-input);
		background: var(--bg-elevated);
		padding: 8px 16px;
		border: 1px solid var(--border-default);
		letter-spacing: 0.02em;
		width: 100%;
		text-align: center;
	}

	.rename-hint-sub {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--text-muted);
		text-align: center;
		margin: 0;
	}

	.rename-hint-close {
		font-family: var(--font-mono);
		font-size: 11px;
		font-weight: 600;
		color: var(--bg-base);
		background: var(--text-primary);
		border: 1px solid var(--text-primary);
		padding: 6px 24px;
		text-transform: uppercase;
		letter-spacing: 0.1em;
		cursor: pointer;
		transition: all var(--transition-fast);
		margin-top: var(--space-xs);
	}

	.rename-hint-close:hover {
		background: var(--text-secondary);
		border-color: var(--text-secondary);
	}
</style>
