<script lang="ts">
	import { flip } from 'svelte/animate';
	import { flyIn } from '$lib/transitions';
	import type { Session } from '$lib/types';
	import { SessionStatus } from '$lib/types';
	import { sessionKeyOf } from '$lib/provider';
	import { columnCount, denseTiles, packColumns, type ProjectGroupLike } from '$lib/dense-layout';
	import DenseSessionRow from './DenseSessionRow.svelte';

	interface Props {
		groups: ProjectGroupLike<Session>[];
		/** One line per session. */
		compact?: boolean;
		onexpand: (session: Session) => void;
		onstop: (session: Session) => void;
		onopen: (session: Session) => void;
	}

	let { groups, compact = false, onexpand, onstop, onopen }: Props = $props();

	const MIN_COLUMN = 300;
	const GAP = 6;
	// Estimated pixel heights; only their ratio matters for packing.
	const HEAD = 30;
	const ROW = 41;
	const ROW_COMPACT = 25;

	let width = $state(0);
	let tiles = $derived(denseTiles(groups));
	let columns = $derived(
		packColumns(
			tiles,
			columnCount(width, MIN_COLUMN, GAP, tiles.length),
			(tile) => HEAD + tile.sessions.length * (compact ? ROW_COMPACT : ROW)
		)
	);
	// Tile order for the entry cascade: top row first, like reading.
	let order = $derived(new Map(tiles.map((tile, i) => [tile.path, i])));

	function toneOf(session: Session): string {
		switch (session.status) {
			case SessionStatus.NeedsAttention: return 'attention';
			case SessionStatus.WaitingForInput: return 'idle';
			case SessionStatus.Connecting: return 'connecting';
			default: return 'working';
		}
	}
</script>

<div class="board" bind:clientWidth={width} style="--cols: {columns.length}; --gap: {GAP}px;">
	{#each columns as column, ci (ci)}
		<div class="column">
			{#each column as tile (tile.path)}
				<section
					class="tile"
					class:attention={tile.tier === 0}
					in:flyIn|global={{ index: order.get(tile.path) ?? 0, y: 4, stride: 30 }}
				>
					<header class="tile-head" title={tile.path}>
						<span class="name">{tile.displayName}</span>
						<span class="count">{tile.sessions.length}</span>
						<span class="pips" aria-hidden="true">
							{#each tile.sessions as session (sessionKeyOf(session))}
								<i class="pip {toneOf(session)}"></i>
							{/each}
						</span>
					</header>
					<div class="rows">
						{#each tile.sessions as session (sessionKeyOf(session))}
							<div class="row-slot" animate:flip={{ duration: 250 }}>
								<DenseSessionRow
									{session}
									{compact}
									onexpand={() => onexpand(session)}
									onstop={() => onstop(session)}
									onopen={() => onopen(session)}
								/>
							</div>
						{/each}
					</div>
				</section>
			{/each}
		</div>
	{/each}
</div>

<style>
	.board {
		display: grid;
		grid-template-columns: repeat(var(--cols), minmax(0, 1fr));
		gap: var(--gap);
		align-items: start;
	}

	.column {
		display: flex;
		flex-direction: column;
		gap: var(--gap);
		min-width: 0;
	}

	.tile {
		container: tile / inline-size;
		background: var(--bg-card);
		border: 1px solid var(--border-default);
		min-width: 0;
	}

	.tile.attention {
		border-color: color-mix(in srgb, var(--status-permission) 60%, var(--border-default));
		box-shadow: 0 0 16px -4px var(--status-permission-glow);
	}

	.tile-head {
		display: flex;
		align-items: center;
		gap: 6px;
		height: 24px;
		padding: 0 8px;
		background: var(--bg-elevated);
		border-bottom: 1px solid var(--border-muted);
		min-width: 0;
	}

	.name {
		min-width: 0;
		overflow: hidden;
		white-space: nowrap;
		text-overflow: ellipsis;
		font-family: var(--font-pixel);
		font-size: 11px;
		font-weight: 600;
		letter-spacing: 0.1em;
		text-transform: uppercase;
		color: var(--text-primary);
	}

	.count {
		flex-shrink: 0;
		font-family: var(--font-pixel);
		font-size: 11px;
		color: var(--text-muted);
	}

	/* One square per session, echoing the system status bar. */
	.pips {
		margin-left: auto;
		display: inline-flex;
		flex-wrap: wrap;
		justify-content: flex-end;
		gap: 2px;
		max-width: 50%;
	}

	.pip {
		width: 6px;
		height: 6px;
		background: var(--status-working);
	}

	.pip.attention { background: var(--status-permission); }
	.pip.idle { background: var(--status-input); }
	.pip.connecting { background: var(--status-connecting); }

	.rows {
		display: flex;
		flex-direction: column;
	}

	.row-slot + .row-slot {
		border-top: 1px solid var(--border-muted);
	}

	@media (max-width: 768px) {
		.tile-head { height: 26px; }
		.name, .count { font-size: 12px; }
	}
</style>
