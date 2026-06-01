<script lang="ts">
	import { fade, scale } from 'svelte/transition';
	import { quintOut } from 'svelte/easing';
	import JsonWidget from './JsonWidget.svelte';
	import { outline } from '$lib/result-outline';

	// Full-view reading modal for a workflow run's RESULT. Left = outline nav
	// (jump to a top-level field), right = reading-optimized widget tree. A
	// Parsed/Raw toggle flips to verbatim JSON. Mounted inside WorkflowViewer so
	// the JsonWidget still reads `wf-result-schemas` from context.
	let {
		value,
		rawJson,
		title,
		onclose,
	}: {
		value: unknown;
		rawJson: string;
		title: string;
		onclose: () => void;
	} = $props();

	let raw = $state(false);
	let entries = $derived(outline(value));
	// Anchor ids for outline jump targets — index-based so duplicate/empty keys
	// still resolve.
	function anchorId(i: number): string {
		return `result-field-${i}`;
	}

	let paneEl = $state<HTMLDivElement | null>(null);
	function jumpTo(i: number) {
		const el = paneEl?.querySelector(`#${anchorId(i)}`);
		el?.scrollIntoView({ behavior: 'smooth', block: 'start' });
	}

	function onKeydown(e: KeyboardEvent) {
		if (e.key === 'Escape') {
			e.stopPropagation();
			onclose();
		}
	}

	function onBackdrop(e: MouseEvent) {
		if (e.target === e.currentTarget) onclose();
	}

	// Render the value as either the top-level object's keyed sections (so each
	// gets an anchor) or, for arrays/scalars, a single widget.
	let isObject = $derived(value !== null && typeof value === 'object' && !Array.isArray(value));
</script>

<svelte:window onkeydown={onKeydown} />

<!-- svelte-ignore a11y_click_events_have_key_events -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
	class="result-backdrop"
	role="dialog"
	aria-modal="true"
	aria-label="Workflow result"
	tabindex="-1"
	onclick={onBackdrop}
	transition:fade={{ duration: 180 }}
>
	<div class="result-modal" in:scale={{ start: 0.96, duration: 240, easing: quintOut }}>
		<header class="result-head">
			<div class="head-left">
				<span class="head-eyebrow">Result</span>
				<span class="head-title">{title}</span>
			</div>
			<div class="head-right">
				<div class="toggle">
					<button class="toggle-btn" class:active={!raw} onclick={() => (raw = false)}>Parsed</button>
					<button class="toggle-btn" class:active={raw} onclick={() => (raw = true)}>Raw</button>
				</div>
				<button class="close-btn" onclick={onclose} aria-label="Close">✕</button>
			</div>
		</header>

		<div class="result-body">
			{#if raw}
				<pre class="raw-pane">{rawJson}</pre>
			{:else}
				{#if entries.length > 0}
					<nav class="outline">
						<div class="outline-head">Outline</div>
						{#each entries as e, i (i)}
							<button class="outline-row" onclick={() => jumpTo(i)}>
								<span class="o-key">{e.key || '(value)'}</span>
								{#if e.kind === 'array' || e.kind === 'object'}
									<span class="o-count">{e.hint}</span>
								{/if}
							</button>
						{/each}
					</nav>
				{/if}
				<div class="reading-pane" bind:this={paneEl}>
					{#if isObject}
						{#each entries as e, i (i)}
							<section class="field" id={anchorId(i)}>
								<div class="field-key">{e.key}</div>
								<div class="field-val">
									<JsonWidget value={(value as Record<string, unknown>)[e.key]} reading />
								</div>
							</section>
						{/each}
					{:else}
						<section class="field" id={anchorId(0)}>
							<JsonWidget {value} reading />
						</section>
					{/if}
				</div>
			{/if}
		</div>
	</div>
</div>

<style>
	.result-backdrop {
		position: fixed;
		inset: 0;
		background: var(--bg-overlay);
		display: flex;
		align-items: center;
		justify-content: center;
		z-index: 1000;
		padding: var(--space-2xl);
	}

	.result-modal {
		display: flex;
		flex-direction: column;
		width: min(1100px, 100%);
		height: min(840px, 92vh);
		background: var(--bg-elevated);
		border: 1px solid var(--border-default);
		overflow: hidden;
	}

	.result-head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding: var(--space-md) var(--space-lg);
		border-bottom: 1px solid var(--border-default);
		flex-shrink: 0;
	}

	.head-left {
		display: flex;
		align-items: baseline;
		gap: var(--space-md);
		min-width: 0;
	}

	.head-eyebrow {
		font-family: var(--font-pixel, var(--font-mono));
		font-size: 13px;
		text-transform: uppercase;
		letter-spacing: 0.1em;
		color: var(--accent-amber);
	}

	.head-title {
		font-family: var(--font-mono);
		font-size: 13px;
		color: var(--text-secondary);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.head-right {
		display: flex;
		align-items: center;
		gap: var(--space-md);
		flex-shrink: 0;
	}

	.toggle {
		display: flex;
		gap: 2px;
	}

	.toggle-btn {
		font-family: var(--font-mono);
		font-size: 10px;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		padding: 3px 10px;
		background: none;
		border: 1px solid var(--border-default);
		color: var(--text-muted);
		cursor: pointer;
	}

	.toggle-btn.active {
		color: var(--accent-amber);
		border-color: var(--accent-amber);
	}

	.close-btn {
		font-family: var(--font-mono);
		font-size: 14px;
		background: none;
		border: none;
		color: var(--text-muted);
		cursor: pointer;
		padding: 0 4px;
	}
	.close-btn:hover {
		color: var(--text-primary);
	}

	.result-body {
		display: flex;
		flex: 1;
		min-height: 0;
	}

	.raw-pane {
		flex: 1;
		margin: 0;
		padding: var(--space-lg);
		overflow: auto;
		font-family: var(--font-mono);
		font-size: 13px;
		line-height: 1.55;
		color: var(--text-secondary);
		white-space: pre-wrap;
		word-break: break-word;
	}

	.outline {
		width: 240px;
		flex-shrink: 0;
		border-right: 1px solid var(--border-default);
		overflow-y: auto;
		padding: var(--space-md) 0;
		background: var(--bg-base);
	}

	.outline-head {
		font-family: var(--font-mono);
		font-size: 10px;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		color: var(--text-muted);
		padding: 0 var(--space-lg) var(--space-sm);
	}

	.outline-row {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		gap: var(--space-sm);
		width: 100%;
		text-align: left;
		background: none;
		border: none;
		padding: 5px var(--space-lg);
		cursor: pointer;
		color: var(--text-secondary);
	}
	.outline-row:hover {
		background: var(--bg-card);
		color: var(--text-primary);
	}

	.o-key {
		font-family: var(--font-mono);
		font-size: 13px;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.o-count {
		font-family: var(--font-mono);
		font-size: 10px;
		color: var(--text-muted);
		flex-shrink: 0;
	}

	.reading-pane {
		flex: 1;
		min-width: 0;
		overflow-y: auto;
		padding: var(--space-lg) var(--space-xl);
	}

	.field {
		padding: var(--space-md) 0;
		border-bottom: 1px solid var(--border-default);
		scroll-margin-top: var(--space-md);
	}
	.field:last-child {
		border-bottom: none;
	}

	.field-key {
		font-family: var(--font-pixel, var(--font-mono));
		font-size: 13px;
		text-transform: uppercase;
		letter-spacing: 0.08em;
		color: var(--accent-amber);
		margin-bottom: var(--space-sm);
	}

	.field-val {
		padding-left: var(--space-sm);
	}
</style>
