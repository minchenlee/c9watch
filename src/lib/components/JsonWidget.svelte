<script lang="ts">
	import { slide } from 'svelte/transition';
	import { cubicOut } from 'svelte/easing';
	import { getContext } from 'svelte';
	import Self from './JsonWidget.svelte';
	import SchemaTable from './SchemaTable.svelte';
	import type { ResultSchema } from '$lib/types';
	import { matchSchema, pickColumns } from '$lib/workflow-schema';

	// Recursive native-GUI renderer for an arbitrary JSON value. Objects render as
	// key/value rows, arrays as numbered cards, strings as (clamped) text blocks,
	// scalars inline. Light key-name heuristics add badges / mono / emphasis.
	let {
		value,
		depth = 0,
		emphasis = false,
	}: { value: unknown; depth?: number; emphasis?: boolean } = $props();

	const MAX_DEPTH = 12;
	const LONG_STRING = 280;

	let expanded = $state(false);

	// Result schemas (from the workflow script) reach the recursive tree via
	// context, so we can render a matched array-of-objects as a tailored table
	// instead of generic cards. Tolerate absence (JsonWidget works standalone).
	const schemaCtx = getContext<{ value: ResultSchema[] }>('wf-result-schemas');
	function resultSchemas(): ResultSchema[] {
		return schemaCtx?.value ?? [];
	}

	// When an array's elements are all objects, try a schema-driven table.
	// Returns null (→ generic card render) when no schema, no match, or the
	// matched layout is unsuitable for a table.
	function tableFor(
		arr: unknown[]
	): { rows: Record<string, unknown>[]; schema: ResultSchema } | null {
		const schemas = resultSchemas();
		if (schemas.length === 0 || arr.length === 0) return null;
		if (!arr.every((x) => x && typeof x === 'object' && !Array.isArray(x))) return null;
		const rows = arr as Record<string, unknown>[];
		const elemKeys = Array.from(new Set(rows.flatMap((r) => Object.keys(r))));
		const schema = matchSchema(elemKeys, schemas);
		if (!schema) return null;
		if (pickColumns(schema, rows).fallback) return null;
		return { rows, schema };
	}

	function kind(v: unknown): string {
		if (v === null) return 'null';
		if (Array.isArray(v)) return 'array';
		return typeof v; // 'object' | 'string' | 'number' | 'boolean'
	}

	// A string that would be ambiguous with a real scalar/null when shown
	// unquoted (e.g. "null", "true", "42"). Quote these on render.
	function looksLikeLiteral(s: string): boolean {
		return s === 'null' || s === 'true' || s === 'false' || /^-?\d+(\.\d+)?$/.test(s);
	}

	// ── Smart hints (cosmetic, key-name driven) ──────────────────────
	interface Hint {
		kind: 'badge' | 'mono' | 'emphasis' | null;
		badgeColor?: 'red' | 'amber' | 'green' | 'neutral';
	}

	// Split a key into lowercase word tokens, handling camelCase, snake_case,
	// kebab-case, and digit boundaries. "statusCode" -> ['status','code'].
	function keyTokens(key: string): string[] {
		return key
			.replace(/([a-z0-9])([A-Z])/g, '$1 $2')
			.split(/[\s_\-.]+/)
			.map((w) => w.toLowerCase())
			.filter(Boolean);
	}

	const BADGE_KEYS = new Set(['severity', 'status', 'state', 'verdict', 'level']);
	const MONO_KEYS = new Set(['file', 'path', 'cwd', 'filepath', 'filename', 'dir', 'directory']);
	const EMPHASIS_KEYS = new Set(['title', 'summary', 'name', 'description', 'topic']);

	function keyHas(key: string, set: Set<string>): boolean {
		return keyTokens(key).some((t) => set.has(t));
	}

	function hintFor(key: string, v: unknown): Hint {
		if (typeof v === 'string' && v.length <= 24 && keyHas(key, BADGE_KEYS)) {
			return { kind: 'badge', badgeColor: badgeColor(v) };
		}
		if (typeof v === 'string' && keyHas(key, MONO_KEYS)) return { kind: 'mono' };
		if (typeof v === 'string' && keyHas(key, EMPHASIS_KEYS)) return { kind: 'emphasis' };
		return { kind: null };
	}

	// Color a short status/severity value by WHOLE-TOKEN match (not substring),
	// so "slow"/"below"/"highlighted" don't collide with low/high. Tokens are
	// matched against curated sets; first set with a hit wins (red > amber > green).
	const RED_TOKENS = new Set(['bug', 'error', 'fail', 'failed', 'failure', 'critical', 'high', 'reject', 'rejected', 'blocked', 'broken', 'crash']);
	const AMBER_TOKENS = new Set(['warn', 'warning', 'medium', 'uncertain', 'partial', 'pending', 'skip', 'skipped', 'unknown']);
	const GREEN_TOKENS = new Set(['ok', 'pass', 'passed', 'done', 'complete', 'completed', 'confirmed', 'low', 'success', 'good', 'green', 'resolved']);

	function badgeColor(v: string): 'red' | 'amber' | 'green' | 'neutral' {
		const toks = v.toLowerCase().split(/[\s_\-/,]+/).filter(Boolean);
		if (toks.some((t) => RED_TOKENS.has(t))) return 'red';
		if (toks.some((t) => AMBER_TOKENS.has(t))) return 'amber';
		if (toks.some((t) => GREEN_TOKENS.has(t))) return 'green';
		return 'neutral';
	}

	// For an array of objects, pick a header field to label each card. Prefer the
	// most descriptive field (title/summary/description) over short id-like
	// fields (name/file), and cap the label length so a long paragraph can't blow
	// out the card header.
	const CARD_LABEL_MAX = 140;
	function cardLabel(obj: Record<string, unknown>): { badge?: string; badgeColor?: string; text?: string } {
		const out: { badge?: string; badgeColor?: string; text?: string } = {};
		for (const bk of ['severity', 'status', 'state', 'verdict']) {
			const v = obj[bk];
			if (typeof v === 'string' && v.length <= 24) {
				out.badge = v;
				out.badgeColor = badgeColor(v);
				break;
			}
		}
		for (const tk of ['title', 'summary', 'description', 'name', 'file', 'topic', 'probe']) {
			const v = obj[tk];
			if (typeof v === 'string' && v.trim()) {
				out.text = v.length > CARD_LABEL_MAX ? v.slice(0, CARD_LABEL_MAX) + '…' : v;
				break;
			}
		}
		return out;
	}

	// entries() that survives the depth cap by stringifying past it
	let entries = $derived(
		value && typeof value === 'object' && !Array.isArray(value)
			? Object.entries(value as Record<string, unknown>)
			: []
	);
</script>

{#if depth > MAX_DEPTH}
	<span class="scalar mono">{JSON.stringify(value)}</span>
{:else if value === null}
	<span class="scalar dim">null</span>
{:else if Array.isArray(value)}
	{#if value.length === 0}
		<span class="scalar dim">[]</span>
	{:else if tableFor(value)}
		{@const tbl = tableFor(value)!}
		<SchemaTable rows={tbl.rows} schema={tbl.schema} />
	{:else}
		<div class="array">
			{#each value as item, i (i)}
				{@const isObj = item && typeof item === 'object' && !Array.isArray(item)}
				<div class="card">
					{#if isObj}
						{@const lbl = cardLabel(item as Record<string, unknown>)}
						<div class="card-head">
							<span class="card-idx">{i + 1}</span>
							{#if lbl.badge}
								<span class="badge {lbl.badgeColor}">{lbl.badge}</span>
							{/if}
							{#if lbl.text}
								<span class="card-title">{lbl.text}</span>
							{/if}
						</div>
						<div class="card-body">
							<Self value={item} depth={depth + 1} />
						</div>
					{:else}
						<div class="card-head">
							<span class="card-idx">{i + 1}</span>
							<Self value={item} depth={depth + 1} />
						</div>
					{/if}
				</div>
			{/each}
		</div>
	{/if}
{:else if typeof value === 'object'}
	{#if entries.length === 0}
		<span class="scalar dim">{'{}'}</span>
	{:else}
		<div class="obj">
			{#each entries as [k, v] (k)}
				{@const h = hintFor(k, v)}
				{@const childKind = kind(v)}
				<div class="row" class:block={childKind === 'array' || (childKind === 'object' && v !== null)}>
					<span class="key">{k}{#if childKind === 'array'}<span class="key-count"> [{(v as unknown[]).length}]</span>{/if}</span>
					<div class="val">
						{#if h.kind === 'badge'}
							<span class="badge {h.badgeColor}">{v}</span>
						{:else if h.kind === 'mono' && typeof v === 'string'}
							<span class="scalar mono ellipsis" title={v}>{v}</span>
						{:else}
							<Self value={v} depth={depth + 1} emphasis={h.kind === 'emphasis'} />
						{/if}
					</div>
				</div>
			{/each}
		</div>
	{/if}
{:else if typeof value === 'string'}
	{@const long = value.length > LONG_STRING}
	{#if value === ''}
		<span class="scalar dim">""</span>
	{:else if long && !expanded}
		<div class="str clamped">
			<span class="str-text">{value.slice(0, LONG_STRING)}…</span>
			<button class="more-btn" onclick={() => (expanded = true)}>▸ more</button>
		</div>
	{:else if long && expanded}
		<div class="str" transition:slide|local={{ duration: 150, easing: cubicOut }}>
			<span class="str-text">{value}</span>
			<button class="more-btn" onclick={() => (expanded = false)}>▾ less</button>
		</div>
	{:else}
		<!-- Quote strings that look like a JSON literal (null/true/false/number)
		     so they can't be mistaken for the real scalar. -->
		<span class="str-text" class:emphasis>{looksLikeLiteral(value) ? JSON.stringify(value) : value}</span>
	{/if}
{:else}
	<!-- number / boolean -->
	<span class="scalar mono">{String(value)}</span>
{/if}

<style>
	.obj {
		display: flex;
		flex-direction: column;
		gap: 2px;
	}

	.row {
		display: flex;
		align-items: baseline;
		gap: var(--space-sm);
		padding: 1px 0;
	}

	/* When the value is itself a block (array/object), stack key above it. */
	.row.block {
		flex-direction: column;
		gap: 2px;
		align-items: stretch;
	}

	.key {
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--text-muted);
		white-space: nowrap;
		flex-shrink: 0;
	}

	.key-count {
		color: var(--text-secondary);
	}

	.val {
		min-width: 0;
		flex: 1;
	}

	.row.block > .val {
		padding-left: var(--space-md);
		border-left: 1px solid var(--border-default);
		margin-left: 2px;
	}

	/* ── Arrays / cards ──────────────────────────────────────────── */
	.array {
		display: flex;
		flex-direction: column;
		gap: var(--space-sm);
	}

	.card {
		background: var(--bg-card);
		border: 1px solid var(--border-default);
		padding: var(--space-sm);
	}

	.card-head {
		display: flex;
		align-items: baseline;
		gap: var(--space-sm);
		margin-bottom: 2px;
	}

	.card-idx {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--text-muted);
		flex-shrink: 0;
	}

	.card-title {
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--text-primary);
		line-height: 1.4;
	}

	.card-body {
		padding-left: var(--space-md);
	}

	/* ── Scalars ─────────────────────────────────────────────────── */
	.scalar {
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--text-secondary);
	}

	.mono {
		font-family: var(--font-mono);
	}

	.dim {
		color: var(--text-muted);
	}

	.ellipsis {
		display: inline-block;
		max-width: 100%;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		vertical-align: bottom;
	}

	.str {
		display: flex;
		flex-direction: column;
		align-items: flex-start;
		gap: 2px;
	}

	.str-text {
		font-family: var(--font-mono);
		font-size: 12px;
		line-height: 1.5;
		color: var(--text-secondary);
		white-space: pre-wrap;
		word-break: break-word;
	}

	.str-text.emphasis {
		color: var(--text-primary);
	}

	.more-btn {
		font-family: var(--font-mono);
		font-size: 10px;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		color: var(--accent-amber);
		background: none;
		border: none;
		padding: 0;
		cursor: pointer;
	}

	/* ── Badges (reuse pill aesthetic) ───────────────────────────── */
	.badge {
		font-family: var(--font-mono);
		font-size: 10px;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		padding: 1px 6px;
		line-height: 1.4;
		flex-shrink: 0;
		white-space: nowrap;
	}

	.badge.red {
		color: var(--accent-red);
		background: rgba(255, 68, 68, 0.12);
	}

	.badge.amber {
		color: var(--accent-amber);
		background: var(--status-permission-glow);
	}

	.badge.green {
		color: #5fd35f;
		background: rgba(95, 211, 95, 0.1);
	}

	.badge.neutral {
		color: var(--text-secondary);
		background: rgba(255, 255, 255, 0.07);
	}
</style>
