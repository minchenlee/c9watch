<script lang="ts">
    // 今天 (redesign §8.13 D2, §8.14; build plan G10a): a review of today.
    // 需要你 is one line that only opens that page. Sections come from
    // `today-model.ts`; data: the G2 `model`, `history.list` and
    // `session.away` (since local midnight).
    import { onMount } from 'svelte';
    import { readable } from 'svelte/store';
    import { model, fixtureOn } from '../data';
    import { DEMO_TODAY } from '../model/gaps-demo';
    import { request } from '$lib/data/client';
    import type { C9pAwayActivity, C9pHistoryEntry } from '$lib/c9p-1';
    import { routeHash } from '../nav';
    import { readPages, type Loaded } from './workspace-extras';
    import { startRefresh, type Refresher } from './refresh';
    import { awayRequests, awayText, localMidnight, todaySections, type TodayItem } from './today-model';

    /** Open needs, the same count as the side bar. */
    export let needs: number;

    const clock = readable(Date.now(), set => { const t = setInterval(() => set(Date.now()), 60_000); return () => clearInterval(t); });
    $: now = Math.max($clock, Date.now());
    $: midnight = localMidnight(now);

    let history: Loaded<C9pHistoryEntry[]> | null = null;
    let away: Record<string, C9pAwayActivity> = {};
    // History and `session.away` are read again every 60 s, on window
    // focus, and when the asked sessions change (at most every 15 s).
    // A failed read keeps the last answer; the next tick retries.
    let refresher: Refresher | null = null;
    let asked = '';
    $: requests = awayRequests($model, midnight);
    $: { const sig = JSON.stringify(requests); if (sig !== asked) { asked = sig; refresher?.kick(); } }
    async function read(alive: () => boolean) {
        const list = requests;
        const [h, a] = await Promise.all([
            readPages(async offset => { const r = await request('history.list', { page: { offset, limit: 1000 } }); return { items: (r?.entries ?? []) as C9pHistoryEntry[], nextOffset: r?.nextOffset }; })
                .then(value => ({ ok: true as const, value }), error => ({ ok: false as const, error })),
            list.length ? request('session.away', { requests: list }).then(r => r?.activity ?? {}, () => null) : Promise.resolve({})
        ]);
        if (!alive()) return;
        if (h.ok || !history?.ok) history = h;
        if (a !== null) away = a;
    }
    onMount(() => {
        let gone = false;
        // The Shell's onMount picks the transport after a child's: start one microtask later.
        void Promise.resolve().then(() => { if (!gone) refresher = startRefresh(read); });
        return () => { gone = true; refresher?.stop(); refresher = null; };
    });

    $: s = todaySections({ model: $model, history: history?.ok ? history.value : null, away, midnight, now, demo: $fixtureOn ? DEMO_TODAY : null });
    const hhmm = (t: number) => { const d = new Date(t); return `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`; };
    const when = (i: TodayItem) => i.atText ?? (i.at != null ? hhmm(i.at) : '');
    const wsHref = (id: string | null) => (id ? routeHash({ page: 'ws', id }) : null);
    $: dateText = (() => { const d = new Date(now); return `${d.getMonth() + 1} 月 ${d.getDate()} 日 · 星期${'日一二三四五六'[d.getDay()]}`; })();
</script>

<div class="today" data-today>
    <header>
        <h1>今天</h1>
        <p class="sub">{dateText} · 今天的成果留到午夜</p>
    </header>

    <a class="needs-line" class:on={needs > 0} href={routeHash({ page: 'needs' })} data-needs-line>
        <span class="need-dot" class:on={needs > 0} aria-hidden="true"></span>
        {#if needs > 0}查看 {needs} 件需要你 →{:else}沒有需要你的事{/if}
    </a>

    <section aria-labelledby="t-done" data-section="done">
        <h2 class="psec" id="t-done">{s.doneTitle} <span>{s.done.length}</span></h2>
        {#if !$fixtureOn && history === null}
            <p class="state-msg">讀取中</p>
        {:else if !$fixtureOn && history && !history.ok}
            <p class="state-msg">讀不到今天完成的對談</p>
        {:else if !s.done.length}
            <p class="state-msg">今天還沒有完成的事</p>
        {:else}
            <ul class="list">
                {#each s.done as i (i.key)}
                    <li class="li"><span class="mono when">{when(i)}</span><span class="what"><span class="t">{i.title}</span><span class="d">{` · ${i.meta}`}</span></span>{#if wsHref(i.workspaceId)}<a class="go" href={wsHref(i.workspaceId)}>看 ›</a>{/if}</li>
                {/each}
            </ul>
        {/if}
    </section>

    {#if s.rejected.length}
        <section aria-labelledby="t-rejected" data-section="rejected">
            <h2 class="psec" id="t-rejected">被退回 <span>{s.rejected.length}</span></h2>
            <ul class="list">
                {#each s.rejected as i (i.key)}
                    <li class="li"><span class="mono when">{when(i)}</span><span class="what"><span class="t">{i.title}</span><span class="d">{` · ${i.meta}`}</span></span>{#if wsHref(i.workspaceId)}<a class="go" href={wsHref(i.workspaceId)}>看 ›</a>{/if}</li>
                {/each}
            </ul>
        </section>
    {/if}

    <section aria-labelledby="t-attn" data-section="attention">
        <h2 class="psec" id="t-attn">需要留意 <span>{s.attention.length}</span></h2>
        {#if !s.attention.length}
            <p class="state-msg">沒有要留意的事</p>
        {:else}
            <p class="note">{s.attention.length} 項需要留意</p>
            <ul class="list">
                {#each s.attention as i (i.key)}
                    <li class="li"><span class="mono when">{when(i)}</span><span class="what"><span class="t">{i.title}</span><span class="d">{` · ${i.meta}`}</span></span>{#if wsHref(i.workspaceId)}<a class="go" href={wsHref(i.workspaceId)}>看 ›</a>{/if}</li>
                {/each}
            </ul>
        {/if}
    </section>

    <section aria-labelledby="t-active" data-section="active">
        <h2 class="psec" id="t-active">還在進行 <span>{s.active.length}</span></h2>
        {#if !s.active.length}
            <p class="state-msg">現在沒有 agent 在做事</p>
        {:else}
            <ul class="list">
                {#each s.active as r (r.key)}
                    <li class="li two"><span class="what"><span class="t">{r.title}</span><span class="d">{` · ${r.working} 個 agent 在做事`}</span>{#if awayText(r.away)}<span class="mono away">{awayText(r.away)}</span>{/if}</span><a class="go" href={routeHash({ page: 'ws', id: r.workspaceId })}>看 ›</a></li>
                {/each}
            </ul>
        {/if}
    </section>
</div>

<style>
    .today { display: grid; gap: var(--space-8); min-width: 0; max-width: 880px; }
    header { display: grid; gap: var(--space-1); }
    h1 { margin: 0; font: var(--type-title); color: var(--text-1); }
    .sub { margin: 0; font: var(--type-body); color: var(--text-3); }
    .mono { font: var(--type-meta); }
    .needs-line {
        display: flex;
        align-items: center;
        gap: var(--space-2);
        min-height: 40px;
        padding: 0 var(--space-4);
        border: 1px solid var(--glass-stroke);
        border-radius: var(--radius-md);
        background: var(--surface-1);
        font: var(--type-heading);
        color: var(--text-2);
        text-decoration: none;
    }
    .needs-line.on { color: var(--status-permission-text); }
    .needs-line:hover { background: var(--surface-1-hover); }
    section { display: grid; gap: var(--space-3); min-width: 0; }
    .psec {
        display: flex;
        align-items: center;
        gap: 10px;
        margin: 0;
        font: var(--type-section);
        letter-spacing: 0.1em;
        text-transform: uppercase;
        color: var(--text-1);
    }
    .psec::before { content: ''; width: 8px; height: 8px; background: var(--status-idle); }
    .psec span { color: var(--text-3); }
    .note { margin: 0; font: var(--type-label); color: var(--text-3); }
    .list { margin: 0; padding: 0; list-style: none; background: var(--surface-1); }
    .li { display: grid; grid-template-columns: auto minmax(0, 1fr) auto; align-items: baseline; gap: var(--space-1) var(--space-3); padding: 10px var(--space-4); border-bottom: 1px solid var(--divider); min-width: 0; }
    .li.two { grid-template-columns: minmax(0, 1fr) auto; }
    .li:last-child { border-bottom: 0; }
    .when { font-size: 12px; color: var(--text-3); }
    .what { min-width: 0; overflow: hidden; text-overflow: ellipsis; }
    .t { color: var(--text-1); }
    .d { color: var(--text-2); }
    .away { display: block; margin-top: 2px; font-size: 12px; color: var(--text-3); }
    .go { font: var(--type-label); color: var(--text-2); text-decoration: none; white-space: nowrap; }
    .go:hover { color: var(--text-1); }
</style>
