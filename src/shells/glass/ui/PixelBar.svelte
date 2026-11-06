<script lang="ts">
    // Classic pixel bar (direction §10): a grid of 8px blocks, split in
    // proportion, pixel counts, one sweep when the counts change. No
    // scanline and no mesh. Legend 12px.
    import { allocateBlocks, blockColumns } from './format';

    export let working: number;
    export let needs: number;
    export let waiting: number;

    let width = 0;
    let sweeping = false;
    let lastKey = '';
    let timer: ReturnType<typeof setTimeout> | undefined;

    $: key = `${working}-${needs}-${waiting}`;
    $: {
        if (lastKey && key !== lastKey) {
            sweeping = false;
            clearTimeout(timer);
            // Restart the animation on the next frame.
            requestAnimationFrame(() => { sweeping = true; timer = setTimeout(() => (sweeping = false), 2000); });
        }
        lastKey = key;
    }
    $: columns = blockColumns(width);
    $: split = allocateBlocks([working, needs, waiting], columns);
    $: cells = [
        ...Array(split[0]).fill('working'),
        ...Array(split[1]).fill('needs'),
        ...Array(split[2]).fill('waiting'),
        ...Array(Math.max(0, columns - split[0] - split[1] - split[2])).fill('empty')
    ] as string[];
</script>

<div class="pixel-bar" data-working={working} data-needs={needs} data-waiting={waiting}>
    <div class="track" class:empty={working + needs + waiting === 0} bind:clientWidth={width} aria-hidden="true">
        <div class="cells" class:sweeping>
            {#each cells as c, i (i)}<i class={c}></i>{/each}
        </div>
    </div>
    <div class="legend">
        <span class="lg working" class:off={!working}>WORKING <b>{working}</b></span>
        <span class="lg needs" class:off={!needs}>NEEDS YOU <b>{needs}</b></span>
        <span class="lg waiting" class:off={!waiting}>WAITING <b>{waiting}</b></span>
    </div>
</div>

<style>
    .pixel-bar {
        display: grid;
        gap: var(--space-3);
        min-width: 0;
        padding: var(--space-4) var(--space-4) var(--space-3);
        background: var(--surface-1);
    }
    .track { height: 8px; overflow: hidden; }
    .track.empty { opacity: 0.5; }
    .cells { position: relative; display: flex; gap: 2px; height: 8px; }
    .cells i { flex: 0 0 8px; height: 8px; background: rgb(255 255 255 / 0.06); transition: background-color 0.4s; }
    .cells i.working { background: var(--status-working); }
    .cells i.needs { background: var(--status-permission); }
    .cells i.waiting { background: var(--status-input); }
    .cells::after { content: ''; position: absolute; inset: 0; background: #fff; opacity: 0; pointer-events: none; }
    .cells.sweeping { animation: glass-sweep 2s ease-out forwards; }
    .cells.sweeping::after { animation: glass-flash 2s ease-out forwards; }
    @keyframes glass-sweep { 0% { transform: scale(1); } 20% { transform: scale(0.99); } 40% { transform: scale(1.01); } 100% { transform: scale(1); } }
    @keyframes glass-flash { 0% { opacity: 0; } 20% { opacity: 0.05; } 40% { opacity: 0.25; } 100% { opacity: 0; } }
    @media (prefers-reduced-motion: reduce) {
        .cells.sweeping, .cells.sweeping::after { animation: none; }
    }
    .legend { display: flex; flex-wrap: wrap; gap: var(--space-1) var(--space-5); align-items: center; }
    .lg {
        display: inline-flex;
        align-items: center;
        gap: var(--space-2);
        font-family: var(--font-data);
        font-size: 12px;
        line-height: 16px;
        letter-spacing: 0.1em;
        color: var(--text-2);
    }
    .lg::before { content: ''; width: 8px; height: 8px; }
    .lg.working::before { background: var(--status-working); }
    .lg.needs::before { background: var(--status-permission); }
    .lg.waiting::before { background: var(--status-input); }
    .lg.off { color: var(--text-3); }
    .lg b { font-family: var(--font-pixel); font-weight: 500; font-size: 16px; letter-spacing: 0; color: var(--text-1); }
    .lg.off b { color: var(--text-3); }
</style>
