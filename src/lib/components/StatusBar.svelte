<script lang="ts">
	import { untrack } from 'svelte';

	interface Props {
		summary: {
			working: number;
			permission: number;
			input: number;
		};
		total: number;
	}

	let { summary, total }: Props = $props();

	let trackWidth = $state(0);
	let isSweeping = $state(false);
	let prevSummaryKey = $state('');

	// Create a key from the summary to detect changes
	let summaryKey = $derived(`${summary.working}-${summary.permission}-${summary.input}`);

	// Detect changes and trigger sweep animation
	$effect(() => {
		const currentKey = summaryKey;
		const previousKey = untrack(() => prevSummaryKey);
		
		if (previousKey !== '' && currentKey !== previousKey) {
			// Status changed, trigger sweep
			isSweeping = true;
			setTimeout(() => {
				isSweeping = false;
			}, 6000); // 6s duration to cover ripple delay across all blocks
		}
		prevSummaryKey = currentKey;
	});

	// Calculate blocks to maintain square aspect ratio
	// Height = 16px - 2px(border) - 6px(padding) = 8px
	// Square blocks
	// Height = 8px
	// Block width = 8px
	// Gap = 2px
	// Stride = 10px
	let columns = $derived(Math.max(1, Math.floor((trackWidth - 6) / 10)));
	let totalBlocks = $derived(columns);

	let blocks = $derived.by(() => {
		if (total === 0) return { working: 0, permission: 0, input: 0 };

		const counts = [summary.working, summary.permission, summary.input];
		const percentages = counts.map((c) => (c / total) * totalBlocks);
		const integerParts = percentages.map((p) => Math.floor(p));
		const remainders = percentages.map((p, i) => p - integerParts[i]);

		let allocated = integerParts.reduce((a, b) => a + b, 0);
		const result = [...integerParts];

		while (allocated < totalBlocks) {
			let maxRemainder = -1;
			let maxIndex = -1;
			for (let i = 0; i < remainders.length; i++) {
				if (remainders[i] > maxRemainder) {
					maxRemainder = remainders[i];
					maxIndex = i;
				}
			}
			if (maxIndex === -1) break;
			result[maxIndex]++;
			remainders[maxIndex] = -1;
			allocated++;
		}

		return {
			working: result[0],
			permission: result[1],
			input: result[2]
		};
	});

	let statusArray = $derived.by(() => {
		const arr: string[] = [];
		for (let i = 0; i < blocks.working; i++) arr.push('working');
		for (let i = 0; i < blocks.permission; i++) arr.push('permission');
		for (let i = 0; i < blocks.input; i++) arr.push('input');
		while (arr.length < totalBlocks) arr.push('empty');
		return arr;
	});
</script>

<div class="system-status-bar">

	<div class="progress-track" class:empty={total === 0} bind:clientWidth={trackWidth}>
		<div class="grid-container" style="grid-template-columns: repeat({columns}, 1fr);">
			{#each statusArray as status, i}
				<div class="rect {status}" class:sweeping={isSweeping} style="animation-delay: {i * 10}ms; transition-delay: {i * 25}ms"></div>
			{/each}
		</div>
	</div>

	<div class="legend">
		<div class="legend-item" class:inactive={summary.working === 0}>
			<span class="dot working"></span>
			<span class="label">WORKING</span>
			<span class="count">{summary.working}</span>
		</div>
		<div class="legend-item" class:inactive={summary.permission === 0}>
			<span class="dot permission"></span>
			<span class="label">APPROVAL</span>
			<span class="count">{summary.permission}</span>
		</div>
		<div class="legend-item" class:inactive={summary.input === 0}>
			<span class="dot input"></span>
			<span class="label">WAITING</span>
			<span class="count">{summary.input}</span>
		</div>
	</div>

	<div class="deco-mesh"></div>
</div>

<style>
	.system-status-bar {
		position: relative;
		background: var(--bg-card);
		border: 1px solid var(--border-default);
		padding: var(--space-lg) var(--space-xl);
		display: flex;
		flex-direction: column;
		gap: var(--space-md);
		overflow: hidden;
		transition: border-color var(--transition-fast);
	}

	.system-status-bar:hover {
		border-color: var(--text-muted);
	}

	/* Scanline effect */
	.system-status-bar::after {
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

	.progress-track.empty {
		opacity: 0.5;
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
		background: rgba(255, 255, 255, 0.05); /* Slightly darker base */
		border-radius: 1px;
		opacity: 1; /* Normal visibility by default */
		transition: background-color 0.4s, box-shadow 0.4s;
	}

	/* Sweep animation only plays on status change */
	.rect.sweeping {
		animation: monitor-sweep 2s ease-out forwards;
	}

	@keyframes monitor-sweep {
		0% { transform: scale(1); filter: brightness(1); }
		20% { transform: scale(0.95); filter: brightness(1.1); }
		40% { transform: scale(1.1); filter: brightness(1.4) drop-shadow(0 0 2px currentColor); }
		100% { transform: scale(1); filter: brightness(1); }
	}

	.rect.working { background-color: var(--status-working); color: var(--status-working); box-shadow: 0 0 4px var(--status-working-glow); }
	.rect.permission { background-color: var(--status-permission); color: var(--status-permission); box-shadow: 0 0 4px var(--status-permission-glow); }
	.rect.input { background-color: var(--status-input); color: var(--status-input); box-shadow: 0 0 4px var(--status-input-glow); }

	.legend {
		display: flex;
		gap: var(--space-xl);
	}

	.legend-item {
		display: flex;
		align-items: center;
		gap: var(--space-sm);
		transition: transform var(--transition-fast);
	}

	.legend-item.inactive {
		opacity: 1; /* Force full opacity for readability */
	}

	.legend-item .dot {
		width: 8px;
		height: 8px;
	}

	.legend-item .dot.working { background: var(--status-working); }
	.legend-item .dot.permission { background: var(--status-permission); }
	.legend-item .dot.input { background: var(--status-input); }

	.legend-item .label {
		font-family: var(--font-mono);
		font-size: 14px;
		color: var(--text-secondary);
		letter-spacing: 0.1em;
	}

	.legend-item .count {
		font-family: var(--font-pixel);
		font-size: 16px;
		color: var(--text-primary);
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

	/* ── Mobile Responsive ─────────────────────────────────────── */
	@media (max-width: 768px) {
		.system-status-bar {
			padding: var(--space-md);
			gap: var(--space-sm);
		}

		.legend {
			flex-wrap: wrap;
			gap: var(--space-sm) var(--space-lg);
		}

		.legend-item {
			gap: 6px;
		}

		.legend-item .label {
			font-size: 11px;
		}

		.legend-item .count {
			font-size: 13px;
		}
	}

	@media (max-width: 768px) and (orientation: landscape) {
		.system-status-bar {
			padding: var(--space-sm) var(--space-md);
			gap: 6px;
		}

		.progress-track {
			height: 12px;
		}
	}

</style>
