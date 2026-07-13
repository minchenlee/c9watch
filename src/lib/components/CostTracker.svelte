<script lang="ts">
	import { onMount } from 'svelte';
	import { getConversation } from '$lib/api';
	import type { HistoryEntry, Conversation, CostData, SessionCostRecord, SessionProvider } from '$lib/types';
	import TokenDistanceVisualizer from './token-distance/TokenDistanceVisualizer.svelte';
	import HistoryCardOverlay from './HistoryCardOverlay.svelte';
	import { costData as costDataStore, costMode, refreshCostData } from '$lib/stores/cost';
	import { formatCost, formatTokens, modelDisplayName } from '$lib/cost-utils';
	import { flyIn, fadeIn } from '$lib/transitions';
	import { providerFilter } from '$lib/stores/provider-filter';
	import { matchesProvider, providerFilterLabel } from '$lib/provider';
	import ProviderBadge from './ProviderBadge.svelte';
	import { isCostAvailable, selectChartDays, summarizeCostSessions } from '$lib/cost-semantics';

	type TimeScale = 'daily' | 'weekly' | 'monthly';
	type CostSessionRow = SessionCostRecord & {
		models: string[];
		unpricedTokens: number;
	};

	interface TimeBucket {
		key: string;
		label: string;
		cost: number;
		tokens: number;
		unpricedTokens: number;
		sessions: import('$lib/types').SessionCostRecord[];
		subBuckets?: { label: string; cost: number; tokens: number; unpricedTokens: number; sessions: import('$lib/types').SessionCostRecord[] }[];
	}

	function sumTokens(sessions: import('$lib/types').SessionCostRecord[]): number {
		return sessions.reduce((sum, s) => sum + (s.totalTokens || 0), 0);
	}

	function providerUsageValue(sessions: SessionCostRecord[], provider: SessionProvider): number {
		return sessions
			.filter((session) => (session.provider === 'codex' ? 'codex' : 'claudeCode') === provider)
			.reduce((sum, session) => {
				if (mode === 'tokens') return sum + (session.totalTokens || 0);
				return sum + (isCostAvailable(session) ? session.cost : 0);
			}, 0);
	}

	function claudeUsageShare(sessions: SessionCostRecord[]): number {
		const claude = providerUsageValue(sessions, 'claudeCode');
		const codex = providerUsageValue(sessions, 'codex');
		return claude + codex > 0 ? (claude / (claude + codex)) * 100 : 100;
	}

	function providerBreakdownLabel(sessions: SessionCostRecord[]): string {
		const claude = providerUsageValue(sessions, 'claudeCode');
		const codex = providerUsageValue(sessions, 'codex');
		const total = claude + codex;
		const format = mode === 'tokens' ? formatTokens : formatCost;
		const percentage = (value: number) => total > 0 ? `${((value / total) * 100).toFixed(0)}%` : '0%';
		return `Claude Code ${format(claude)} (${percentage(claude)}), Codex ${format(codex)} (${percentage(codex)})`;
	}

	function formatAggregate(cost: number, tokens: number, unpricedTokens: number): string {
		if (mode === 'tokens') return formatTokens(tokens);
		if (tokens > 0 && unpricedTokens === tokens) return 'UNPRICED';
		return unpricedTokens > 0 ? `${formatCost(cost)} PRICED` : formatCost(cost);
	}

	/** Merge per-day/per-model accounting fragments into one visible session row. */
	function mergeSessionRows(sessions: SessionCostRecord[]): CostSessionRow[] {
		const rows = new Map<string, CostSessionRow>();
		for (const [index, session] of sessions.entries()) {
			const provider = session.provider === 'codex' ? 'codex' : 'claudeCode';
			const key = session.sessionId
				? `${provider}:${session.sessionId}`
				: `${provider}:${session.date}:${session.timestamp}:${session.model}:${index}`;
			const unpricedTokens = isCostAvailable(session) ? 0 : session.totalTokens;
			const model = session.model || 'unknown';
			const existing = rows.get(key);
			if (!existing) {
				rows.set(key, { ...session, models: [model], unpricedTokens });
				continue;
			}

			existing.cost += session.cost;
			existing.inputTokens += session.inputTokens;
			existing.cachedInputTokens += session.cachedInputTokens;
			existing.outputTokens += session.outputTokens;
			existing.reasoningOutputTokens += session.reasoningOutputTokens;
			existing.totalTokens += session.totalTokens;
			existing.unpricedTokens += unpricedTokens;
			existing.costAvailable = existing.unpricedTokens < existing.totalTokens;
			if (!existing.models.includes(model)) existing.models.push(model);
			if (session.timestamp > existing.timestamp) {
				existing.timestamp = session.timestamp;
				existing.date = session.date;
				existing.surface = session.surface;
				existing.agentKind = session.agentKind;
			}
			if (session.sessionName) existing.sessionName = session.sessionName;
		}
		return Array.from(rows.values());
	}

	function sessionModelLabel(session: CostSessionRow): string {
		if (session.models.length === 1) {
			const model = session.models[0];
			return model === 'unknown' ? 'UNPRICED' : modelDisplayName(model);
		}
		return `${session.models.length} MODELS`;
	}

	function sessionModelTitle(session: CostSessionRow): string {
		return session.models
			.map((model) => model === 'unknown' ? 'Unpriced / unknown model' : modelDisplayName(model))
			.join(', ');
	}

	const MODEL_COLOR_PALETTE = [
		'var(--accent-blue)',
		'#00c2a8',
		'var(--accent-green)',
		'var(--accent-purple)',
		'var(--accent-pink)',
		'#f5c542',
		'var(--accent-red)',
		'#22d3ee'
	];

	/** Keep model colors stable across the bar and legend, regardless of usage order. */
	function modelColor(model: string, provider: SessionProvider): string {
		const normalized = model.trim().toLowerCase();
		if (!normalized || normalized === '-') return 'var(--text-muted)';

		if (normalized.startsWith('claude-opus')) return 'var(--accent-amber)';
		if (normalized.startsWith('claude-sonnet')) return 'var(--accent-purple)';
		if (normalized.startsWith('claude-haiku')) return 'var(--accent-pink)';

		if (normalized.includes('terra')) return '#00c2a8';
		if (normalized.includes('sol')) return 'var(--accent-blue)';
		if (normalized.includes('luna')) return 'var(--accent-green)';
		if (normalized.includes('spark')) return 'var(--accent-pink)';
		if (normalized.includes('mini') || normalized.includes('nano')) return '#f5c542';
		if (normalized.startsWith('gpt-5.5')) return 'var(--accent-purple)';
		if (normalized.startsWith('gpt-5.4')) return 'var(--accent-red)';

		if (provider === 'codex' || normalized.startsWith('gpt-')) {
			let hash = 0;
			for (let i = 0; i < normalized.length; i++) {
				hash = ((hash << 5) - hash + normalized.charCodeAt(i)) | 0;
			}
			return MODEL_COLOR_PALETTE[Math.abs(hash) % MODEL_COLOR_PALETTE.length];
		}

		return 'var(--text-muted)';
	}

	// ── State ────────────────────────────────────────────────────────
	let loading = $state(true);
	let rawCostData = $derived($costDataStore);
	let costData = $derived.by((): CostData | null => {
		if (!rawCostData) return null;
		const dailyCosts = rawCostData.dailyCosts.map((day) => {
			const sessions = day.sessions.filter((session) => matchesProvider(session, $providerFilter));
			return { ...day, sessions, cost: summarizeCostSessions(sessions).cost };
		});
		const projectCosts = rawCostData.projectCosts.map((project) => {
			const sessions = project.sessions.filter((session) => matchesProvider(session, $providerFilter));
			return { ...project, sessions, totalCost: summarizeCostSessions(sessions).cost };
		}).filter((project) => project.sessions.length > 0);
		const sessions = dailyCosts.flatMap((day) => day.sessions);
		const summary = summarizeCostSessions(sessions);
		return {
			...rawCostData,
			dailyCosts,
			projectCosts,
			totalCost: summary.cost,
			totalTokens: summary.tokens,
			unpricedTokens: summary.unpricedTokens
		};
	});
	let mode = $derived($costMode);
	let hasCodexUsage = $derived(costData?.dailyCosts.some((day) => day.sessions.some((session) => session.provider === 'codex')) ?? false);
	let collapsedProjects = $state<Set<string>>(new Set());
	let modelTrackWidth = $state(0);
	let projectTrackWidth = $state(0);
	let timeScale = $state<TimeScale>('daily');
	let dropdownOpen = $state(false);
	let hoveredBucket = $state<string | null>(null);
	let expandedProjects = $state<Set<string>>(new Set());
	let showVisualizer = $state(false);
	let selectedEntry = $state<HistoryEntry | null>(null);
	let conversation = $state<Conversation | null>(null);

	type SortField = 'date' | 'cost';
	type SortDir = 'asc' | 'desc';
	let sessionSortField = $state<SortField>('date');
	let sessionSortDir = $state<SortDir>('desc');

	// ── Helpers ──────────────────────────────────────────────────────
	/** Format a Date object as YYYY-MM-DD in local time */
	function toLocalDateStr(d: Date): string {
		return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
	}

	function formatDayLabel(dateStr: string): string {
		const now = new Date();
		const today = toLocalDateStr(now);
		const yd = new Date(now);
		yd.setDate(yd.getDate() - 1);
		const yesterday = toLocalDateStr(yd);
		if (dateStr === today) return 'TODAY';
		if (dateStr === yesterday) return 'YESTERDAY';
		const d = new Date(dateStr + 'T00:00:00');
		return d.toLocaleDateString('en-US', { month: 'short', day: 'numeric' }).toUpperCase();
	}

	/** Get ISO week start (Monday) for a date string */
	function getWeekStart(dateStr: string): string {
		const d = new Date(dateStr + 'T00:00:00');
		const day = d.getDay();
		const diff = d.getDate() - day + (day === 0 ? -6 : 1); // Monday
		const monday = new Date(d);
		monday.setDate(diff);
		return toLocalDateStr(monday);
	}

	function formatWeekLabel(weekStartStr: string): string {
		const now = new Date();
		const thisWeekStart = getWeekStart(toLocalDateStr(now));
		const lastWeekDate = new Date(thisWeekStart + 'T00:00:00');
		lastWeekDate.setDate(lastWeekDate.getDate() - 7);
		const lastWeekStart = toLocalDateStr(lastWeekDate);

		if (weekStartStr === thisWeekStart) return 'THIS WEEK';
		if (weekStartStr === lastWeekStart) return 'LAST WEEK';

		const start = new Date(weekStartStr + 'T00:00:00');
		const end = new Date(start.getTime() + 6 * 86400000);
		const fmt = (d: Date) => d.toLocaleDateString('en-US', { month: 'short', day: 'numeric' }).toUpperCase();
		return `${fmt(start)}–${fmt(end)}`;
	}

	function formatMonthLabel(monthKey: string): string {
		const now = new Date();
		const thisMonth = `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, '0')}`;
		const lastMonthDate = new Date(now.getFullYear(), now.getMonth() - 1, 1);
		const lastMonth = `${lastMonthDate.getFullYear()}-${String(lastMonthDate.getMonth() + 1).padStart(2, '0')}`;

		if (monthKey === thisMonth) return 'THIS MONTH';
		if (monthKey === lastMonth) return 'LAST MONTH';

		const d = new Date(monthKey + '-01T00:00:00');
		return d.toLocaleDateString('en-US', { month: 'short', year: 'numeric' }).toUpperCase();
	}

	function formatTime(timestamp: string): string {
		const d = new Date(timestamp);
		return d.toLocaleTimeString('en-US', { hour: '2-digit', minute: '2-digit', hour12: false });
	}

	function formatDateTime(timestamp: string): string {
		const d = new Date(timestamp);
		const date = d.toLocaleDateString('en-US', { month: 'short', day: 'numeric' }).toUpperCase();
		const time = d.toLocaleTimeString('en-US', { hour: '2-digit', minute: '2-digit', hour12: false });
		return `${date} ${time}`;
	}

	function toggleSort(field: SortField) {
		if (sessionSortField === field) {
			sessionSortDir = sessionSortDir === 'desc' ? 'asc' : 'desc';
		} else {
			sessionSortField = field;
			sessionSortDir = 'desc';
		}
	}

	function sortSessions<T extends SessionCostRecord>(sessions: T[]): T[] {
		const dir = sessionSortDir === 'desc' ? -1 : 1;
		return [...sessions].sort((a, b) => {
			if (sessionSortField === 'cost') {
				return (mode === 'usd' ? (a.cost - b.cost) : (a.totalTokens - b.totalTokens)) * dir;
			}
			return a.timestamp.localeCompare(b.timestamp) * dir;
		});
	}

	let loadingConversation = $state(false);

	async function handleSessionClick(session: import('$lib/types').SessionCostRecord) {
		if (loadingConversation) return;
		loadingConversation = true;
		selectedEntry = {
			sessionId: session.sessionId,
			display: session.sessionName || session.sessionId.slice(0, 8),
			timestamp: new Date(session.timestamp).getTime(),
			project: session.project,
			projectName: session.projectName,
			customTitle: session.sessionName || null,
			provider: session.provider,
			surface: session.surface,
		};
		conversation = null;
		try {
			conversation = await getConversation(session.sessionId);
		} catch (e) {
			console.error('Failed to load conversation:', e);
		} finally {
			loadingConversation = false;
		}
	}

	/** Returns the date range [startInclusive, endExclusive) for current time scale window */
	function getTimeWindow(): { start: string; end: string } | null {
		const now = new Date();
		const todayStr = toLocalDateStr(now);

		if (timeScale === 'daily') {
			const tomorrow = new Date(now);
			tomorrow.setDate(tomorrow.getDate() + 1);
			return { start: todayStr, end: toLocalDateStr(tomorrow) };
		}
		if (timeScale === 'weekly') {
			const weekStart = getWeekStart(todayStr);
			const weekEnd = new Date(weekStart + 'T00:00:00');
			weekEnd.setDate(weekEnd.getDate() + 7);
			return { start: weekStart, end: toLocalDateStr(weekEnd) };
		}
		// monthly
		const monthStart = `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, '0')}-01`;
		const nextMonth = new Date(now.getFullYear(), now.getMonth() + 1, 1);
		return { start: monthStart, end: toLocalDateStr(nextMonth) };
	}

	// ── Derived ──────────────────────────────────────────────────────
	let timeBuckets = $derived.by((): TimeBucket[] => {
		if (!costData) return [];
		const days = selectChartDays(costData.dailyCosts, mode);
		const today = toLocalDateStr(new Date());

		if (timeScale === 'daily') {
			return days.slice(0, 14).map(d => {
				const summary = summarizeCostSessions(d.sessions);
				return {
					key: d.date,
					label: formatDayLabel(d.date),
					cost: summary.cost,
					tokens: summary.tokens,
					unpricedTokens: summary.unpricedTokens,
					sessions: d.sessions
				};
			});
		}

		if (timeScale === 'weekly') {
			if (days.length === 0) return [];
			// Build data map from actual sessions
			const weekMap = new Map<string, { cost: number; sessions: typeof days[0]['sessions']; dayBuckets: Map<string, { cost: number; sessions: typeof days[0]['sessions'] }> }>();
			for (const d of days) {
				const wk = getWeekStart(d.date);
				if (!weekMap.has(wk)) weekMap.set(wk, { cost: 0, sessions: [], dayBuckets: new Map() });
				const entry = weekMap.get(wk)!;
				entry.cost += d.cost;
				entry.sessions.push(...d.sessions);
				entry.dayBuckets.set(d.date, { cost: d.cost, sessions: d.sessions });
			}
			// Generate last 4 weeks anchored to today, newest→oldest
			// (Claude Code auto-deletes logs after 30 days)
			const thisWeek = getWeekStart(today);
			return Array.from({ length: 4 }, (_, i) => {
				const d = new Date(thisWeek + 'T00:00:00');
				d.setDate(d.getDate() - i * 7);
				return toLocalDateStr(d);
			}).map(wk => {
				const data = weekMap.get(wk);
				return {
					key: wk,
					label: formatWeekLabel(wk),
					cost: data?.cost ?? 0,
					tokens: data ? sumTokens(data.sessions) : 0,
					unpricedTokens: data ? summarizeCostSessions(data.sessions).unpricedTokens : 0,
					sessions: data?.sessions ?? [],
					subBuckets: data ? Array.from(data.dayBuckets.entries())
						.sort(([a], [b]) => b.localeCompare(a))
						.map(([date, d]) => ({ label: formatDayLabel(date), cost: d.cost, tokens: sumTokens(d.sessions), unpricedTokens: summarizeCostSessions(d.sessions).unpricedTokens, sessions: d.sessions })) : []
				};
			});
		}

		// monthly
		if (days.length === 0) return [];
		const monthMap = new Map<string, { cost: number; sessions: typeof days[0]['sessions']; weekBuckets: Map<string, { cost: number; sessions: typeof days[0]['sessions'] }> }>();
		for (const d of days) {
			const mk = d.date.slice(0, 7);
			if (!monthMap.has(mk)) monthMap.set(mk, { cost: 0, sessions: [], weekBuckets: new Map() });
			const entry = monthMap.get(mk)!;
			entry.cost += d.cost;
			entry.sessions.push(...d.sessions);
			const wk = getWeekStart(d.date);
			if (!entry.weekBuckets.has(wk)) entry.weekBuckets.set(wk, { cost: 0, sessions: [] });
			const wEntry = entry.weekBuckets.get(wk)!;
			wEntry.cost += d.cost;
			wEntry.sessions.push(...d.sessions);
		}
		// Generate last 2 months anchored to this month, newest→oldest
		// (Claude Code auto-deletes logs after 30 days)
		const now = new Date();
		return Array.from({ length: 2 }, (_, i) => {
			const d = new Date(now.getFullYear(), now.getMonth() - i, 1);
			return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}`;
		}).map(mk => {
			const data = monthMap.get(mk);
			return {
				key: mk,
				label: formatMonthLabel(mk),
				cost: data?.cost ?? 0,
				tokens: data ? sumTokens(data.sessions) : 0,
				unpricedTokens: data ? summarizeCostSessions(data.sessions).unpricedTokens : 0,
				sessions: data?.sessions ?? [],
				subBuckets: data ? Array.from(data.weekBuckets.entries())
					.sort(([a], [b]) => b.localeCompare(a))
					.map(([wk, w]) => ({ label: formatWeekLabel(wk), cost: w.cost, tokens: sumTokens(w.sessions), unpricedTokens: summarizeCostSessions(w.sessions).unpricedTokens, sessions: w.sessions })) : []
			};
		});
	});

	/** Chronological order (oldest → newest) for the bar chart */
	let chronoBuckets = $derived([...timeBuckets].reverse());

	let maxBucketCost = $derived.by(() => {
		if (timeBuckets.length === 0) return 0;
		return Math.max(...timeBuckets.map(b => b.cost));
	});

	let maxBucketTokens = $derived.by(() => {
		if (timeBuckets.length === 0) return 0;
		return Math.max(...timeBuckets.map(b => b.tokens));
	});

	let bucketScaleMax = $derived(mode === 'usd' ? maxBucketCost : maxBucketTokens);
	let maxProjectTokens = $derived.by(() => {
		if (filteredProjectCosts.length === 0) return 0;
		return Math.max(...filteredProjectCosts.map(p => p.totalTokens));
	});

	let scaleLabel = $derived(
		timeScale === 'daily' ? 'DAILY' : timeScale === 'weekly' ? 'WEEKLY' : 'MONTHLY'
	);

	let scaleSectionTitle = $derived.by(() => {
		const suffix = mode === 'usd' ? 'COST' : 'TOKENS';
		const prefix = timeScale === 'daily' ? 'DAILY' : timeScale === 'weekly' ? 'WEEKLY' : 'MONTHLY';
		return `${prefix} ${suffix}`;
	});

	/** Model costs filtered to the active time window */
	let filteredModelCosts = $derived.by((): Array<{ key: string; model: string; provider: import('$lib/types').SessionProvider; displayName: string; cost: number; tokens: number; unpricedTokens: number; percentage: number }> => {
		if (!costData) return [];
		const tw = getTimeWindow();

		const sessions = costData.dailyCosts
			.filter(d => !tw || (d.date >= tw.start && d.date < tw.end))
			.flatMap(d => d.sessions);

		const modelMap = new Map<string, { model: string; provider: import('$lib/types').SessionProvider; cost: number; tokens: number; unpricedTokens: number }>();
		for (const s of sessions) {
			const provider = s.provider === 'codex' ? 'codex' : 'claudeCode';
			const key = `${provider}:${s.model}`;
			const cur = modelMap.get(key) || { model: s.model, provider, cost: 0, tokens: 0, unpricedTokens: 0 };
			if (isCostAvailable(s)) cur.cost += s.cost;
			cur.tokens += s.totalTokens || 0;
			if (!isCostAvailable(s)) cur.unpricedTokens += s.totalTokens || 0;
			modelMap.set(key, cur);
		}

		const totalCost = Array.from(modelMap.values()).reduce((a, b) => a + b.cost, 0);
		const totalTokens = Array.from(modelMap.values()).reduce((a, b) => a + b.tokens, 0);
		return Array.from(modelMap.entries())
			.map(([key, v]) => ({
				key,
				model: v.model,
				provider: v.provider,
				displayName: modelDisplayName(v.model),
				cost: v.cost,
				tokens: v.tokens,
				unpricedTokens: v.unpricedTokens,
				percentage: mode === 'tokens'
					? (totalTokens > 0 ? (v.tokens / totalTokens) * 100 : 0)
					: (totalCost > 0 ? (v.cost / totalCost) * 100 : 0)
			}))
			.sort((a, b) => mode === 'tokens' ? b.tokens - a.tokens : b.cost - a.cost);
	});

	/** Project costs filtered to the active time window */
	let filteredProjectCosts = $derived.by(() => {
		if (!costData) return [] as Array<{ project: string; projectName: string; totalCost: number; totalTokens: number; unpricedTokens: number; sessions: SessionCostRecord[]; displaySessions: CostSessionRow[] }>;
		const tw = getTimeWindow();

		const projMap = new Map<string, { project: string; projectName: string; totalCost: number; totalTokens: number; unpricedTokens: number; sessions: SessionCostRecord[]; displaySessions: CostSessionRow[] }>();
		for (const proj of costData.projectCosts) {
			const filtered = tw ? proj.sessions.filter(s => s.date >= tw.start && s.date < tw.end) : proj.sessions;
			if (filtered.length === 0) continue;
			const summary = summarizeCostSessions(filtered);
			projMap.set(proj.project, { project: proj.project, projectName: proj.projectName, totalCost: summary.cost, totalTokens: summary.tokens, unpricedTokens: summary.unpricedTokens, sessions: filtered, displaySessions: mergeSessionRows(filtered) });
		}
		return Array.from(projMap.values()).sort((a, b) => mode === 'tokens' ? b.totalTokens - a.totalTokens : b.totalCost - a.totalCost);
	});

	/** Total cost filtered to the active time window */
	let filteredTotalCost = $derived.by(() => {
		if (!costData) return 0;
		const tw = getTimeWindow();
		if (!tw) return costData.totalCost;
		return costData.dailyCosts
			.filter(d => d.date >= tw.start && d.date < tw.end)
			.reduce((sum, d) => sum + summarizeCostSessions(d.sessions).cost, 0);
	});

	/** Total tokens filtered to the active time window */
	let filteredTotalTokens = $derived.by(() => {
		if (!costData) return 0;
		const tw = getTimeWindow();
		if (!tw) return costData.totalTokens;
		return costData.dailyCosts
			.filter(d => d.date >= tw.start && d.date < tw.end)
			.reduce((sum, d) => sum + sumTokens(d.sessions), 0);
	});

	let filteredUnpricedTokens = $derived.by(() => {
		if (!costData) return 0;
		const tw = getTimeWindow();
		return costData.dailyCosts
			.filter(d => !tw || (d.date >= tw.start && d.date < tw.end))
			.reduce((sum, d) => sum + summarizeCostSessions(d.sessions).unpricedTokens, 0);
	});

	let allCollapsed = $derived(
		filteredProjectCosts.length > 0 &&
		filteredProjectCosts.every(p => collapsedProjects.has(p.project))
	);

	let maxProjectCost = $derived.by(() => {
		if (filteredProjectCosts.length === 0) return 0;
		return Math.max(...filteredProjectCosts.map(p => p.totalCost));
	});

	let projectScaleMax = $derived(mode === 'usd' ? maxProjectCost : maxProjectTokens);

	// Grid-block helpers for inline bars
	let modelBarColumns = $derived(Math.max(1, Math.floor((modelTrackWidth - 6) / 10)));
	let projectBarColumns = $derived(Math.max(1, Math.floor((projectTrackWidth - 6) / 10)));

	/** Combined model bar: allocates blocks proportionally like StatusBar */
	let modelStatusArray = $derived.by(() => {
		if (filteredModelCosts.length === 0) return Array<string>(modelBarColumns).fill('');

		const models = filteredModelCosts;
		const percentages = models.map(mc => (mc.percentage / 100) * modelBarColumns);
		const integerParts = percentages.map(p => Math.floor(p));
		const remainders = percentages.map((p, i) => p - integerParts[i]);
		const result = [...integerParts];
		let allocated = result.reduce((a, b) => a + b, 0);

		while (allocated < modelBarColumns) {
			let maxR = -1, maxI = -1;
			for (let i = 0; i < remainders.length; i++) {
				if (remainders[i] > maxR) { maxR = remainders[i]; maxI = i; }
			}
			if (maxI === -1) break;
			result[maxI]++;
			remainders[maxI] = -1;
			allocated++;
		}

		const arr: string[] = [];
		for (let i = 0; i < models.length; i++) {
			const color = modelColor(models[i].model, models[i].provider);
			for (let j = 0; j < result[i]; j++) arr.push(color);
		}
		while (arr.length < modelBarColumns) arr.push('');
		return arr;
	});

	/** Build a project bar with provider-specific blocks, then fill the remainder. */
	function buildProviderBarBlocks(fillPct: number, totalCols: number, sessions: SessionCostRecord[]): Array<{ type: string }> {
		const filled = Math.round((fillPct / 100) * totalCols);
		const claude = providerUsageValue(sessions, 'claudeCode');
		const codex = providerUsageValue(sessions, 'codex');
		const total = claude + codex;
		const claudeBlocks = total > 0 ? Math.round((claude / total) * filled) : 0;
		const arr: Array<{ type: string }> = [];
		for (let i = 0; i < claudeBlocks; i++) arr.push({ type: 'claude' });
		for (let i = claudeBlocks; i < filled; i++) arr.push({ type: 'codex' });
		while (arr.length < totalCols) arr.push({ type: 'empty' });
		return arr;
	}

	// ── Actions ──────────────────────────────────────────────────────
	function setTimeScale(scale: TimeScale) {
		timeScale = scale;
		collapsedProjects = new Set();
	}

	function toggleProjectCollapse(project: string) {
		const next = new Set(collapsedProjects);
		if (next.has(project)) {
			next.delete(project);
		} else {
			next.add(project);
		}
		collapsedProjects = next;
	}

	function toggleAllProjects() {
		if (allCollapsed) {
			collapsedProjects = new Set();
		} else {
			collapsedProjects = new Set(filteredProjectCosts.map(p => p.project));
		}
	}

	// ── Click-outside to close dropdown ─────────────────────────────
	$effect(() => {
		if (!dropdownOpen) return;
		const close = () => { dropdownOpen = false; };
		document.addEventListener('click', close);
		return () => document.removeEventListener('click', close);
	});

	// ── Lifecycle ────────────────────────────────────────────────────
	onMount(async () => {
		await refreshCostData();
		collapsedProjects = new Set();
		loading = false;
	});
</script>

<div class="cost-container">
	<!-- ── Header ─────────────────────────────────────────────────── -->
	<!-- svelte-ignore a11y_click_events_have_key_events -->
	<!-- svelte-ignore a11y_no_static_element_interactions -->
	<div class="section-header">
		<span class="section-title">COST TRACKER</span>
		{#if costData}
			<span class="section-total" class:unpriced={mode === 'usd' && filteredUnpricedTokens > 0} in:flyIn={{ index: 0, stride: 40 }}>{formatAggregate(filteredTotalCost, filteredTotalTokens, filteredUnpricedTokens)}</span>
			<div class="mode-toggle" role="group" aria-label="Display mode" in:flyIn={{ index: 1, stride: 40 }}>
				<button
					class="mode-btn"
					class:active={mode === 'usd'}
					onclick={() => costMode.set('usd')}
					title="Show costs in USD"
				>USD</button>
				<button
					class="mode-btn"
					class:active={mode === 'tokens'}
					onclick={() => costMode.set('tokens')}
					title="Show totals in tokens"
				>TOKENS</button>
			</div>
			<button class="distance-btn" onclick={() => showVisualizer = true} in:flyIn={{ index: 2, stride: 40 }}>
				DISTANCE
			</button>
		{/if}
		<div class="scale-dropdown" onclick={(e) => e.stopPropagation()}>
			<button class="scale-trigger" onclick={() => dropdownOpen = !dropdownOpen}>
				{scaleLabel} ▾
			</button>
			{#if dropdownOpen}
				<div class="scale-menu">
					{#each ['daily', 'weekly', 'monthly'] as scale}
						<button
							class="scale-option"
							class:active={timeScale === scale}
							onclick={() => { setTimeScale(scale as TimeScale); dropdownOpen = false; }}
						>
							{scale.toUpperCase()}
						</button>
					{/each}
				</div>
			{/if}
		</div>
	</div>

	{#if loading}
		<div class="state-msg">Loading cost data...</div>
	{:else if !costData}
		<div class="state-msg">No cost data available.</div>
		{:else}
		<div class="list-area">
			{#if mode === 'usd' && (hasCodexUsage || filteredUnpricedTokens > 0)}
				<div class="pricing-note" role="note">
					<span class="pricing-note-label">{hasCodexUsage ? 'ESTIMATED USD' : 'USD COVERAGE'}</span>
					<span>
						{#if hasCodexUsage}Codex cost is a lower-bound estimate using OpenAI Standard short-context API rates. Long-context calls may cost more. It is not your ChatGPT/Codex subscription bill.{/if}
						{#if hasCodexUsage && filteredUnpricedTokens > 0}<span aria-hidden="true"> · </span>{/if}
						{#if filteredUnpricedTokens > 0}
							{#if filteredUnpricedTokens === filteredTotalTokens}
								Pricing unavailable for {formatTokens(filteredUnpricedTokens)} tracked tokens. Token totals remain complete.
							{:else}
								{formatCost(filteredTotalCost)} priced · {formatTokens(filteredUnpricedTokens)} tokens unpriced. USD totals exclude unpriced usage.
							{/if}
						{/if}
					</span>
				</div>
			{/if}
			<div class="provider-usage-legend" aria-label="Cost chart provider colors">
				<span class="provider-usage-legend-item"><span class="provider-usage-swatch claude" aria-hidden="true"></span>CLAUDE CODE</span>
				<span class="provider-usage-legend-item"><span class="provider-usage-swatch codex" aria-hidden="true"></span>CODEX</span>
			</div>
			{#if filteredProjectCosts.length === 0}
				<div class="state-msg">No {providerFilterLabel($providerFilter)} usage data available.</div>
			{/if}
			<!-- ── BY MODEL ───────────────────────────────────────── -->
			<div class="model-status-bar" in:flyIn={{ index: 0, duration: 650, stride: 90 }}>
				<div class="sub-header">BY MODEL</div>

				<div class="progress-track" bind:clientWidth={modelTrackWidth}>
					<div class="grid-container" style="grid-template-columns: repeat({modelBarColumns}, 1fr);">
						{#each modelStatusArray as status, i}
							<div class="rect" class:filled={status !== ''} style={status ? `--model-color: ${status}` : undefined}></div>
						{/each}
					</div>
				</div>

				<div class="model-legend">
					{#each filteredModelCosts as mc (mc.key)}
						<div class="model-legend-item">
							<span class="dot" style={`--model-color: ${modelColor(mc.model, mc.provider)}`}></span>
							<span class="model-legend-label">{mc.displayName.toUpperCase()}</span>
							<span class="model-legend-cost" class:unpriced={mode === 'usd' && mc.unpricedTokens === mc.tokens}>{formatAggregate(mc.cost, mc.tokens, mc.unpricedTokens)}</span>
							<span class="model-legend-pct">{mode === 'usd' && mc.unpricedTokens === mc.tokens ? '—' : `${mc.percentage.toFixed(0)}%`}</span>
						</div>
					{/each}
				</div>

				<div class="deco-mesh"></div>
			</div>

			<!-- ── TIME-BASED COST ────────────────────────────────── -->
			<div class="cost-section" in:flyIn={{ index: 1, duration: 650, stride: 90 }}>
				<div class="sub-header">{scaleSectionTitle}</div>

				{#if mode === 'usd' && chronoBuckets.length === 0 && filteredUnpricedTokens > 0}
					<div class="chart-unpriced-state">NO PRICED USD DATA · {formatTokens(filteredUnpricedTokens)} TOKENS AVAILABLE IN TOKENS MODE</div>
				{:else}
				<div class="vchart-area">
					{#each chronoBuckets as bucket (bucket.key)}
						{@const barValue = mode === 'usd' ? bucket.cost : bucket.tokens}
						{@const claudeShare = claudeUsageShare(bucket.sessions)}
						<div
							class="vchart-col"
							onmouseenter={() => hoveredBucket = bucket.key}
							onmouseleave={() => hoveredBucket = null}
							role="img"
							aria-label="{bucket.label}: {formatAggregate(bucket.cost, bucket.tokens, bucket.unpricedTokens)}; {providerBreakdownLabel(bucket.sessions)}"
						>
							{#if hoveredBucket === bucket.key}
								<div class="vchart-tooltip">
									<span class="vchart-tooltip-label">{bucket.label}</span>
									<span class="vchart-tooltip-cost">{formatAggregate(bucket.cost, bucket.tokens, bucket.unpricedTokens)}</span>
									{#if mode === 'usd' && bucket.unpricedTokens > 0}<span class="vchart-tooltip-unpriced">+ {formatTokens(bucket.unpricedTokens)} TOKENS UNPRICED</span>{/if}
								</div>
							{/if}
							<div class="vchart-bar-wrap">
								<div
									class="vchart-bar"
									class:vchart-bar-empty={barValue === 0}
									style="height: {bucketScaleMax > 0 ? (barValue / bucketScaleMax) * 100 : 0}%; --claude-share: {claudeShare}%"
								></div>
							</div>
							<span class="vchart-label">
								{bucket.label}
							</span>
						</div>
					{/each}
				</div>
				{/if}
			</div>

			<!-- ── BY PROJECT ─────────────────────────────────────── -->
			<div class="cost-section" in:flyIn={{ index: 2, duration: 650, stride: 90 }}>
				<div class="sub-header-row">
					<span class="sub-header">BY PROJECT</span>
					<div class="sort-group">
						<button class="option-btn" class:active={sessionSortField === 'date'} onclick={() => toggleSort('date')}>
							DATE {sessionSortField === 'date' ? (sessionSortDir === 'desc' ? '↓' : '↑') : ''}
						</button>
						<button class="option-btn" class:active={sessionSortField === 'cost'} onclick={() => toggleSort('cost')}>
							COST {sessionSortField === 'cost' ? (sessionSortDir === 'desc' ? '↓' : '↑') : ''}
						</button>
						<button class="option-btn" onclick={toggleAllProjects}>
							{allCollapsed ? 'EXPAND ALL' : 'COLLAPSE ALL'}
						</button>
					</div>
				</div>

				{#each filteredProjectCosts as proj, i (proj.project)}
					<div class="project-group" in:flyIn={{ index: i, duration: 650, stride: 90 }}>
						<!-- svelte-ignore a11y_click_events_have_key_events -->
						<!-- svelte-ignore a11y_no_static_element_interactions -->
						<div
							class="group-header"
							onclick={() => toggleProjectCollapse(proj.project)}
							role="button"
							tabindex="0"
							aria-label={collapsedProjects.has(proj.project) ? 'Expand project' : 'Collapse project'}
						>
							<span class="collapse-toggle" aria-hidden="true">{collapsedProjects.has(proj.project) ? '▶' : '▼'}</span>
							<span class="group-name">{proj.projectName.toUpperCase()} <span class="group-count">({proj.displaySessions.length})</span></span>
							<span class="group-cost" class:unpriced={mode === 'usd' && proj.unpricedTokens === proj.totalTokens}>{formatAggregate(proj.totalCost, proj.totalTokens, proj.unpricedTokens)}</span>
						</div>

						{#if !collapsedProjects.has(proj.project)}
							{@const projValue = mode === 'usd' ? proj.totalCost : proj.totalTokens}
							<div
								class="grid-bar-track"
								bind:clientWidth={projectTrackWidth}
								role="img"
								aria-label="{proj.projectName}: {formatAggregate(proj.totalCost, proj.totalTokens, proj.unpricedTokens)}; {providerBreakdownLabel(proj.sessions)}"
							>
								<div class="grid-container" style="grid-template-columns: repeat({projectBarColumns}, 1fr);">
									{#each buildProviderBarBlocks(projectScaleMax > 0 ? (projValue / projectScaleMax) * 100 : 0, projectBarColumns, proj.sessions) as block}
										<div class="rect {block.type}"></div>
									{/each}
								</div>
							</div>

							<!-- svelte-ignore a11y_click_events_have_key_events -->
							<!-- svelte-ignore a11y_no_static_element_interactions -->
							{@const sorted = sortSessions(proj.displaySessions)}
							{#each expandedProjects.has(proj.project) ? sorted : sorted.slice(0, 5) as session ((session.provider ?? 'claudeCode') + '-' + session.sessionId + '-' + session.timestamp)}
								<!-- svelte-ignore a11y_click_events_have_key_events -->
								<!-- svelte-ignore a11y_no_static_element_interactions -->
								<div class="session-detail" class:codex-session={session.provider === 'codex'} onclick={() => handleSessionClick(session)}>
									<ProviderBadge provider={session.provider} surface={session.surface} compact />
									<span class="detail-name" title={session.sessionName || session.sessionId}>{session.sessionName || session.sessionId.slice(0, 8)}</span>
									<span class="detail-session-id" title={session.sessionId}>{session.sessionId.slice(0, 8)}</span>
									<span class="detail-spacer"></span>
									<span class="detail-time">{formatDateTime(session.timestamp)}</span>
									<span class="detail-model" title={sessionModelTitle(session)}>{sessionModelLabel(session)}</span>
									<span class="detail-cost" class:unpriced={mode === 'usd' && session.unpricedTokens === session.totalTokens}>{formatAggregate(session.cost, session.totalTokens, session.unpricedTokens)}</span>
								</div>
							{/each}

							{#if proj.displaySessions.length > 5}
								<!-- svelte-ignore a11y_click_events_have_key_events -->
								<!-- svelte-ignore a11y_no_static_element_interactions -->
								<div class="more-sessions" onclick={() => {
									const next = new Set(expandedProjects);
									if (next.has(proj.project)) {
										next.delete(proj.project);
									} else {
										next.add(proj.project);
									}
									expandedProjects = next;
								}}>
									{expandedProjects.has(proj.project) ? 'Show less' : `${proj.displaySessions.length - 5} more sessions`}
								</div>
							{/if}
						{/if}
					</div>
				{/each}
			</div>
		</div>
	{/if}
{#if showVisualizer && costData}
	<TokenDistanceVisualizer
		totalTokens={costData.totalTokens}
		dateRange={(() => {
			const dates = costData.dailyCosts.map(d => d.date).sort();
			if (dates.length === 0) return '';
			const fmt = (d: string) => {
				const dt = new Date(d + 'T00:00:00');
				return dt.toLocaleDateString('en-US', { month: 'short', day: 'numeric' }).toUpperCase();
			};
			return `${fmt(dates[0])} – ${fmt(dates[dates.length - 1])}`;
		})()}
		onclose={() => showVisualizer = false}
	/>
{/if}
{#if selectedEntry}
	<HistoryCardOverlay entry={selectedEntry} {conversation} onclose={() => { selectedEntry = null; conversation = null; }} />
{/if}
</div>

<style>
	.cost-container {
		display: flex;
		flex-direction: column;
		height: 100%;
		overflow: hidden;
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

	.section-total {
		font-family: var(--font-pixel);
		font-size: 18px;
		font-weight: 500;
		line-height: 1;
		color: var(--text-secondary);
	}

	.section-total.unpriced,
	.model-legend-cost.unpriced,
	.group-cost.unpriced,
	.detail-cost.unpriced {
		color: var(--accent-amber);
		letter-spacing: 0.06em;
	}

	.list-area {
		flex: 1;
		overflow-y: auto;
		padding: var(--space-lg) 0;
		display: flex;
		flex-direction: column;
		gap: var(--space-xl);
	}

	.pricing-note {
		display: flex;
		flex-wrap: wrap;
		align-items: baseline;
		gap: var(--space-md);
		padding: var(--space-sm) var(--space-md);
		border-left: 2px solid var(--accent-amber);
		background: color-mix(in srgb, var(--accent-amber) 7%, transparent);
		font-family: var(--font-mono);
		font-size: 12px;
		line-height: 1.5;
		color: var(--text-secondary);
	}

	.pricing-note-label {
		flex: 0 0 auto;
		font-family: var(--font-pixel);
		font-size: 10px;
		letter-spacing: 0.08em;
		color: var(--accent-amber);
	}

	.provider-usage-legend {
		display: flex;
		align-items: center;
		gap: var(--space-lg);
		font-family: var(--font-mono);
		font-size: 10px;
		letter-spacing: 0.08em;
		color: var(--text-muted);
	}

	.provider-usage-legend-item {
		display: inline-flex;
		align-items: center;
		gap: 6px;
	}

	.provider-usage-swatch {
		width: 10px;
		height: 10px;
		border: 1px solid color-mix(in srgb, currentColor 55%, var(--border-default));
	}

	.provider-usage-swatch.claude {
		color: var(--accent-amber);
		background: var(--accent-amber);
	}

	.provider-usage-swatch.codex {
		color: var(--accent-blue);
		background: var(--accent-blue);
	}

	.cost-section {
		display: flex;
		flex-direction: column;
		gap: var(--space-sm);
	}

	.sub-header {
		font-family: var(--font-pixel);
		font-size: 16px;
		text-transform: uppercase;
		color: var(--text-secondary);
		letter-spacing: 0.1em;
		line-height: 1;
	}

	.sub-header-row {
		display: flex;
		align-items: center;
		justify-content: space-between;
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

	.scale-dropdown {
		position: relative;
	}

	.scale-trigger {
		font-family: var(--font-pixel);
		font-size: 11px;
		letter-spacing: 0.05em;
		padding: 4px var(--space-sm);
		background: transparent;
		border: 1px solid var(--border-default);
		color: var(--text-secondary);
		cursor: pointer;
		text-transform: uppercase;
	}

	.scale-trigger:hover {
		border-color: var(--text-muted);
		color: var(--text-primary);
	}

	.scale-menu {
		position: absolute;
		top: 100%;
		right: 0;
		margin-top: 2px;
		background: var(--bg-card, var(--bg-surface));
		border: 1px solid var(--border-default);
		z-index: 10;
		display: flex;
		flex-direction: column;
	}

	.scale-option {
		font-family: var(--font-pixel);
		font-size: 11px;
		letter-spacing: 0.05em;
		padding: 6px var(--space-md);
		background: transparent;
		border: none;
		color: var(--text-muted);
		cursor: pointer;
		text-align: left;
		white-space: nowrap;
	}

	.scale-option:hover {
		background: rgba(255, 255, 255, 0.1);
		color: var(--text-primary);
	}

	.scale-option.active {
		color: var(--text-primary);
	}

	/* ── Model status bar (StatusBar-style card) ────────────────── */
	.model-status-bar {
		position: relative;
		background: var(--bg-card);
		border: 1px solid var(--border-default);
		padding: var(--space-lg) var(--space-xl);
		display: flex;
		flex-direction: column;
		gap: var(--space-md);
		overflow: hidden;
		transition: border-color var(--transition-fast);
		flex-shrink: 0;
	}

	.model-status-bar:hover {
		border-color: var(--text-muted);
	}

	/* Scanline effect */
	.model-status-bar::after {
		content: '';
		position: absolute;
		top: 0;
		left: 0;
		width: 100%;
		height: 100%;
		background: linear-gradient(
			to bottom,
			transparent 50%,
			rgba(0, 0, 0, 0.1) 51%,
			transparent 52%
		);
		background-size: 100% 4px;
		pointer-events: none;
		z-index: 10;
		opacity: 0.3;
	}

	.progress-track {
		height: 16px;
		background: var(--bg-surface);
		border: 1px solid var(--border-default);
		position: relative;
		overflow: hidden;
		padding: 3px;
	}

	.grid-container {
		display: grid;
		grid-template-rows: 1fr;
		gap: 2px;
		height: 100%;
	}

	.rect {
		width: 100%;
		height: 100%;
		background: rgba(255, 255, 255, 0.05);
		border-radius: 1px;
	}

	.rect.filled { background-color: var(--model-color); box-shadow: 0 0 4px color-mix(in srgb, var(--model-color) 30%, transparent); }

	.model-legend {
		display: flex;
		flex-wrap: wrap;
		gap: var(--space-xl);
	}

	.model-legend-item {
		display: flex;
		align-items: center;
		gap: var(--space-sm);
	}

	.model-legend-item .dot {
		width: 8px;
		height: 8px;
		background: var(--model-color);
	}

	.model-legend-label {
		font-family: var(--font-mono);
		font-size: 14px;
		color: var(--text-secondary);
		letter-spacing: 0.1em;
	}

	.model-legend-cost {
		font-family: var(--font-pixel);
		font-size: 16px;
		color: var(--text-primary);
	}

	.model-legend-pct {
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--text-muted);
	}

	.deco-mesh {
		position: absolute;
		top: 0;
		right: 0;
		width: 100px;
		height: 100%;
		pointer-events: none;
		opacity: 0.05;
		background-image:
			radial-gradient(var(--text-muted) 1px, transparent 1px);
		background-size: 4px 4px;
	}

	/* ── Grid bar track (for project inline bars) ────────────────── */
	.grid-bar-track {
		flex: 1;
		height: 10px;
		background: var(--bg-surface);
		border: 1px solid var(--border-default);
		overflow: hidden;
		padding: 1px;
	}

	/* ── Vertical bar chart ──────────────────────────────────────── */
	.vchart-area {
		display: flex;
		align-items: flex-end;
		gap: 8px;
		height: 180px;
		padding: var(--space-sm) 0;
	}

	.chart-unpriced-state {
		display: grid;
		place-items: center;
		min-height: 120px;
		border: 1px dashed color-mix(in srgb, var(--accent-amber) 45%, var(--border-default));
		font-family: var(--font-mono);
		font-size: 11px;
		letter-spacing: 0.06em;
		color: var(--accent-amber);
		text-align: center;
		padding: var(--space-lg);
	}

	.vchart-col {
		flex: 1;
		display: flex;
		flex-direction: column;
		align-items: center;
		height: 100%;
		position: relative;
	}

	.vchart-bar-wrap {
		flex: 1;
		width: 100%;
		display: flex;
		align-items: flex-end;
		justify-content: center;
	}

	.vchart-bar {
		width: 100%;
		min-height: 2px;
		background:
			repeating-linear-gradient(
				0deg,
				transparent,
				transparent 3px,
				rgba(0, 0, 0, 0.2) 3px,
				rgba(0, 0, 0, 0.2) 4px
			),
			linear-gradient(
				to top,
				var(--accent-amber) 0 var(--claude-share),
				var(--accent-blue) var(--claude-share) 100%
			);
		box-shadow: 0 0 4px color-mix(in srgb, var(--text-secondary) 30%, transparent);
		transition: height 300ms ease;
	}

	.vchart-col:hover .vchart-bar {
		box-shadow: 0 0 8px color-mix(in srgb, var(--text-secondary) 50%, transparent);
	}

	.vchart-bar-empty {
		background: var(--border-default);
		background-image: none;
		box-shadow: none;
		opacity: 0.4;
		min-height: 2px;
	}

	.vchart-label {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--text-primary);
		margin-top: 4px;
		white-space: nowrap;
		text-align: center;
	}


	.vchart-tooltip {
		position: absolute;
		bottom: 100%;
		left: 50%;
		transform: translateX(-50%);
		background: var(--bg-card, var(--bg-surface));
		border: 1px solid var(--border-default);
		padding: 4px var(--space-sm);
		white-space: nowrap;
		z-index: 10;
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 2px;
		pointer-events: none;
	}

	.vchart-tooltip-label {
		font-family: var(--font-pixel);
		font-size: 9px;
		color: var(--text-muted);
	}

	.vchart-tooltip-unpriced {
		font-family: var(--font-mono);
		font-size: 9px;
		color: var(--accent-amber);
		letter-spacing: 0.04em;
	}

	.vchart-tooltip-cost {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--text-primary);
	}

	.session-detail {
		display: grid;
		grid-template-columns: auto minmax(100px, 300px) auto minmax(0, 1fr) auto auto minmax(60px, max-content);
		gap: var(--space-md);
		align-items: center;
		padding: var(--space-xs) var(--space-sm);
		font-family: var(--font-mono);
		font-size: 13px;
		cursor: pointer;
	}

	.session-detail.codex-session {
		box-shadow: inset 2px 0 0 color-mix(in srgb, var(--accent-blue) 70%, transparent);
	}

	.session-detail:hover {
		background: var(--bg-elevated);
	}

	.detail-name {
		color: var(--text-primary);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.detail-session-id {
		color: var(--text-muted);
		font-size: 11px;
		white-space: nowrap;
	}

	.detail-time {
		color: var(--text-muted);
		white-space: nowrap;
	}

	.detail-model {
		color: var(--text-muted);
		min-width: 50px;
		white-space: nowrap;
	}

	.detail-cost {
		color: var(--text-secondary);
		min-width: 50px;
		text-align: right;
		white-space: nowrap;
	}

	.session-detail.codex-session .detail-cost {
		color: var(--accent-blue);
	}

	/* ── Project groups ───────────────────────────────────────────── */
	.project-group {
		display: flex;
		flex-direction: column;
		gap: var(--space-xs);
		margin-bottom: var(--space-md);
	}

	.group-header {
		display: flex;
		align-items: center;
		gap: var(--space-md);
		padding-bottom: var(--space-sm);
		border-bottom: 1px solid var(--border-default);
		margin-bottom: var(--space-sm);
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

	.group-name {
		font-family: var(--font-pixel);
		font-size: 16px;
		color: var(--text-primary);
		letter-spacing: 0.1em;
		flex: 1;
	}

	.group-count {
		font-family: var(--font-mono);
		font-size: 14px;
		color: var(--text-muted);
		letter-spacing: normal;
	}

	.group-cost {
		font-family: var(--font-mono);
		font-size: 13px;
		color: var(--text-secondary);
		flex-shrink: 0;
	}

	.more-sessions {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--text-muted);
		padding: var(--space-xs) var(--space-sm);
		cursor: pointer;
	}

	.more-sessions:hover {
		color: var(--text-secondary);
	}

	/* ── Sort/toggle buttons ──────────────────────────────────────── */
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

	.option-btn:hover {
		color: var(--text-primary);
		background: rgba(255, 255, 255, 0.1);
	}

	.option-btn.active {
		color: var(--accent-amber);
	}

	.mode-toggle {
		display: flex;
		border: 1px solid var(--border-default);
	}

	.mode-btn {
		font-family: var(--font-pixel);
		font-size: 10px;
		letter-spacing: 0.1em;
		padding: 4px var(--space-sm);
		background: transparent;
		border: none;
		color: var(--text-muted);
		cursor: pointer;
		text-transform: uppercase;
	}

	.mode-btn:hover {
		color: var(--text-primary);
		background: rgba(255, 255, 255, 0.08);
	}

	.mode-btn.active {
		color: var(--accent-amber);
	}

	.distance-btn {
		font-family: var(--font-pixel);
		font-size: 11px;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		color: var(--accent-amber);
		background: none;
		border: 1px solid var(--accent-amber);
		padding: 2px 8px;
		cursor: pointer;
		transition: background 0.15s, color 0.15s;
		margin-left: auto;
	}

	.distance-btn:hover {
		background: var(--accent-amber);
		color: var(--bg-base);
	}

</style>
