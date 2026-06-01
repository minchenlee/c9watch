<script lang="ts">
	import { untrack } from 'svelte';
	import { slide } from 'svelte/transition';
	import { cubicOut } from 'svelte/easing';
	import {
		workflows,
		workflowsLoading,
		workflowsError,
		refreshWorkflows,
		startWorkflowPolling,
		stopWorkflowPolling,
	} from '$lib/stores/workflows';
	import { getWorkflowDetail } from '$lib/api';
	import type { WorkflowSummary, WorkflowAgent, WorkflowDetail } from '$lib/types';

	// ── State ────────────────────────────────────────────────────────
	let selectedRunId = $state<string | null>(null);
	let detail = $state<WorkflowDetail | null>(null);
	let detailLoading = $state(false);
	let expandedAgent = $state<string | null>(null);
	let agentsOpen = $state(true);
	let scriptOpen = $state(false);
	let resultOpen = $state(false);

	// Selected summary from the list — keeps the header rendering instantly.
	let selectedSummary = $derived(
		selectedRunId ? ($workflows.find((w) => w.runId === selectedRunId) ?? null) : null
	);

	// ── Lifecycle ────────────────────────────────────────────────────
	$effect(() => {
		startWorkflowPolling();
		refreshWorkflows();
		return () => stopWorkflowPolling();
	});

	// Fetch detail when a run is selected, and refetch while it's running
	// whenever the matching summary changes (durationMs/totalTokens/status).
	$effect(() => {
		const s = selectedSummary;
		if (!s) {
			detail = null;
			return;
		}
		// Track the live-changing fields so a running run refetches on each update.
		// (Do NOT read `detail` here — that would make the effect re-run every time
		// the fetch resolves and sets `detail`, causing an infinite refetch loop.)
		void s.durationMs;
		void s.totalTokens;
		void s.status;

		const runId = s.runId;
		const alreadyShowing = untrack(() => detail?.runId === runId);
		detailLoading = !alreadyShowing;
		let cancelled = false;
		getWorkflowDetail(runId)
			.then((d) => {
				if (!cancelled) detail = d;
			})
			.catch(() => {})
			.finally(() => {
				if (!cancelled) detailLoading = false;
			});
		return () => {
			cancelled = true;
		};
	});

	// ── Navigation ───────────────────────────────────────────────────
	function selectRun(runId: string) {
		selectedRunId = runId;
		detail = null;
		expandedAgent = null;
		scriptOpen = false;
		resultOpen = false;
	}

	function back() {
		selectedRunId = null;
		detail = null;
	}

	function toggleAgent(key: string) {
		expandedAgent = expandedAgent === key ? null : key;
	}

	// ── Grouping ─────────────────────────────────────────────────────
	interface PhaseGroup {
		title: string;
		agents: WorkflowAgent[];
	}

	let phaseGroups = $derived.by((): PhaseGroup[] => {
		if (!detail) return [];
		const groups: PhaseGroup[] = detail.phases.map((title) => ({ title, agents: [] }));
		const byTitle = new Map(groups.map((g) => [g.title, g]));
		const other: PhaseGroup = { title: 'Other', agents: [] };
		for (const agent of detail.agents) {
			const g = byTitle.get(agent.phaseTitle);
			if (g) g.agents.push(agent);
			else other.agents.push(agent);
		}
		const result = groups.filter((g) => g.agents.length > 0);
		if (other.agents.length > 0) result.push(other);
		return result;
	});

	// ── Formatters ───────────────────────────────────────────────────
	function formatRelativeTime(epochMs: number): string {
		const diff = Date.now() - epochMs;
		if (diff < 60_000) return 'just now';
		const mins = Math.floor(diff / 60_000);
		if (mins < 60) return `${mins}m ago`;
		const hours = Math.floor(mins / 60);
		if (hours < 24) return `${hours}h ago`;
		const days = Math.floor(hours / 24);
		return `${days}d ago`;
	}

	function formatTokens(n: number): string {
		if (n >= 1_000_000) {
			const v = n / 1_000_000;
			return `${v >= 10 ? Math.round(v) : v.toFixed(1)}M`;
		}
		if (n >= 1_000) {
			return `${Math.round(n / 1_000)}k`;
		}
		return String(n);
	}

	function formatDuration(ms: number): string {
		if (ms < 1_000) return `${ms}ms`;
		const totalSec = Math.floor(ms / 1_000);
		if (totalSec < 60) {
			const s = ms / 1_000;
			return `${s.toFixed(1)}s`;
		}
		const totalMin = Math.floor(totalSec / 60);
		if (totalMin < 60) {
			return `${totalMin}m ${totalSec % 60}s`;
		}
		const hours = Math.floor(totalMin / 60);
		return `${hours}h ${totalMin % 60}m`;
	}

	// Prettify a preview string: pretty-print the first JSON object/array found
	// in it (2-space indent); otherwise return the text trimmed. Agent
	// resultPreviews are often raw single-line JSON, sometimes with a prefix,
	// suffix, or — most commonly — TRUNCATED mid-value (no closing brace). We
	// slice from the first {/[ and repair truncation by closing open
	// brackets/strings before parsing, so even a cut-off preview prettifies.
	function formatPreview(text: string): string {
		const t = text.trim();
		const start = firstBracket(t);
		if (start === -1) return t;
		const repaired = repairJson(t.slice(start));
		if (repaired !== null) {
			try {
				return JSON.stringify(JSON.parse(repaired), null, 2);
			} catch {
				// still unparseable — fall through to raw
			}
		}
		return t;
	}

	// Index of the first { or [ in the string, or -1.
	function firstBracket(t: string): number {
		const o = t.indexOf('{');
		const a = t.indexOf('[');
		if (o === -1) return a;
		if (a === -1) return o;
		return Math.min(o, a);
	}

	// Walk a JSON-ish string from its opening bracket, tracking string/escape
	// state and a stack of open {/[. Stop at the position where the top-level
	// value closes (balanced). If the string ends while still open (truncated),
	// trim any trailing partial token and append the missing closers. Returns a
	// parseable string, or null if nothing usable.
	function repairJson(s: string): string | null {
		const stack: string[] = [];
		let inStr = false;
		let esc = false;
		let end = -1;
		for (let i = 0; i < s.length; i++) {
			const c = s[i];
			if (inStr) {
				if (esc) esc = false;
				else if (c === '\\') esc = true;
				else if (c === '"') inStr = false;
				continue;
			}
			if (c === '"') inStr = true;
			else if (c === '{' || c === '[') stack.push(c === '{' ? '}' : ']');
			else if (c === '}' || c === ']') {
				stack.pop();
				if (stack.length === 0) {
					end = i;
					break;
				}
			}
		}
		// Balanced top-level value found — return exactly that span.
		if (end !== -1) return s.slice(0, end + 1);
		if (stack.length === 0) return null;

		// Truncated: build a repaired tail. Close an open string, drop a dangling
		// partial token (after the last comma/colon/bracket), then add closers.
		let body = s;
		if (inStr) body += '"';
		body = dropDanglingTail(body);
		while (stack.length) body += stack.pop();
		return body;
	}

	// After truncation repair, the text may end on a partial key/value with no
	// value yet (e.g. `…,"foo"` or `…,"foo":`). Trim back to the last point that
	// leaves valid JSON: cut a trailing comma, or a `"key":` with no value.
	function dropDanglingTail(s: string): string {
		let t = s.replace(/,\s*$/, '');
		// `"key":` dangling with no value → drop the whole key:value start.
		t = t.replace(/,?\s*"(?:[^"\\]|\\.)*"\s*:\s*$/, '');
		// trailing comma left by the above
		t = t.replace(/,\s*$/, '');
		return t;
	}

	// ── Raw/Parsed toggle ────────────────────────────────────────────
	// Tracks which code boxes the user flipped to raw. Default is parsed.
	let rawBoxes = $state<Set<string>>(new Set());

	function isRaw(id: string): boolean {
		return rawBoxes.has(id);
	}

	function toggleRaw(id: string) {
		const next = new Set(rawBoxes);
		if (next.has(id)) next.delete(id);
		else next.add(id);
		rawBoxes = next;
	}
</script>

<div class="workflow-viewer">
	{#if !selectedSummary}
		<!-- ── LIST ─────────────────────────────────────────────────── -->
		<div class="section-header">
			<span class="section-title">Workflows</span>
			<span class="section-count">{$workflows.length}</span>
		</div>

		{#if $workflowsLoading && $workflows.length === 0}
			<div class="state-msg">Loading workflows…</div>
		{:else if $workflowsError}
			<div class="state-msg">{$workflowsError}</div>
		{:else if $workflows.length === 0}
			<div class="state-msg">No workflows yet</div>
		{:else}
			<div class="list-area">
				{#each $workflows as wf (wf.runId)}
					<button
						class="run-row"
						class:running={wf.status === 'running'}
						onclick={() => selectRun(wf.runId)}
					>
						<div class="run-main">
							<div class="run-name-row">
								{#if wf.status === 'running'}
									<span class="live-dot" aria-hidden="true"></span>
								{/if}
								<span class="run-name">{wf.workflowName}</span>
								<span class="pill {wf.status}">{wf.status}</span>
							</div>
							<span class="run-project">{wf.projectName}</span>
						</div>
						<div class="run-meta">
							<span class="run-time">{formatRelativeTime(wf.startTime)}</span>
							<span class="run-stats">
								{wf.agentCount} agents
								<span class="dot-sep">·</span>
								{formatTokens(wf.totalTokens)}
								<span class="dot-sep">·</span>
								{#if wf.status === 'running'}
									<span class="live-text">live</span>
								{:else}
									{formatDuration(wf.durationMs)}
								{/if}
							</span>
						</div>
					</button>
				{/each}
			</div>
		{/if}
	{:else}
		<!-- ── DETAIL ───────────────────────────────────────────────── -->
		<div class="section-header detail-header-bar">
			<button class="back-btn" onclick={back}>← Workflows</button>
			{#if detailLoading}
				<span class="inline-loading">loading detail…</span>
			{/if}
		</div>

		<div class="detail-area">
			<div class="detail-head">
				<div class="detail-title-row">
					<span class="detail-name">{selectedSummary.workflowName}</span>
					<span class="pill {selectedSummary.status}">{selectedSummary.status}</span>
				</div>
				{#if selectedSummary.summary}
					<p class="detail-summary">{selectedSummary.summary}</p>
				{/if}
				<div class="detail-path">{selectedSummary.projectPath}</div>
				<div class="detail-stats">
					<span class="stat-chip">{selectedSummary.agentCount} agents</span>
					<span class="stat-chip">{formatTokens(selectedSummary.totalTokens)} tokens</span>
					<span class="stat-chip">{selectedSummary.totalToolCalls} tools</span>
					<span class="stat-chip">
						{selectedSummary.status === 'running'
							? 'live'
							: formatDuration(selectedSummary.durationMs)}
					</span>
					<span class="stat-chip model">{selectedSummary.defaultModel}</span>
				</div>
			</div>

			{#if phaseGroups.length === 0}
				<div class="state-msg">
					{detailLoading ? 'Loading detail…' : 'No agents recorded'}
				</div>
			{:else}
				<!-- ── AGENTS (peer panel to SCRIPT / RESULT) ──────── -->
				<div class="wf-panel">
					<button
						class="panel-head"
						class:open={agentsOpen}
						onclick={() => (agentsOpen = !agentsOpen)}
					>
						<span class="panel-title">Agents</span>
						<span class="panel-count">{detail ? detail.agents.length : ''}</span>
						<span class="panel-chevron" aria-hidden="true">{agentsOpen ? '▾' : '▸'}</span>
					</button>
					{#if agentsOpen}
						<div
							class="panel-inner"
							transition:slide|local={{ duration: 200, easing: cubicOut }}
						>
							{#each phaseGroups as group (group.title)}
								<div class="phase-group">
									<div class="sub-header">{group.title}</div>
									{#each group.agents as agent, ai (group.title + '-' + agent.label + '-' + ai)}
										{@const key = group.title + '-' + agent.label + '-' + ai}
										<div class="agent-card" class:expanded={expandedAgent === key}>
											<button class="agent-head" onclick={() => toggleAgent(key)}>
												<span class="agent-label">{agent.label}</span>
												<span class="pill agent {agent.state}">{agent.state}</span>
												<span class="agent-spacer"></span>
												<span class="agent-meta">{agent.model}</span>
												<span class="agent-meta">{formatTokens(agent.tokens)}</span>
												<span class="agent-meta">{agent.toolCalls} tools</span>
												<span class="agent-meta">{formatDuration(agent.durationMs)}</span>
												{#if agent.lastToolName}
													<span class="agent-tool">{agent.lastToolName}</span>
												{/if}
												<span class="agent-chevron" aria-hidden="true"
													>{expandedAgent === key ? '▾' : '▸'}</span
												>
											</button>
											{#if expandedAgent === key}
												<div
													class="agent-body"
													transition:slide|local={{ duration: 200, easing: cubicOut }}
												>
													{#if agent.promptPreview}
														<div class="preview-label">Prompt</div>
														<pre class="preview-box">{agent.promptPreview}</pre>
													{/if}
													{#if agent.resultPreview}
														{@const rid = key + ':result'}
														<div class="preview-label">
															Result
															<span class="raw-toggle">
																<button
																	class="raw-btn"
																	class:active={!isRaw(rid)}
																	onclick={() => toggleRaw(rid)}>Parsed</button
																>
																<button
																	class="raw-btn"
																	class:active={isRaw(rid)}
																	onclick={() => toggleRaw(rid)}>Raw</button
																>
															</span>
														</div>
														<pre class="preview-box">{isRaw(rid)
																? agent.resultPreview
																: formatPreview(agent.resultPreview)}</pre>
													{/if}
													{#if !agent.promptPreview && !agent.resultPreview}
														<div class="preview-empty">No preview available</div>
													{/if}
												</div>
											{/if}
										</div>
									{/each}
								</div>
							{/each}
						</div>
					{/if}
				</div>

				{#if detail}
					<!-- ── SCRIPT ─────────────────────────────────────── -->
					<div class="wf-panel">
						<button
							class="panel-head"
							class:open={scriptOpen}
							onclick={() => (scriptOpen = !scriptOpen)}
						>
							<span class="panel-title">Script</span>
							<span class="panel-chevron" aria-hidden="true">{scriptOpen ? '▾' : '▸'}</span>
						</button>
						{#if scriptOpen}
							<div transition:slide|local={{ duration: 200, easing: cubicOut }}>
								<pre class="code-box">{detail.script}</pre>
							</div>
						{/if}
					</div>

					<!-- ── RESULT ─────────────────────────────────────── -->
					{#if detail.resultJson}
						<div class="wf-panel">
							<button
								class="panel-head"
								class:open={resultOpen}
								onclick={() => (resultOpen = !resultOpen)}
							>
								<span class="panel-title">Result</span>
								<span class="panel-chevron" aria-hidden="true">{resultOpen ? '▾' : '▸'}</span>
							</button>
							{#if resultOpen}
								<div transition:slide|local={{ duration: 200, easing: cubicOut }}>
									<div class="code-toolbar">
										<button
											class="raw-btn"
											class:active={!isRaw('result-panel')}
											onclick={() => toggleRaw('result-panel')}>Parsed</button
										>
										<button
											class="raw-btn"
											class:active={isRaw('result-panel')}
											onclick={() => toggleRaw('result-panel')}>Raw</button
										>
									</div>
									<pre class="code-box">{isRaw('result-panel')
											? detail.resultJson
											: formatPreview(detail.resultJson)}</pre>
								</div>
							{/if}
						</div>
					{/if}
				{/if}
			{/if}
		</div>
	{/if}
</div>

<style>
	.workflow-viewer {
		display: flex;
		flex-direction: column;
		height: 100%;
		overflow: hidden;
	}

	/* ── Section header (matches HISTORY / COST tabs) ────────────── */
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

	/* ── Loading / empty / error states ──────────────────────────── */
	.state-msg {
		font-family: var(--font-mono);
		font-size: 13px;
		color: var(--text-muted);
		text-transform: uppercase;
		letter-spacing: 0.05em;
		padding: var(--space-xl) 0;
		text-align: center;
	}

	/* ── List ────────────────────────────────────────────────────── */
	.list-area {
		flex: 1;
		overflow-y: auto;
		display: flex;
		flex-direction: column;
	}

	.run-row {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--space-md);
		width: 100%;
		padding: var(--space-md);
		background: none;
		border: none;
		border-bottom: 1px solid var(--border-default);
		cursor: pointer;
		text-align: left;
		transition: all 0.15s ease;
	}

	.run-row:hover {
		background: var(--bg-elevated);
	}

	.run-main {
		display: flex;
		flex-direction: column;
		gap: 2px;
		min-width: 0;
	}

	.run-name-row {
		display: flex;
		align-items: center;
		gap: var(--space-sm);
	}

	.run-name {
		font-family: var(--font-mono);
		font-size: 14px;
		color: var(--text-primary);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.run-project {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--text-muted);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.run-meta {
		display: flex;
		flex-direction: column;
		align-items: flex-end;
		gap: 2px;
		flex-shrink: 0;
	}

	.run-time {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--text-muted);
	}

	.run-stats {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--text-secondary);
		white-space: nowrap;
	}

	.dot-sep {
		color: var(--text-muted);
		margin: 0 2px;
	}

	.live-text {
		color: var(--accent-amber);
		text-transform: uppercase;
		letter-spacing: 0.05em;
	}

	.live-dot {
		width: 7px;
		height: 7px;
		border-radius: 50%;
		background: var(--accent-amber);
		box-shadow: 0 0 6px var(--accent-amber);
		flex-shrink: 0;
		animation: pulse 1.4s ease-in-out infinite;
	}

	@keyframes pulse {
		0%,
		100% {
			opacity: 1;
		}
		50% {
			opacity: 0.3;
		}
	}

	/* ── Status pills ────────────────────────────────────────────── */
	.pill {
		font-family: var(--font-mono);
		font-size: 10px;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		padding: 2px 6px;
		line-height: 1;
		flex-shrink: 0;
	}

	.pill.completed,
	.pill.done {
		color: var(--text-secondary);
		background: rgba(255, 255, 255, 0.07);
	}

	.pill.running {
		color: var(--accent-amber);
		background: var(--status-permission-glow);
	}

	.pill.failed,
	.pill.error {
		color: var(--accent-red);
		background: rgba(255, 68, 68, 0.12);
	}

	.pill.queued {
		color: var(--text-secondary);
		background: rgba(255, 255, 255, 0.04);
	}

	/* ── Detail ──────────────────────────────────────────────────── */
	.detail-header-bar {
		justify-content: flex-start;
	}

	.back-btn {
		font-family: var(--font-pixel);
		font-size: 14px;
		text-transform: uppercase;
		letter-spacing: 0.1em;
		color: var(--text-secondary);
		background: none;
		border: none;
		cursor: pointer;
		padding: 0;
		transition: color 0.15s ease;
	}

	.back-btn:hover {
		color: var(--text-primary);
	}

	.inline-loading {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--text-muted);
		text-transform: uppercase;
		letter-spacing: 0.05em;
		margin-left: auto;
	}

	.detail-area {
		flex: 1;
		overflow-y: auto;
		padding: 0 var(--space-xs) var(--space-lg);
	}

	.detail-head {
		padding-bottom: var(--space-md);
		margin-bottom: var(--space-lg);
		border-bottom: 1px solid var(--border-default);
	}

	.detail-title-row {
		display: flex;
		align-items: center;
		gap: var(--space-sm);
	}

	.detail-name {
		font-family: var(--font-mono);
		font-size: 18px;
		color: var(--text-primary);
	}

	.detail-summary {
		font-family: var(--font-sans);
		font-size: 13px;
		color: var(--text-muted);
		margin: var(--space-sm) 0 0;
		line-height: 1.5;
	}

	.detail-path {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--text-muted);
		margin-top: var(--space-sm);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.detail-stats {
		display: flex;
		flex-wrap: wrap;
		gap: var(--space-sm);
		margin-top: var(--space-md);
	}

	.stat-chip {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--text-secondary);
		background: var(--bg-card);
		border: 1px solid var(--border-default);
		padding: 2px var(--space-sm);
	}

	.stat-chip.model {
		color: var(--text-muted);
	}

	/* ── Phase groups + sub-headers ──────────────────────────────── */
	.phase-group {
		margin-bottom: var(--space-lg);
	}

	.sub-header {
		font-family: var(--font-pixel);
		font-size: 13px;
		text-transform: uppercase;
		color: var(--accent-amber);
		letter-spacing: 0.1em;
		line-height: 1;
		padding-bottom: var(--space-sm);
		margin-bottom: var(--space-sm);
		border-bottom: 1px solid var(--border-default);
	}

	/* ── Agent cards ─────────────────────────────────────────────── */
	.agent-card {
		background: var(--bg-card);
		border: 1px solid var(--border-default);
		margin-bottom: var(--space-sm);
		overflow: hidden;
		transition: border-color 0.15s ease;
	}

	.agent-card:hover,
	.agent-card.expanded {
		border-color: var(--text-muted);
	}

	.agent-head {
		display: flex;
		align-items: center;
		gap: var(--space-sm);
		width: 100%;
		padding: var(--space-sm) var(--space-md);
		background: none;
		border: none;
		cursor: pointer;
		text-align: left;
	}

	.agent-label {
		font-family: var(--font-mono);
		font-size: 13px;
		color: var(--text-primary);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		max-width: 220px;
	}

	.agent-spacer {
		flex: 1;
	}

	.agent-meta {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--text-muted);
		white-space: nowrap;
	}

	.agent-tool {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--text-secondary);
		white-space: nowrap;
	}

	.agent-chevron {
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--text-muted);
		flex-shrink: 0;
	}

	.agent-body {
		padding: 0 var(--space-md) var(--space-md);
	}

	.preview-label {
		display: flex;
		align-items: center;
		font-family: var(--font-pixel);
		font-size: 11px;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		color: var(--text-muted);
		margin: var(--space-sm) 0 var(--space-xs);
	}

	.preview-label .raw-toggle {
		margin-left: auto;
	}

	.preview-box {
		font-family: var(--font-mono);
		font-size: 12px;
		line-height: 1.5;
		color: var(--text-secondary);
		background: var(--bg-base);
		border: 1px solid var(--border-default);
		padding: var(--space-sm);
		margin: 0;
		max-height: 240px;
		overflow: auto;
		white-space: pre-wrap;
		word-break: break-word;
	}

	.preview-empty {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--text-muted);
		text-transform: uppercase;
		letter-spacing: 0.05em;
		padding: var(--space-sm) 0;
	}

	/* ── Collapsible script / result panels ──────────────────────── */
	.wf-panel {
		margin-top: var(--space-md);
	}

	.panel-head {
		display: flex;
		align-items: center;
		gap: var(--space-sm);
		width: 100%;
		font-family: var(--font-pixel);
		font-size: 13px;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.1em;
		color: var(--accent-amber);
		padding: var(--space-sm) 0;
		background: none;
		border: none;
		cursor: pointer;
		text-align: left;
		transition: color 0.15s ease;
	}

	.panel-head:hover {
		color: var(--text-primary);
	}

	.panel-title {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.panel-count {
		font-family: var(--font-pixel);
		font-size: 12px;
		color: var(--text-secondary);
	}

	.panel-inner {
		padding-top: var(--space-md);
	}

	.panel-chevron {
		margin-left: auto;
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--text-muted);
		flex-shrink: 0;
	}

	.code-box {
		font-family: var(--font-mono);
		font-size: 12px;
		line-height: 1.55;
		color: var(--text-secondary);
		background: var(--bg-base);
		border: 1px solid var(--border-default);
		padding: var(--space-md);
		margin: 0;
		max-height: 360px;
		overflow: auto;
		white-space: pre;
	}

	/* ── Raw/Parsed toggle ───────────────────────────────────────── */
	.raw-toggle {
		display: inline-flex;
		gap: 2px;
		margin-left: var(--space-sm);
	}

	.code-toolbar {
		display: flex;
		justify-content: flex-end;
		gap: 2px;
		margin-bottom: var(--space-xs);
	}

	.raw-btn {
		font-family: var(--font-mono);
		font-size: 10px;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		color: var(--text-muted);
		background: none;
		border: 1px solid var(--border-default);
		padding: 1px 6px;
		cursor: pointer;
		transition: all 0.15s ease;
	}

	.raw-btn:hover {
		color: var(--text-secondary);
	}

	.raw-btn.active {
		color: var(--accent-amber);
		border-color: var(--accent-amber);
	}
</style>
