<script lang="ts">
    // The undo toasts of the handling panel (build plan §G5, R7-SPEC §A.5).
    // One toast per waiting send, each with its own timer: a new toast or a
    // notice never hides another one (handoff tests #2, #3). ⌘Z undoes the
    // newest send; each toast also has its own 復原 button.
    import { secondsLeft, type Pending } from './actions';

    export let pending: Pending[] = [];
    /** Results that stay until closed (a failed send) or end on their own. */
    export let notices: { id: string; text: string; tone: 'info' | 'error' }[] = [];
    export let now: number;
    export let demo = false;
    export let onUndo: (id: string) => void;
    export let onDismiss: (id: string) => void;

    $: newest = [...pending].reverse().find(p => p.state === 'waiting')?.id ?? null;
</script>

{#if pending.length || notices.length}
    <div class="toasts" aria-live="polite">
        {#each notices as n (n.id)}
            <div class="toast" class:error={n.tone === 'error'} role="status" data-notice>
                <span class="t">{n.text}</span>
                <button type="button" on:click={() => onDismiss(n.id)}>關閉</button>
            </div>
        {/each}
        {#each pending as p (p.id)}
            <div class="toast" role="status" data-undo={p.id} data-state={p.state}>
                {#if p.state === 'waiting'}
                    <span class="t"><b>{secondsLeft(p, now)} 秒後送出</b> · {p.label}</span>
                    <button type="button" on:click={() => onUndo(p.id)}>復原{#if p.id === newest}<kbd>⌘Z</kbd>{/if}</button>
                {:else}
                    <span class="t"><b>送出中</b> · {p.label}</span>
                {/if}
            </div>
        {/each}
        {#if pending.length}
            <p class="rule" data-leave-rule>還沒送出的回答：離開這一頁或關掉視窗就不送，這件回到需要你，回答留在草稿裡。結束 c9watch 草稿就沒了。{demo ? '示範模式：不會真的送出。' : ''}</p>
        {/if}
    </div>
{/if}

<style>
    /* Sticky at the bottom of the page's scroll box. It takes its own space
       there, so at the end of the page it covers no button. */
    .toasts {
        position: sticky; bottom: var(--space-4); z-index: 5; max-width: 760px; margin-top: var(--space-6);
        display: flex; flex-direction: column; gap: var(--space-2);
    }
    .toast {
        display: flex; align-items: center; gap: var(--space-3); padding: 10px 14px;
        border: 1px solid var(--glass-stroke); border-radius: var(--radius-md);
        background: var(--surface-window-solid); box-shadow: 0 8px 24px rgb(0 0 0 / 0.35);
        font: var(--type-body); font-size: 13px; color: var(--text-1);
    }
    .toast.error { border-color: rgb(255 92 92 / 0.5); }
    .t { flex: 1; min-width: 0; }
    .t b { font-weight: 600; }
    button {
        display: inline-flex; gap: 6px; align-items: center; flex: none; padding: var(--space-1) var(--space-3);
        border: 1px solid var(--text-2); border-radius: var(--radius-sm); background: none;
        font: var(--type-label); color: var(--text-1); cursor: pointer;
    }
    button:hover { background: var(--fill-hover); }
    .rule { margin: 0; padding: 2px var(--space-1); font-size: 12px; color: var(--text-2); background: var(--surface-window-solid); border-radius: var(--radius-sm); }
</style>
