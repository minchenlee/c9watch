<script lang="ts">
	import { onMount } from 'svelte';
	import { browser } from '$app/environment';
	import { deepSearchSessions, getConversation } from '$lib/api';
	import type { HistoryEntry, Conversation, DeepSearchHit } from '$lib/types';
	import HistoryCardOverlay from './HistoryCardOverlay.svelte';
	import { flyIn } from '$lib/transitions';
	import { historyEntries, historyLoading, historyError, refreshSessionHistory } from '$lib/stores/history';

	// ── Props ────────────────────────────────────────────────────────
	let { activeSessionIds = new Set<string>() }: { activeSessionIds?: Set<string> } = $props();

	// ── State ────────────────────────────────────────────────────────
	let allEntries = $derived($historyEntries);
	let loading = $derived($historyLoading);
	let error = $derived($historyError);

	let query = $state('');
	let sortOrder = $state<'newest' | 'oldest'>('newest');
	let groupByProject = $state(false);
	let collapsedProjects = $state<Set<string>>(new Set());
	let caseSensitive = $state(false);
	let wholeWord = $state(false);

	let deepSearching = $state(false);
	// Map of sessionId → matching snippet. null = no search run yet.
	let deepSearchResults = $state<Map<string, string> | null>(null);

	// Conversation viewer state
	let selectedEntry = $state<HistoryEntry | null>(null);
	let conversation = $state<Conversation | null>(null);
	// ── Persistence ──────────────────────────────────────────────────
	onMount(() => {
		if (browser) {
			const savedSort = localStorage.getItem('historySort');
			if (savedSort === 'newest' || savedSort === 'oldest') sortOrder = savedSort;
			const savedGroup = localStorage.getItem('historyGroup');
			if (savedGroup === 'true') groupByProject = true;
			if (localStorage.getItem('c9watch.historySearch.caseSensitive') === 'true')
				caseSensitive = true;
			if (localStorage.getItem('c9watch.historySearch.wholeWord') === 'true') wholeWord = true;
		}
		// History data is preloaded at app startup (see +page.svelte onMount);
		// this refresh picks up anything new if the tab was reopened later.
		refreshSessionHistory();
	});

	$effect(() => {
		if (browser) localStorage.setItem('historySort', sortOrder);
	});

	$effect(() => {
		if (browser) localStorage.setItem('historyGroup', String(groupByProject));
	});

	$effect(() => {
		if (browser)
			localStorage.setItem('c9watch.historySearch.caseSensitive', String(caseSensitive));
	});

	$effect(() => {
		if (browser) localStorage.setItem('c9watch.historySearch.wholeWord', String(wholeWord));
	});

	// Debounced deep search: fires 300ms after the query settles.
	// deepSearching is set only after the timer fires — no spinner during the
	// debounce window itself, which avoids flicker on every keystroke.
	$effect(() => {
		const q = query;
		const cs = caseSensitive;
		const ww = wholeWord;
		if (!q.trim()) {
			deepSearchResults = null;
			deepSearching = false;
			return;
		}
		deepSearchResults = null; // clear stale results from the previous query immediately
		let cancelled = false;
		const timer = setTimeout(async () => {
			deepSearching = true;
			try {
				const hits = await deepSearchSessions(q, cs, ww);
				if (!cancelled) deepSearchResults = new Map(hits.map((h) => [h.sessionId, h.snippet]));
			} catch (e) {
				if (!cancelled) console.error('Deep search failed:', e);
			} finally {
				if (!cancelled) deepSearching = false;
			}
		}, 300);
		return () => {
			cancelled = true;
			clearTimeout(timer);
		};
	});

	// ── Match helpers ────────────────────────────────────────────────
	/** Normalize a string for comparison given the current case-sensitivity mode. */
	function norm(s: string): string {
		return caseSensitive ? s : s.toLowerCase();
	}

	/** Whole-word match: true if `needle` appears in `haystack` flanked by non-alphanumerics. */
	function phraseMatch(haystack: string, needle: string): boolean {
		if (!needle) return false;
		if (!wholeWord) return haystack.includes(needle);
		let from = 0;
		while (from <= haystack.length) {
			const pos = haystack.indexOf(needle, from);
			if (pos < 0) return false;
			const before = pos === 0 ? '' : haystack.charAt(pos - 1);
			const after = pos + needle.length >= haystack.length ? '' : haystack.charAt(pos + needle.length);
			const boundaryBefore = before === '' || !/[a-z0-9]/i.test(before);
			const boundaryAfter = after === '' || !/[a-z0-9]/i.test(after);
			if (boundaryBefore && boundaryAfter) return true;
			from = pos + 1;
		}
		return false;
	}

	// ── Filtering & sorting ──────────────────────────────────────────
	let filtered = $derived.by(() => {
		let entries = allEntries;

		if (query.trim()) {
			const needle = norm(query);
			entries = entries.filter((e) => {
				const display = norm(e.display);
				const project = norm(e.projectName);
				const title = e.customTitle ? norm(e.customTitle) : '';
				return (
					phraseMatch(display, needle) ||
					phraseMatch(project, needle) ||
					phraseMatch(title, needle)
				);
			});

			// If deep search has run, also include sessions that matched full content
			if (deepSearchResults !== null) {
				const metaIds = new Set(entries.map((e) => e.sessionId));
				const deepOnly = allEntries.filter(
					(e) => deepSearchResults!.has(e.sessionId) && !metaIds.has(e.sessionId)
				);
				entries = [...entries, ...deepOnly];
			}
		} else if (deepSearchResults !== null) {
			// No text query but deep search ran — show all deep search hits
			entries = allEntries.filter((e) => deepSearchResults!.has(e.sessionId));
		}

		return [...entries].sort((a, b) =>
			sortOrder === 'newest' ? b.timestamp - a.timestamp : a.timestamp - b.timestamp
		);
	});

	// ── Grouping ─────────────────────────────────────────────────────
	let groups = $derived.by(() => {
		if (!groupByProject) return null;

		const map = new Map<string, { project: string; projectName: string; entries: HistoryEntry[] }>();
		for (const entry of filtered) {
			if (!map.has(entry.project)) {
				map.set(entry.project, { project: entry.project, projectName: entry.projectName, entries: [] });
			}
			map.get(entry.project)!.entries.push(entry);
		}

		return [...map.values()];
	});

	// Running-offset array so entry animations share a single index namespace
	// when grouped by project. Each group's starting index is the cumulative
	// count of everything before it. Group header is at `offset + 0`; inner
	// rows continue from `offset + 1`. Collapsed groups skip row contributions.
	let groupOffsets = $derived.by(() => {
		if (!groups) return [] as number[];
		const offsets: number[] = [];
		let acc = 0;
		for (const g of groups) {
			offsets.push(acc);
			const rowCount = collapsedProjects.has(g.project) ? 0 : g.entries.length;
			acc += 1 + rowCount;
		}
		return offsets;
	});

	// ── Collapse state ───────────────────────────────────────────────
	$effect(() => {
		if (!groupByProject) collapsedProjects = new Set();
	});

	let allCollapsed = $derived(
		groups !== null && groups.length > 0 && groups.every((g) => collapsedProjects.has(g.project))
	);

	function toggleProjectCollapse(project: string) {
		const next = new Set(collapsedProjects);
		if (next.has(project)) {
			next.delete(project);
		} else {
			next.add(project);
		}
		collapsedProjects = next;
	}

	// ── Actions ──────────────────────────────────────────────────────
	async function handleSelectEntry(entry: HistoryEntry) {
		selectedEntry = entry;
		conversation = null;
		try {
			conversation = await getConversation(entry.sessionId);
		} catch (e) {
			console.error('Failed to load conversation:', e);
		}
	}

	function handleCloseConversation() {
		selectedEntry = null;
		conversation = null;
	}

	// ── Helpers ──────────────────────────────────────────────────────

	/** Wrap every occurrence of the query phrase in `text` with <mark> tags.
	 * Honors the current caseSensitive + wholeWord toggle state. */
	function highlight(text: string, kw: string): string {
		if (!kw.trim()) return escapeHtml(text);
		const escapedQuery = kw.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
		const pattern = wholeWord ? `(?<![a-z0-9])${escapedQuery}(?![a-z0-9])` : escapedQuery;
		const flags = caseSensitive ? 'g' : 'gi';
		let re: RegExp;
		try {
			re = new RegExp(pattern, flags);
		} catch {
			return escapeHtml(text);
		}
		// Walk the source string so the <mark> wraps the matched substring with
		// its original casing, then escape only the surrounding segments.
		let out = '';
		let last = 0;
		for (const m of text.matchAll(re)) {
			const start = m.index ?? 0;
			out += escapeHtml(text.slice(last, start));
			out += `<mark>${escapeHtml(m[0])}</mark>`;
			last = start + m[0].length;
		}
		out += escapeHtml(text.slice(last));
		return out;
	}

	function escapeHtml(s: string): string {
		return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
	}

	function relativeTime(ms: number): string {
		const diff = Date.now() - ms;
		const mins = Math.floor(diff / 60_000);
		if (mins < 1) return 'just now';
		if (mins < 60) return `${mins}m ago`;
		const hours = Math.floor(mins / 60);
		if (hours < 24) return `${hours}h ago`;
		const days = Math.floor(hours / 24);
		if (days === 1) return 'yesterday';
		if (days < 7) return `${days}d ago`;
		return new Date(ms).toLocaleDateString();
	}

</script>

<!-- ── Search bar & controls ──────────────────────────────────────── -->
<div class="history-container">
	<div class="controls">
		<div class="section-header" in:flyIn|global={{ index: 0, duration: 350, stride: 25 }}>
			<span class="section-title">SESSION HISTORY</span>
			<span class="section-count">{allEntries.length}</span>
		</div>

		<div class="search-row" in:flyIn|global={{ index: 1, duration: 350, stride: 25 }}>
			<input
				class="search-input"
				type="text"
				placeholder="Search sessions..."
				bind:value={query}
			/>
			<div class="match-toggle" role="group" aria-label="Search match options">
				<button
					class="match-btn"
					class:active={caseSensitive}
					onclick={() => (caseSensitive = !caseSensitive)}
					title="Match case"
					aria-pressed={caseSensitive}
				>Aa</button>
				<button
					class="match-btn"
					class:active={wholeWord}
					onclick={() => (wholeWord = !wholeWord)}
					title="Match whole word"
					aria-pressed={wholeWord}
				>W</button>
			</div>
		</div>

		<div class="options-row" in:flyIn|global={{ index: 2, duration: 350, stride: 25 }}>
			<div class="sort-group">
				<button
					class="option-btn"
					class:active={sortOrder === 'newest'}
					onclick={() => (sortOrder = 'newest')}
				>NEWEST</button>
				<button
					class="option-btn"
					class:active={sortOrder === 'oldest'}
					onclick={() => (sortOrder = 'oldest')}
				>OLDEST</button>
			</div>

			<div class="sort-group">
				<button
					class="option-btn"
					class:active={!groupByProject}
					onclick={() => (groupByProject = false)}
				>FLAT</button>
				<button
					class="option-btn"
					class:active={groupByProject}
					onclick={() => (groupByProject = true)}
				>BY PROJECT</button>
			</div>

			{#if groupByProject}
			<div class="sort-group">
				<button class="option-btn" onclick={() => {
					if (allCollapsed) {
						collapsedProjects = new Set();
					} else {
						collapsedProjects = new Set(groups!.map(g => g.project));
					}
				}}>
					{allCollapsed ? 'EXPAND ALL' : 'COLLAPSE ALL'}
				</button>
			</div>
			{/if}
		</div>
	</div>

	{#if deepSearching}
		<div class="searching-indicator">Searching...</div>
	{/if}

	<!-- ── List ──────────────────────────────────────────────────── -->
	<div class="list-area">
		{#if loading}
			<div class="state-msg">Loading history...</div>
		{:else if error}
			<div class="state-msg error">Error: {error}</div>
		{:else if filtered.length === 0}
			<div class="state-msg">No sessions found.</div>
		{:else if groupByProject && groups}
			{#each groups as group, gi (group.project)}
				{@const baseIdx = (groupOffsets[gi] ?? 0) + 3}
				<div class="project-group" in:flyIn|global={{ index: baseIdx, duration: 350, stride: 25 }}>
					<!-- svelte-ignore a11y_click_events_have_key_events -->
					<!-- svelte-ignore a11y_no_static_element_interactions -->
					<div
						class="group-header"
						onclick={() => toggleProjectCollapse(group.project)}
						role="button"
						tabindex="0"
						aria-label={collapsedProjects.has(group.project) ? 'Expand group' : 'Collapse group'}
					>
						<span class="collapse-toggle" aria-hidden="true">{collapsedProjects.has(group.project) ? '▶' : '▼'}</span>
						<span class="group-name">{group.projectName}</span>
						<span class="group-count">{group.entries.length}</span>
					</div>
					{#if !collapsedProjects.has(group.project)}
						{#each group.entries as entry, i (entry.sessionId)}
							{@const snippet = query.trim() ? (deepSearchResults?.get(entry.sessionId) ?? null) : null}
							<button
								class="session-row session-row-grid"
								class:has-snippet={!!snippet}
								onclick={() => handleSelectEntry(entry)}
								in:flyIn|global={{ index: baseIdx + 1 + i, duration: 350, stride: 25 }}
							>
								<span class="row-number">{i + 1}</span>
								<span class="row-prompt">
									{#if entry.customTitle}<span class="row-title">{@html highlight(entry.customTitle, query)}</span>{/if}
									<span class="row-display">{@html highlight((snippet ?? entry.display) || '(no prompt)', query)}</span>
								</span>
								<span class="row-badge-slot">{#if activeSessionIds.has(entry.sessionId)}<span class="active-badge">ACTIVE</span>{/if}</span>
								<span class="row-time">{relativeTime(entry.timestamp)}</span>
							</button>
						{/each}
					{/if}
				</div>
			{/each}
		{:else}
			{#each filtered as entry, i (entry.sessionId)}
				{@const snippet = query.trim() ? (deepSearchResults?.get(entry.sessionId) ?? null) : null}
				<button
					class="session-row session-row-flat"
					class:has-snippet={!!snippet}
					onclick={() => handleSelectEntry(entry)}
					in:flyIn|global={{ index: i + 3, duration: 350, stride: 25 }}
				>
					<span class="row-number">{i + 1}</span>
					<div class="row-content">
						<div class="row-top-grid">
							<span class="row-project">{entry.projectName}</span>
							<span class="row-badge-slot">{#if activeSessionIds.has(entry.sessionId)}<span class="active-badge">ACTIVE</span>{/if}</span>
							<span class="row-time">{relativeTime(entry.timestamp)}</span>
						</div>
						<span class="row-prompt">
							{#if entry.customTitle}<span class="row-title">{@html highlight(entry.customTitle, query)}</span>{/if}
							<span class="row-display">{@html highlight((snippet ?? entry.display) || '(no prompt)', query)}</span>
						</span>
					</div>
				</button>
			{/each}
		{/if}

	</div>
</div>

<!-- ── Conversation overlay ───────────────────────────────────────── -->
{#if selectedEntry}
	<HistoryCardOverlay entry={selectedEntry} {conversation} searchQuery={query.trim() && deepSearchResults?.has(selectedEntry.sessionId) ? query.trim() : undefined} onclose={handleCloseConversation} />
{/if}

<style>
	.history-container {
		display: flex;
		flex-direction: column;
		height: 100%;
		overflow: hidden;
	}

	.controls {
		flex-shrink: 0;
		padding: 0 0 var(--space-md);
		display: flex;
		flex-direction: column;
		gap: var(--space-sm);
		border-bottom: 1px solid var(--border-default);
	}

	.search-row {
		display: flex;
		align-items: stretch;
		gap: var(--space-sm);
	}

	.search-input {
		flex: 1;
		min-width: 0;
		background: var(--bg-elevated);
		border: 1px solid var(--border-default);
		color: var(--text-primary);
		font-family: var(--font-mono);
		font-size: 13px;
		padding: var(--space-sm) var(--space-md);
		outline: none;
		box-sizing: border-box;
	}

	.match-toggle {
		display: flex;
		border: 1px solid var(--border-default);
		flex-shrink: 0;
	}

	.match-btn {
		font-family: var(--font-pixel);
		font-size: 11px;
		letter-spacing: 0.08em;
		width: 35.5px;
		height: 35.5px;
		display: flex;
		align-items: center;
		justify-content: center;
		padding: 0;
		background: transparent;
		border: none;
		color: var(--text-muted);
		cursor: pointer;
		text-transform: uppercase;
	}

	.match-btn:hover {
		color: var(--text-primary);
		background: rgba(255, 255, 255, 0.08);
	}

	.match-btn.active {
		background: rgba(255, 255, 255, 0.1);
		color: var(--text-primary);
	}

	.search-input:focus {
		border-color: var(--border-focus);
	}

	.search-input::placeholder {
		color: var(--text-muted);
		text-transform: uppercase;
		letter-spacing: 0.05em;
	}

	.options-row {
		display: flex;
		gap: var(--space-md);
	}

	.sort-group {
		display: flex;
		border: 1px solid var(--border-default);
	}

	.option-btn {
		font-family: var(--font-pixel);
		font-size: 10px;
		letter-spacing: 0.05em;
		padding: 4px var(--space-sm);
		background: transparent;
		border: none;
		color: var(--text-muted);
		cursor: pointer;
	}

	.option-btn.active {
		background: rgba(255, 255, 255, 0.1);
		color: var(--text-primary);
	}

	.list-area {
		flex: 1;
		overflow-y: auto;
		padding: var(--space-md) 0;
		display: flex;
		flex-direction: column;
		gap: var(--space-sm);
	}

	.state-msg {
		font-family: var(--font-mono);
		font-size: 13px;
		color: var(--text-muted);
		text-transform: uppercase;
		letter-spacing: 0.05em;
		padding: var(--space-xl) 0;
		text-align: center;
	}

	.state-msg.error {
		color: var(--accent-red);
	}

	.session-row {
		width: 100%;
		text-align: left;
		background: var(--bg-card);
		border: 1px solid var(--border-muted);
		padding: var(--space-md);
		cursor: pointer;
		display: flex;
		flex-direction: row;
		align-items: flex-start;
		gap: var(--space-md);
		transition: border-color var(--transition-fast);
	}

	.session-row:hover {
		border-color: var(--border-default);
		background: var(--bg-card-hover);
	}

	.session-row-grid {
		display: grid;
		grid-template-columns: auto minmax(0, 1fr) auto minmax(70px, auto);
		align-items: baseline;
		overflow: hidden;
	}

	.session-row-grid .row-time {
		text-align: right;
	}

	.row-top-grid {
		display: grid;
		grid-template-columns: 1fr auto minmax(70px, auto);
		align-items: baseline;
		gap: var(--space-md);
	}

	.row-top-grid .row-time {
		text-align: right;
	}

	.row-badge-slot {
		display: flex;
		justify-content: flex-end;
		min-width: 55px;
	}

	.row-project {
		font-family: var(--font-sans);
		font-size: 13px;
		font-weight: 600;
		color: var(--text-primary);
		letter-spacing: 0.05em;
	}

	.row-time {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--text-muted);
	}

	.active-badge {
		font-family: var(--font-pixel);
		font-size: 9px;
		font-weight: 700;
		color: var(--accent-green);
		background: color-mix(in srgb, var(--accent-green) 10%, transparent);
		padding: 1px 5px;
		border: 1px solid color-mix(in srgb, var(--accent-green) 30%, transparent);
		letter-spacing: 0.08em;
		line-height: 1;
		text-transform: uppercase;
		flex-shrink: 0;
	}

	.row-prompt {
		font-family: var(--font-mono);
		font-size: 13px;
		color: var(--text-secondary);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.row-title {
		color: var(--accent-amber);
		margin-right: var(--space-sm);
	}

	.row-display {
		color: var(--text-muted);
	}

	/* When a deep-search snippet is shown, allow it to wrap for readability */
	.session-row.has-snippet .row-prompt {
		white-space: normal;
		display: -webkit-box;
		-webkit-line-clamp: 2;
		line-clamp: 2;
		-webkit-box-orient: vertical;
	}

	/* Keyword highlight inside session rows */
	.row-prompt :global(mark) {
		background: transparent;
		color: var(--accent-amber);
		font-weight: 600;
	}

	.project-group {
		display: flex;
		flex-direction: column;
		gap: var(--space-xs);
		margin-bottom: var(--space-xl);
	}

	.group-header {
		display: flex;
		align-items: center;
		gap: var(--space-md);
		padding-bottom: var(--space-sm);
		border-bottom: 1px solid var(--border-default);
		margin-bottom: var(--space-sm);
	}

	.group-name {
		font-family: var(--font-sans);
		font-size: 16px;
		color: var(--text-primary);
		letter-spacing: 0.1em;
	}

	.group-count {
		font-family: var(--font-mono);
		font-size: 13px;
		color: var(--text-muted);
	}

	.searching-indicator {
		flex-shrink: 0;
		padding: var(--space-xs) var(--space-xl);
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--text-muted);
		text-transform: uppercase;
		letter-spacing: 0.05em;
	}

	.group-header {
		cursor: pointer;
	}

	.group-header:hover .group-name {
		color: var(--text-primary);
	}

	.collapse-toggle {
		color: var(--text-muted);
		font-family: var(--font-mono);
		font-size: 11px;
		line-height: 1;
		flex-shrink: 0;
	}

	.section-header {
		display: flex;
		align-items: center;
		gap: var(--space-md);
		padding-bottom: var(--space-md);
		border-bottom: 1px solid var(--text-primary);
		margin-bottom: var(--space-md);
		flex-shrink: 0;
	}

	.section-title {
		font-family: var(--font-pixel);
		font-size: 22px;
		font-weight: 600;
		color: var(--text-primary);
		text-transform: uppercase;
		letter-spacing: 0.1em;
		line-height: 1;
	}

	.section-count {
		font-family: var(--font-pixel);
		font-size: 18px;
		font-weight: 500;
		line-height: 1;
		color: var(--text-secondary);
	}


	.row-number {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--text-muted);
		line-height: 1.6;
		flex-shrink: 0;
		min-width: 1.5em;
		text-align: right;
		user-select: none;
	}

	.row-content {
		flex: 1;
		min-width: 0;
		display: flex;
		flex-direction: column;
		gap: var(--space-xs);
	}

</style>
