<script lang="ts">
	import type { Session } from '$lib/types';
	import { SessionStatus } from '$lib/types';
	import { sessionCostMap, costMode } from '$lib/stores/cost';
	import { formatCostOrTokens } from '$lib/cost-utils';
	import { isCostAvailable } from '$lib/cost-semantics';
	import { awayActivity } from '$lib/stores/away';
	import { awayActivityLabel, awayActivityShortLabel } from '$lib/session-utils';
	import { getSessionStatusShortLabel } from '$lib/status-utils';
	import { formatTimeSince } from '$lib/time-utils';
	import {
		canSessionAction,
		providerLabel,
		providerOf,
		providerShortLabel,
		sessionKeyOf,
		sessionReturnKind
	} from '$lib/provider';

	interface Props {
		session: Session;
		/** One line per session: no preview line. */
		compact?: boolean;
		onexpand?: () => void;
		onstop?: () => void;
		onopen?: () => void;
	}

	let { session, compact = false, onexpand, onstop, onopen }: Props = $props();

	let key = $derived(sessionKeyOf(session));
	let provider = $derived(providerOf(session));
	let tone = $derived(
		session.status === SessionStatus.NeedsAttention ? 'attention'
		: session.status === SessionStatus.WaitingForInput ? 'idle'
		: session.status === SessionStatus.Connecting ? 'connecting'
		: 'working'
	);
	let statusWord = $derived(
		getSessionStatusShortLabel(session.status, session.waitingFor, session.pendingToolName)
	);
	let title = $derived(
		session.customTitle
		|| session.officialName
		|| session.codexTitle
		|| session.cursorTitle
		|| session.aiTitle
		|| session.summary
		|| session.firstPrompt
		|| session.sessionName
	);
	// The tool waiting for approval leads the preview line, in the status color.
	let pendingTool = $derived(
		session.waitingFor?.kind === 'approval'
			? session.waitingFor.tool
			: !session.waitingFor && statusWord === 'APPROVE'
				? session.pendingToolName
				: null
	);
	let preview = $derived(
		session.waitingFor
			? session.lastAssistantMessage || session.latestMessage || session.firstPrompt
			: session.latestMessage || session.firstPrompt
	);
	let away = $derived(session.waitingFor ? $awayActivity[key] : undefined);
	let awayText = $derived(awayActivityShortLabel(away));

	let health = $derived.by(() => {
		switch (session.sourceHealth) {
			case 'stale': return { label: 'STALE', title: 'Source observation may be out of date' };
			case 'partial': return { label: 'PARTIAL', title: 'Source observation is incomplete' };
			case 'unavailable': return { label: 'NO SRC', title: 'Source observation is unavailable' };
			case 'unknown': return { label: '?', title: 'Source health is unknown' };
			default: return null;
		}
	});

	let costRecord = $derived($sessionCostMap.get(key));
	// "UNPRICED" is noise at this density: show nothing instead.
	let costLabel = $derived(
		costRecord && ($costMode !== 'usd' || isCostAvailable(costRecord))
			? formatCostOrTokens(costRecord.cost, costRecord.totalTokens, $costMode, true)
			: null
	);
	let startedAtIso = $derived(
		session.startedAtMs != null ? new Date(session.startedAtMs).toISOString() : session.modified
	);

	let canExpand = $derived(canSessionAction(session, 'conversation'));
	let canOpen = $derived(canSessionAction(session, 'open'));
	let canStop = $derived(canSessionAction(session, 'stop'));
	let openTitle = $derived(
		sessionReturnKind(session) === 'native' ? 'Focus exact terminal'
		: session.openTarget === 'project' ? 'Open project only — select the session terminal manually'
		: 'Activate application — exact session focus unavailable'
	);

	function handleClick(e: MouseEvent) {
		if ((e.target as HTMLElement).closest('.act')) return;
		if (canExpand) onexpand?.();
	}

	function handleKeydown(e: KeyboardEvent) {
		if (e.target !== e.currentTarget) return;
		if (e.key === 'Enter' || e.key === ' ') {
			e.preventDefault();
			if (canExpand) onexpand?.();
		}
	}
</script>

<div
	class="row {tone}"
	class:compact
	class:expandable={canExpand}
	onclick={handleClick}
	onkeydown={handleKeydown}
	role="button"
	tabindex="0"
	aria-label={`${title} — ${statusWord.toLowerCase()}`}
>
	<span class="rail" aria-hidden="true"></span>

	<div class="body">
		<div class="line">
			<span class="chip">{statusWord}</span>
			<span class="title" title={`${title}\n${session.id}`}>{title}</span>
			{#if session.gitBranch}
				<span class="branch" title={`Branch: ${session.gitBranch}`}>{session.gitBranch}</span>
			{/if}
			<span class="meta">
				{#if health}
					<span class="health" title={health.title}>{health.label}</span>
				{/if}
				<span class="prov" title={providerLabel(provider)}>{providerShortLabel(provider)}</span>
				<span class="num msgs" title={`${session.messageCount} messages`}>
					<svg width="9" height="9" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" aria-hidden="true">
						<path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z" />
					</svg>{session.messageCount}
				</span>
				{#if costLabel}
					<span class="num cost" title={$costMode === 'usd' ? 'Session cost' : 'Session tokens'}>{costLabel}</span>
				{/if}
				<span class="num age" title={`Started ${new Date(startedAtIso).toLocaleString()}`}>{formatTimeSince(startedAtIso)}</span>
			</span>
		</div>

		{#if !compact}
			<div class="line sub">
				{#if pendingTool}
					<span class="need">{pendingTool}</span>
				{/if}
				<span class="preview" title={preview}>{preview || '—'}</span>
				{#if awayText}
					<span class="away" title={`While you were away: ${awayActivityLabel(away)}`}>{awayText}</span>
				{/if}
			</div>
		{/if}
	</div>

	<div class="actions">
		{#if canStop}
			<button type="button" class="act stop" onclick={() => onstop?.()} title="Stop" aria-label="Stop session">
				<svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" aria-hidden="true">
					<rect x="6" y="6" width="12" height="12" rx="1" />
				</svg>
			</button>
		{/if}
		{#if canOpen}
			<button type="button" class="act open" onclick={() => onopen?.()} title={openTitle} aria-label={openTitle}>
				<svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" aria-hidden="true">
					<path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6" />
					<polyline points="15 3 21 3 21 9" />
					<line x1="10" y1="14" x2="21" y2="3" />
				</svg>
			</button>
		{/if}
	</div>
</div>

<style>
	/* Sizes are in em of the tile's font size (10px × the A−/A+ scale), so one
	   variable scales the whole row. */
	.row {
		--tone: var(--status-working);
		position: relative;
		display: flex;
		align-items: center;
		gap: 6px;
		min-height: 4em;
		padding: 4px 4px 4px 9px;
		cursor: default;
		outline: none;
		transition: background-color var(--transition-fast);
	}

	.row.compact { min-height: 2.4em; padding-top: 2px; padding-bottom: 2px; }
	.row.expandable { cursor: pointer; }
	.row:hover { background: var(--bg-card-hover); }
	.row:focus-visible { box-shadow: inset 0 0 0 1px var(--border-focus); }

	.row.attention { --tone: var(--status-permission); background: color-mix(in srgb, var(--status-permission) 7%, transparent); }
	.row.attention:hover { background: color-mix(in srgb, var(--status-permission) 12%, var(--bg-card-hover)); }
	.row.idle { --tone: var(--status-input); }
	.row.connecting { --tone: var(--status-connecting); }

	/* Status rail: the colour you scan for down a tile. */
	.rail {
		position: absolute;
		left: 0;
		top: 0;
		bottom: 0;
		width: 3px;
		background: var(--tone);
		overflow: hidden;
	}

	/* A working session's rail carries a slow light, so a glance tells live
	   work from a stalled board. Transform-only: composited, no repaint. */
	.row.working .rail::after {
		content: '';
		position: absolute;
		left: 0;
		right: 0;
		top: 0;
		height: 60%;
		background: linear-gradient(to bottom, transparent, rgba(255, 255, 255, 0.7), transparent);
		animation: rail-scan 1.8s linear infinite;
	}

	@keyframes rail-scan {
		from { transform: translateY(-100%); }
		to { transform: translateY(170%); }
	}

	@media (prefers-reduced-motion: reduce) {
		.row.working .rail::after { animation: none; display: none; }
	}

	.body {
		flex: 1;
		min-width: 0;
		display: flex;
		flex-direction: column;
		gap: 2px;
	}

	.line {
		display: flex;
		align-items: center;
		gap: 6px;
		min-width: 0;
		line-height: 1.6em;
	}

	.chip {
		flex-shrink: 0;
		min-width: 8.6ch;
		height: 1.56em;
		padding: 0 4px;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		font-family: var(--font-mono);
		font-size: 0.9em;
		font-weight: 700;
		letter-spacing: 0.06em;
		color: var(--tone);
		background: color-mix(in srgb, var(--tone) 14%, transparent);
		border: 1px solid color-mix(in srgb, var(--tone) 45%, transparent);
	}

	/* Attention is the one state that shouts. */
	.row.attention .chip {
		color: #000;
		background: var(--tone);
		border-color: var(--tone);
	}

	/* Purple on black is too dark for small type: lift it toward white. */
	.row.working .chip { color: color-mix(in srgb, var(--tone) 45%, #fff); }
	.row.connecting .chip { color: var(--text-secondary); }

	.title {
		min-width: 0;
		flex: 0 1 auto;
		overflow: hidden;
		white-space: nowrap;
		text-overflow: ellipsis;
		font-family: var(--font-sans);
		font-size: 1.25em;
		font-weight: 600;
		color: var(--text-primary);
		letter-spacing: 0.01em;
	}

	.branch {
		flex: 0 10 auto;
		min-width: 0;
		max-width: 16ch;
		overflow: hidden;
		white-space: nowrap;
		text-overflow: ellipsis;
		font-family: var(--font-mono);
		font-size: 1em;
		color: var(--text-muted);
	}

	.branch::before { content: '⎇ '; }

	.meta {
		margin-left: auto;
		flex-shrink: 0;
		display: inline-flex;
		align-items: center;
		gap: 7px;
		padding-left: 4px;
	}

	.num,
	.health {
		display: inline-flex;
		align-items: center;
		gap: 3px;
		font-family: var(--font-mono);
		font-size: 1em;
		color: var(--text-muted);
		white-space: nowrap;
		font-variant-numeric: tabular-nums;
	}

	.msgs svg { width: 0.9em; height: 0.9em; }

	.health {
		font-size: 0.8em;
		letter-spacing: 0.06em;
		padding: 0 3px;
		border: 1px dashed var(--border-default);
	}

	/* Grey on purpose: provider colours would collide with status colours. */
	.prov {
		font-family: var(--font-mono);
		font-size: 0.8em;
		font-weight: 700;
		letter-spacing: 0.08em;
		color: var(--text-secondary);
	}

	.sub { gap: 6px; line-height: 1.5em; }

	.need {
		flex-shrink: 0;
		font-family: var(--font-mono);
		font-size: 1.05em;
		font-weight: 600;
		color: var(--tone);
	}

	.preview {
		flex: 1;
		min-width: 0;
		overflow: hidden;
		white-space: nowrap;
		text-overflow: ellipsis;
		font-size: 1.15em;
		color: var(--text-secondary);
	}

	.away {
		flex: 0 1 auto;
		max-width: 40%;
		overflow: hidden;
		white-space: nowrap;
		text-overflow: ellipsis;
		font-family: var(--font-mono);
		font-size: 1em;
		color: var(--text-muted);
	}

	.actions {
		flex-shrink: 0;
		display: flex;
		align-items: center;
		gap: 2px;
	}

	.act {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 22px;
		height: 22px;
		padding: 0;
		background: transparent;
		border: 1px solid transparent;
		color: var(--text-muted);
		cursor: pointer;
		transition: color var(--transition-fast), border-color var(--transition-fast), background-color var(--transition-fast);
	}

	.act:hover { color: var(--text-primary); border-color: var(--border-default); background: var(--bg-base); }
	.act.stop:hover { color: var(--status-permission); border-color: var(--status-permission); }

	/* Stop is destructive and rare: only offered where you can hover. */
	.act.stop { display: none; }

	@media (hover: hover) {
		.row:hover .act.stop,
		.row:focus-within .act.stop { display: inline-flex; }
		.row:hover .act.open { color: var(--text-primary); }
	}

	/* Narrow tiles drop the least useful bits first. In em, so bigger type
	   gives up detail sooner instead of squeezing the title. */
	@container tile (max-width: 56em) {
		.branch { display: none; }
	}

	@container tile (max-width: 42em) {
		.away { display: none; }
	}

	@container tile (max-width: 38em) {
		.cost, .msgs { display: none; }
	}

	/* Touch screens: a little more type; targets follow the global 36px. */
	@media (max-width: 768px) {
		.row { min-height: max(44px, 4em); }
		.row.compact { min-height: max(36px, 2.4em); padding-top: 0; padding-bottom: 0; }
		.title { font-size: 1.3em; }
		.preview { font-size: 1.2em; }
		.act { width: 36px; height: 36px; }
	}
</style>
