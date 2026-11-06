<script lang="ts">
    // R7-SPEC §A.8: the top of the handling panel while c9watch has lost the
    // agent. The panel's main button then says 「先存著，連上後送出」.
    import { waitText } from './detail';

    export let since: number;
    export let now: number;
    export let onReconnect: (() => void) | undefined = undefined;

    $: ago = now - since < 60_000 ? '剛剛' : `${waitText(since, now)}前`;
</script>

<div class="offline" role="status" data-offline>
    <span class="dot" aria-hidden="true"></span>
    <span>和 agent 斷線了 · {ago}</span>
    {#if onReconnect}
        <button type="button" on:click={onReconnect}>重新連線</button>
    {/if}
</div>

<style>
    .offline {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        padding: var(--space-2) var(--space-4);
        border: 1px solid rgb(255 92 92 / 0.35);
        border-radius: var(--radius-md);
        background: rgb(255 92 92 / 0.08);
        font: var(--type-body);
        font-size: 13px;
        color: var(--text-1);
    }
    .dot { width: 8px; height: 8px; background: var(--status-error); flex: none; }
    button {
        margin-left: auto;
        padding: var(--space-1) var(--space-3);
        border: 1px solid var(--glass-stroke);
        border-radius: var(--radius-sm);
        background: none;
        font: var(--type-label);
        color: var(--text-1);
        cursor: pointer;
    }
    button:hover { background: var(--fill-hover); }
</style>
