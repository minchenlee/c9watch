<script lang="ts">
    // Workspace page (redesign §8.3, build plan G3): 進行中 (classic cards
    // per lead or talk row), 已完成 (`history.list`), 用量 (`cost.get`) and
    // 記憶與指示 (`memory.list`). Rows and members come from the G2 `model`
    // store; this page never derives them from raw c9p rows.
    import { onMount } from 'svelte';
    import { readable } from 'svelte/store';
    import { model, annotations, fixtureOn } from '../data';
    import type { MemberView, RowView, WorkspaceView } from '../model';
    import type { Need } from '../model/needs';
    import { DEMO_GOALS, DEMO_RULES } from '../model/gaps-demo';
    import SessionCard from '../ui/SessionCard.svelte';
    import PixelBar from '../ui/PixelBar.svelte';
    import { annotationsFor, formatTokens, formatUsd, timeAgo } from '../ui/format';
    import { codeParts, historyFor, historyTitle, loadExtras, localDay, memoryFor, rulesFor, spendFor, type Loaded } from './workspace-extras';
    import type { C9pCostData, C9pHistoryEntry, C9pProjectMemory } from '$lib/c9p-1';

    export let id: string;
    export let title: string;

    const clock = readable(Date.now(), set => { const t = setInterval(() => set(Date.now()), 60_000); return () => clearInterval(t); });
    $: now = Math.max($clock, Date.now());

    $: ws = $model.workspaces.find(w => w.id === id) ?? null;
    $: needByMember = new Map<string, Need>($model.needs.map(n => [n.memberKey, n]));
    $: members = ws ? ws.rows.flatMap(r => r.members) : [];
    $: counts = countOf(members, needByMember);

    function countOf(list: MemberView[], needs: Map<string, Need>) {
        const c = { working: 0, needs: 0, waiting: 0 };
        for (const m of list) {
            if (!m.live) continue;
            if (needs.has(m.key) || m.status === 'needs') c.needs++;
            else if (m.status === 'working') c.working++;
            else if (m.status === 'waiting') c.waiting++;
        }
        return c;
    }
    function rowLabel(r: RowView): { name: string; meta: string } {
        if (r.kind === 'talk') return { name: `對談 · ${r.title}`, meta: '' };
        const workers = r.members.filter(m => m.role === 'worker').length;
        const seats = r.cap != null && r.freeSeats != null ? ` · 空 ${r.freeSeats} 位，上限 ${r.cap}` : '';
        return { name: r.title, meta: `${workers} 個 worker${seats}` };
    }
    const liveCount = (w: WorkspaceView) => w.rows.flatMap(r => r.members).filter(m => m.live).length;

    type Section<T> = Loaded<T> | null;
    let history: Section<C9pHistoryEntry[]> = null;
    let cost: Section<C9pCostData | null> = null;
    let memory: Section<C9pProjectMemory[]> = null;
    onMount(() => {
        let gone = false;
        // A child's onMount runs before the Shell's, which picks the demo
        // transport (startGlassData). Read after it, one microtask later.
        void Promise.resolve().then(loadExtras).then(x => { if (gone) return; history = x.history; cost = x.cost; memory = x.memory; });
        return () => { gone = true; };
    });

    const HISTORY_SHOWN = 8;
    $: liveIds = new Set(members.filter(m => m.live && m.session).map(m => m.session!.id));
    $: done = ws && history?.ok ? historyFor(history.value, ws.root, liveIds) : [];
    $: spend = ws && cost?.ok ? spendFor(cost.value, ws.root, localDay(now)) : null;
    $: files = ws && memory?.ok ? memoryFor(memory.value, ws.root) : [];
    // Rules exist only in the demo gaps file (Gap R): fixture mode only.
    $: rules = ws && $fixtureOn ? rulesFor(DEMO_RULES, ws, DEMO_GOALS) : [];
    const day = (t: number) => { const d = new Date(t); return `${String(d.getMonth() + 1).padStart(2, '0')}/${String(d.getDate()).padStart(2, '0')}`; };
    const failMsg = '讀不到這一區的資料';
</script>

<div class="ws-page" data-ws={id}>
    <header>
        <h1>{title}</h1>
        {#if ws}<p class="sub"><span class="mono">{ws.root}</span> · {liveCount(ws)} 個 agent 在線</p>{/if}
    </header>

    {#if !ws}
        <p class="state-msg">這個 workspace 現在沒有 agent</p>
    {:else}
        <PixelBar working={counts.working} needs={counts.needs} waiting={counts.waiting} />

        <section aria-labelledby="ws-active">
            <h2 class="psec" class:p={counts.needs > 0} id="ws-active">進行中 <span>{ws.rows.length}</span></h2>
            {#each ws.rows as row (row.id)}
                {@const l = rowLabel(row)}
                <div class="row" data-row={row.id}>
                    <h3 class="row-h"><span class="name">{l.name}</span>{#if l.meta}<span class="meta">{l.meta}</span>{/if}</h3>
                    <div class="grid">
                        {#each row.members as m (m.key)}
                            <SessionCard member={m} need={needByMember.get(m.key) ?? null} annotations={annotationsFor($annotations.values(), m.sessionKey)} {now} />
                        {/each}
                    </div>
                </div>
            {/each}
        </section>

        <section aria-labelledby="ws-done">
            <h2 class="psec" id="ws-done">已完成 <span>{done.length}</span></h2>
            {#if history === null}
                <p class="state-msg">讀取中</p>
            {:else if !history.ok}
                <p class="state-msg">{failMsg}</p>
            {:else if !done.length}
                <p class="state-msg">這個 workspace 還沒有完成的對談</p>
            {:else}
                <ul class="list">
                    {#each done.slice(0, HISTORY_SHOWN) as e (e.provider + e.sessionId)}
                        <li class="li">
                            <span class="mono when">{day(e.timestamp)}</span>
                            <span class="what"><span class="t">{historyTitle(e)}</span>{#if e.display && e.display !== historyTitle(e)}<span class="d">{` · ${e.display}`}</span>{/if}</span>
                            <span class="mono meta">{timeAgo(e.timestamp, now)}</span>
                        </li>
                    {/each}
                </ul>
                {#if done.length > HISTORY_SHOWN}<p class="more">還有 {done.length - HISTORY_SHOWN} 件</p>{/if}
            {/if}
        </section>

        <section aria-labelledby="ws-cost">
            <h2 class="psec" id="ws-cost">用量（token）</h2>
            {#if cost === null}
                <p class="state-msg">讀取中</p>
            {:else if !cost.ok}
                <p class="state-msg">{failMsg}</p>
            {:else if !spend}
                <p class="state-msg">這個 workspace 還沒有用量紀錄</p>
            {:else}
                <div class="tiles">
                    <div class="tile"><span class="lbl">今天</span><b class="num">{formatTokens(spend.today)}</b><span class="mono meta">≈ API 價 {formatUsd(spend.todayCost)}</span></div>
                    <div class="tile"><span class="lbl">本月（{localDay(now).slice(5, 7)}/01 起）</span><b class="num">{formatTokens(spend.month)}</b><span class="mono meta">≈ API 價 {formatUsd(spend.monthCost)}</span></div>
                </div>
                <p class="note">
                    {#if spend.monthShare != null}本月占全部 workspace 的 {Math.round(spend.monthShare)}%。{/if}
                    「≈ API 價」是照 API 價格估算，不是實付金額。
                </p>
                <table class="tbl">
                    <thead><tr><th>模型</th><th class="n">token</th><th class="n">≈ API 價</th></tr></thead>
                    <tbody>
                        {#each spend.byModel as m (m.model)}
                            <tr><td>{m.model}</td><td class="n">{formatTokens(m.tokens)}</td><td class="n">{m.priced ? formatUsd(m.cost) : '沒有價格表'}</td></tr>
                        {/each}
                    </tbody>
                </table>
            {/if}
        </section>

        <section aria-labelledby="ws-memory">
            <h2 class="psec" id="ws-memory">記憶與指示</h2>
            {#if rules.length}
                <ul class="list rules" aria-label="這個 workspace 的規則">
                    {#each rules as r (r.kind + r.text)}
                        <li class="rule"><span class="t">{#each codeParts(r.text) as part}{#if part.code}<code>{part.text}</code>{:else}{part.text}{/if}{/each}</span><span class="mono meta">{r.kind} · {r.scope} · 上次用到 {r.lastUsed} · 共 {r.count} 次</span></li>
                    {/each}
                </ul>
            {/if}
            {#if memory === null}
                <p class="state-msg">讀取中</p>
            {:else if !memory.ok}
                <p class="state-msg">{failMsg}</p>
            {:else if !files.length}
                {#if !rules.length}<p class="state-msg">這個 workspace 還沒有記憶檔</p>{/if}
            {:else}
                <div class="files">
                    {#each files as f (f.dir + f.filename)}
                        <details class="file">
                            <summary><span class="t">{f.filename}</span><span class="mono meta">{f.dir}</span></summary>
                            <pre class="body">{f.content}</pre>
                        </details>
                    {/each}
                </div>
            {/if}
        </section>
    {/if}
</div>

<style>
    .ws-page { display: grid; gap: var(--space-8); min-width: 0; }
    header { display: grid; gap: var(--space-1); }
    h1 { margin: 0; font: var(--type-title); color: var(--text-1); }
    .sub { margin: 0; font: var(--type-body); color: var(--text-3); overflow-wrap: anywhere; }
    .mono { font: var(--type-meta); }
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
    .psec.p::before { background: var(--status-permission); }
    .psec span { color: var(--text-3); }
    .row { display: grid; gap: var(--space-2); min-width: 0; }
    .row + .row { margin-top: var(--space-3); }
    .row-h { display: flex; flex-wrap: wrap; align-items: baseline; gap: var(--space-2); margin: 0; font: var(--type-heading); color: var(--text-1); }
    .row-h .meta { font: var(--type-meta); font-size: 12px; color: var(--text-3); }
    .grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(min(260px, 100%), 1fr)); gap: 10px; }
    .list { margin: 0; padding: 0; list-style: none; background: var(--surface-1); }
    .li, .rule { display: grid; gap: var(--space-1) var(--space-3); padding: 10px var(--space-4); border-bottom: 1px solid var(--divider); min-width: 0; }
    .li { grid-template-columns: auto minmax(0, 1fr) auto; align-items: baseline; }
    .li:last-child, .rule:last-child { border-bottom: 0; }
    .when, .meta { font-size: 12px; color: var(--text-3); }
    .what { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .t { color: var(--text-1); }
    .t code { font: var(--type-meta); color: var(--text-1); }
    .d { color: var(--text-2); }
    .more, .note { margin: 0; font: var(--type-label); color: var(--text-3); }
    .tiles { display: grid; grid-template-columns: repeat(auto-fit, minmax(min(200px, 100%), 1fr)); gap: 10px; }
    .tile { display: grid; gap: var(--space-1); padding: 14px var(--space-4); background: var(--surface-1); }
    .lbl { font: var(--type-label); color: var(--text-3); }
    .num { font: var(--type-display); color: var(--text-1); }
    .tbl { width: 100%; border-collapse: collapse; background: var(--surface-1); font: var(--type-body); }
    .tbl th { text-align: left; font: var(--type-label); color: var(--text-3); padding: 10px var(--space-4); border-bottom: 1px solid var(--divider); }
    .tbl td { padding: 10px var(--space-4); border-bottom: 1px solid var(--divider); color: var(--text-2); overflow-wrap: anywhere; }
    .tbl tr:last-child td { border-bottom: 0; }
    .tbl .n { text-align: right; font: var(--type-meta); white-space: nowrap; }
    .files { display: grid; gap: 1px; background: var(--divider); }
    .file { background: var(--surface-1); }
    .file summary { display: flex; flex-wrap: wrap; gap: var(--space-1) var(--space-3); align-items: baseline; padding: 10px var(--space-4); cursor: pointer; }
    .file summary .meta { overflow-wrap: anywhere; }
    .body { margin: 0; padding: 0 var(--space-4) var(--space-4); font: var(--type-body); color: var(--text-2); white-space: pre-wrap; overflow-wrap: anywhere; }
</style>
