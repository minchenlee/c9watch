<script lang="ts">
	import { slide } from 'svelte/transition';
	import { cubicOut } from 'svelte/easing';
	import type { ResultSchema } from '$lib/types';
	import { pickColumns, badgeColor, isEnumProp } from '$lib/workflow-schema';

	// Render an array-of-objects as a schema-driven table. Short scalar props are
	// columns; the longest text prop is an expandable per-row body; enum props
	// render as colored badges. Caller only mounts this when matchSchema() hit and
	// pickColumns().fallback is false.
	let { rows, schema }: { rows: Record<string, unknown>[]; schema: ResultSchema } = $props();

	let layout = $derived(pickColumns(schema, rows));
	let openRows = $state<Set<number>>(new Set());

	function toggle(i: number) {
		const next = new Set(openRows);
		if (next.has(i)) next.delete(i);
		else next.add(i);
		openRows = next;
	}

	function cell(row: Record<string, unknown>, key: string): string {
		const v = row[key];
		if (v === null || v === undefined) return '';
		if (typeof v === 'object') return JSON.stringify(v);
		return String(v);
	}

	function isEnum(key: string): boolean {
		const p = schema.props.find((p) => p.name === key);
		return !!p && isEnumProp(p);
	}
</script>

<div class="schema-table">
	<div class="thead" style="--cols: {layout.columns.length}">
		<span class="th idx-col" aria-hidden="true"></span>
		{#each layout.columns as col (col.name)}
			<span class="th">{col.name}</span>
		{/each}
		{#if layout.bodyKey}
			<span class="th expand-col" aria-hidden="true"></span>
		{/if}
	</div>
	{#each rows as row, i (i)}
		{@const hasBody = layout.bodyKey != null && cell(row, layout.bodyKey).trim() !== ''}
		<div class="trow" class:open={openRows.has(i)}>
			<button
				class="tr"
				class:clickable={hasBody}
				style="--cols: {layout.columns.length}"
				onclick={() => hasBody && toggle(i)}
				disabled={!hasBody}
			>
				<span class="td idx-col">{i + 1}</span>
				{#each layout.columns as col (col.name)}
					{@const val = cell(row, col.name)}
					<span class="td">
						{#if isEnum(col.name) && val}
							<span class="badge {badgeColor(val)}">{val}</span>
						{:else}
							<span class="cell-text" title={val}>{val}</span>
						{/if}
					</span>
				{/each}
				{#if layout.bodyKey}
					<span class="td expand-col">{hasBody ? (openRows.has(i) ? '▾' : '▸') : ''}</span>
				{/if}
			</button>
			{#if hasBody && openRows.has(i) && layout.bodyKey}
				<div class="row-body" transition:slide|local={{ duration: 150, easing: cubicOut }}>
					{cell(row, layout.bodyKey)}
				</div>
			{/if}
		</div>
	{/each}
</div>

<style>
	.schema-table {
		display: flex;
		flex-direction: column;
		border: 1px solid var(--border-default);
		background: var(--bg-card);
	}

	.thead,
	.tr {
		display: grid;
		grid-template-columns: 28px repeat(var(--cols, 1), minmax(0, 1fr)) 20px;
		align-items: baseline;
		gap: var(--space-sm);
		width: 100%;
		text-align: left;
	}

	.thead {
		padding: var(--space-sm);
		border-bottom: 1px solid var(--border-default);
	}

	.th {
		font-family: var(--font-mono);
		font-size: 10px;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		color: var(--text-muted);
	}

	.tr {
		background: none;
		border: none;
		padding: var(--space-sm);
		cursor: default;
		font: inherit;
		color: inherit;
	}

	.tr.clickable {
		cursor: pointer;
	}

	.trow + .trow {
		border-top: 1px solid var(--border-default);
	}

	.td {
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--text-secondary);
		min-width: 0;
	}

	.idx-col {
		color: var(--text-muted);
		font-size: 11px;
	}

	.expand-col {
		color: var(--accent-amber);
		text-align: center;
	}

	.cell-text {
		display: block;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.row-body {
		font-family: var(--font-mono);
		font-size: 12px;
		line-height: 1.5;
		color: var(--text-secondary);
		white-space: pre-wrap;
		word-break: break-word;
		padding: 0 var(--space-sm) var(--space-sm) calc(28px + var(--space-sm));
	}

	.badge {
		font-family: var(--font-mono);
		font-size: 10px;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		padding: 1px 6px;
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
