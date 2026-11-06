<script lang="ts">
    // Annotation chips (core plan §8.2): every annotation on a session,
    // ordered by namespace/name, rendered by type, unit and format. Generic:
    // no writer gets special code.
    import type { C9pAnnotation } from '$lib/c9p-1';
    import { annotationLabel, annotationValue } from './format';

    export let items: C9pAnnotation[];
    export let now: number;
</script>

{#if items.length}
    <ul class="chips" aria-label="標註">
        {#each items as a (a.key)}
            <li class="chip" title="{a.key} · {a.writer}">
                <span class="k">{annotationLabel(a)}</span>
                <span class="v" class:redacted={a.redacted || !a.value}>{annotationValue(a, now)}</span>
            </li>
        {/each}
    </ul>
{/if}

<style>
    .chips {
        display: flex;
        flex-wrap: wrap;
        gap: var(--space-1);
        margin: 0;
        padding: 0;
        list-style: none;
        min-width: 0;
    }
    .chip {
        display: inline-flex;
        gap: 6px;
        max-width: 100%;
        padding: 1px 6px;
        border: 1px solid var(--glass-stroke);
        font: var(--type-label);
        color: var(--text-3);
        white-space: nowrap;
        overflow: hidden;
    }
    .k { overflow: hidden; text-overflow: ellipsis; }
    .v { font: var(--type-meta); font-size: 12px; color: var(--text-2); }
    .v.redacted { color: var(--text-3); font-style: italic; }
</style>
