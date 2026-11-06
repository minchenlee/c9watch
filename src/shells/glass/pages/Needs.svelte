<script lang="ts">
    // The 需要你 page (build plan §G4, R7-SPEC §C, needs-proto.html): the
    // queue (280px) next to the one handling panel. Read-only in G4: the
    // panel shows every verb, but sending (G5) is not wired.
    import { onDestroy, onMount } from 'svelte';
    import { dataError, fixtureOn, model } from '../data';
    import { usePageKeys } from '../keys';
    import { DEMO_NEED_DETAILS, DEMO_OFFLINE_AGO_MS, DEMO_OFFLINE_KEY } from '../model/gaps-demo';
    import HandlingPanel from '../handling/HandlingPanel.svelte';
    import { buildDetail, emptyAnswer, locate, waitText, type Answer } from '../handling/detail';

    // Demo details only while the glass fixture is on: live mode never shows them.
    $: demo = $fixtureOn ? DEMO_NEED_DETAILS : null;
    $: needs = $model.needs;
    $: details = needs.map(n => buildDetail(n, $model.workspaces, demo));

    let currentId: string | null = null;
    $: index = Math.max(0, details.findIndex(d => d.id === currentId));
    $: current = details[index] ?? null;
    $: next = details.length > 1 ? details[(index + 1) % details.length] : null;

    // One draft per item, so switching items keeps what was typed.
    let answers: Record<string, Answer> = {};
    $: if (current && !answers[current.id]) answers[current.id] = emptyAnswer();

    // Handled items (G5 fills this). Their verbs are not orange: they no
    // longer need you (handoff 「實作前要改的錯」 #3).
    let handled: { id: string; verb: string; title: string }[] = [];

    let now = Date.now();
    const timer = setInterval(() => (now = Date.now()), 30_000);
    onDestroy(() => clearInterval(timer));

    // A feed error means c9watch lost the core and so every agent. The time
    // is when this page first saw it. Reconnect restarts every feed.
    let offlineSince: number | null = null;
    $: if ($dataError != null && offlineSince === null) offlineSince = Date.now();
    $: if ($dataError == null) offlineSince = demoOffline($fixtureOn);
    function demoOffline(fixture: boolean): number | null {
        if (!fixture) return null;
        try { return localStorage.getItem(DEMO_OFFLINE_KEY) === '1' ? Date.now() - DEMO_OFFLINE_AGO_MS : null; } catch { return null; }
    }
    const reconnect = () => location.reload();

    // True once the person picked an item (click, J / K). Only then may the
    // panel take the focus.
    let moved = false;
    const show = (id: string) => { moved = true; currentId = id; };
    const go = (step: number) => {
        if (!details.length) return;
        show(details[(index + step + details.length) % details.length].id);
    };
    let panel: HandlingPanel | undefined;
    onMount(() => usePageKeys({
        option: n => panel?.option(n),
        submit: () => panel?.submit(),
        openChat: () => panel?.openChat(),
        next: () => go(1),
        prev: () => go(-1)
    }));

    const rowName = (id: string) => {
        const n = needs.find(x => x.id === id);
        const r = n ? locate(n, $model.workspaces).row : null;
        return r ? (r.kind === 'talk' ? `對談 · ${r.title}` : r.title) : '';
    };
    $: sortRule = needs.some(n => n.blocking != null) ? '依擋住的 agent 數排。一樣多時，等越久越前面。' : '等越久越前面。';
</script>

<div class="needs" data-needs={details.length}>
    <aside class="queue" aria-label="需要你的事">
        {#if details.length}<span class="rule">{sortRule}</span>{/if}
        {#each details as d (d.id)}
            <button type="button" class="qi" class:on={d.id === current?.id} aria-current={d.id === current?.id ? 'true' : undefined} on:click={() => show(d.id)} data-need={d.id}>
                <span class="t"><span class="verb">{d.verb}</span>{d.title}</span>
                <span class="s">{[rowName(d.id), `等了 ${waitText(d.since, now)}`, d.reversible ? '' : '不能復原'].filter(Boolean).join(' · ')}</span>
            </button>
        {:else}
            <p class="empty">現在沒有事。</p>
        {/each}
        {#if handled.length}
            <details class="handled">
                <summary>已處理 {handled.length} 件</summary>
                <ul>{#each handled as h (h.id)}<li><span class="verb done">{h.verb}</span>{h.title}</li>{/each}</ul>
            </details>
        {/if}
    </aside>
    <main class="view" aria-label="需要你">
        {#if current}
            <HandlingPanel
                bind:this={panel}
                detail={current}
                bind:answer={answers[current.id]}
                position={{ index: index + 1, total: details.length }}
                next={next ? { verb: next.verb, title: next.title } : null}
                offline={offlineSince !== null ? { since: offlineSince } : null}
                {now}
                autofocus={moved}
                onReconnect={reconnect}
                onNext={() => go(1)}
                onPrev={() => go(-1)}
            />
        {:else}
            <div class="none">
                <h1>沒有需要你的事</h1>
                <p>agent 要你允許、決定、給東西或審查時，會出現在這裡。</p>
            </div>
        {/if}
    </main>
</div>

<style>
    .needs { display: grid; grid-template-columns: 280px minmax(0, 1fr); min-width: 0; min-height: 0; height: 100%; }
    .queue {
        display: flex; flex-direction: column; gap: var(--space-1); min-height: 0; overflow: auto;
        padding: var(--space-5) var(--space-3); border-right: 1px solid var(--divider); background: var(--surface-1);
    }
    .rule { margin: 2px var(--space-1) 10px; font-size: 12px; color: var(--text-3); }
    .qi {
        display: block; width: 100%; padding: 9px var(--space-3); border: 0; border-radius: var(--radius-md);
        background: transparent; font: var(--type-body); font-size: 13px; color: var(--text-2); text-align: left; cursor: pointer;
    }
    .qi:hover { background: var(--fill-hover); }
    .qi.on { background: var(--fill-active); color: var(--text-1); box-shadow: inset 2px 0 0 var(--text-1); }
    .t { display: block; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .s { display: block; font-size: 12px; color: var(--text-3); }
    .verb { margin-right: 4px; font-weight: 700; color: var(--status-permission-text); }
    .verb.done { color: var(--text-3); }
    .empty { margin: var(--space-1); font-size: 13px; color: var(--text-3); }
    .handled { margin: 10px var(--space-1) 0; font-size: 12px; color: var(--text-3); }
    .handled summary { cursor: pointer; text-decoration: underline; }
    .handled ul { margin: 6px 0 0; padding-left: var(--space-4); }
    .view { min-width: 0; overflow: auto; padding: 28px var(--space-4) 60px var(--space-12); }
    .none h1 { margin: 0 0 var(--space-2); font: var(--type-title); color: var(--text-1); }
    .none p { margin: 0; font: var(--type-body); color: var(--text-3); }
    @media (max-width: 1100px) { .view { padding-left: var(--space-8); } }
    @media (max-width: 760px) {
        .needs { grid-template-columns: minmax(0, 1fr); grid-template-rows: auto minmax(0, 1fr); }
        .queue { max-height: 40vh; border-right: 0; border-bottom: 1px solid var(--divider); }
        .view { padding-left: var(--space-4); }
    }
</style>
