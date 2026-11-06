<script lang="ts">
    // 用量 (redesign §8.4, §8.14; build plan G10a): the one place money
    // shows (D6). Periods 今天 / 7 天 / 30 天 / 本月 / 上個月; four cards
    // (實付, 照 API 價估算, 用量, agent 工作時數); daily token bars in one
    // gray; plan quota from `usage.subscriptions`; rows by workspace and by
    // model with token, 分攤（實付）and ≈ API 價. Math: `usage-math.ts`.
    import { onMount } from 'svelte';
    import { readable } from 'svelte/store';
    import { request } from '$lib/data/client';
    import type { C9pCostData, C9pSubscriptionUsage } from '$lib/c9p-1';
    import { fixtureOn } from '../data';
    import { DEMO_AGENT_HOURS, DEMO_WORKSPACES } from '../model/gaps-demo';
    import { monthlyFee } from '../prefs';
    import { routeHash } from '../nav';
    import { formatTokens, formatUsd } from '../ui/format';
    import { localDay, type Loaded } from './workspace-extras';
    import { PERIODS, QUOTA_WARN, apiPriceText, hoursIn, rangeFor, summarize, windowLabel, type Period, type UsageRow } from './usage-math';

    const clock = readable(Date.now(), set => { const t = setInterval(() => set(Date.now()), 60_000); return () => clearInterval(t); });
    $: now = Math.max($clock, Date.now());
    $: today = localDay(now);

    let period: Period = 'month';
    let cost: Loaded<C9pCostData | null> | null = null;
    let quota: Loaded<C9pSubscriptionUsage[]> | null = null;
    const settle = <T>(p: Promise<T>): Promise<Loaded<T>> => p.then(value => ({ ok: true as const, value }), error => ({ ok: false as const, error }));
    onMount(() => {
        let gone = false;
        // The Shell's onMount picks the transport after a child's: read one microtask later.
        void Promise.resolve().then(() => Promise.all([
            settle(request('cost.get', {}).then(r => (r ?? null) as C9pCostData | null)),
            settle(request('usage.subscriptions', {}).then(r => (r?.usage ?? []) as C9pSubscriptionUsage[]))
        ])).then(([c, q]) => { if (gone) return; cost = c; quota = q; });
        return () => { gone = true; };
    });

    $: range = rangeFor(period, today);
    $: sum = cost?.ok ? summarize(cost.value, range, $monthlyFee, $fixtureOn ? DEMO_WORKSPACES : null) : null;
    $: hours = $fixtureOn ? hoursIn(range, today, DEMO_AGENT_HOURS) : null;
    $: peak = sum ? Math.max(0, ...sum.daily.map(d => d.tokens)) : 0;
    const errText = (e: unknown) => (e && typeof e === 'object' && 'msg' in e ? String((e as { msg: unknown }).msg) : e instanceof Error ? e.message : String(e));
    const md = (day: string) => `${Number(day.slice(5, 7))}/${Number(day.slice(8, 10))}`;
    $: rangeText = range.from === range.to ? md(range.from) : `${md(range.from)} – ${md(range.to)}`;
    const resetText = (at: number | null) => {
        if (at == null) return '';
        const d = new Date(at * 1000);
        return `${d.getMonth() + 1}/${d.getDate()} ${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')} 重置`;
    };
    const paidText = (r: UsageRow) => (r.paid === null ? '—' : formatUsd(r.paid));
    const barTitle = (d: { date: string; tokens: number; apiCost: number; pricing: 'all' | 'some' | 'none' }) =>
        `${md(d.date)} · ${formatTokens(d.tokens)} token${d.tokens ? ` · ≈ API 價 ${apiPriceText(d.apiCost, d.pricing, formatUsd)}` : ''}`;
</script>

<div class="usage" data-usage data-period={period}>
    <header>
        <h1>用量</h1>
        <p class="sub">{rangeText} · 「≈ API 價」照 API 價格估算，不是實付金額。</p>
    </header>

    <div class="periods" role="radiogroup" aria-label="期間">
        {#each PERIODS as p (p.id)}
            <button type="button" role="radio" aria-checked={period === p.id} class:on={period === p.id} data-period-btn={p.id} on:click={() => (period = p.id)}>{p.label}</button>
        {/each}
    </div>

    {#if cost === null}
        <p class="state-msg">讀取中</p>
    {:else if !cost.ok}
        <p class="state-msg">讀不到用量資料</p>
    {:else if sum}
        <div class="tiles">
            <div class="tile" data-card="paid">
                <span class="lbl">實付</span>
                {#if sum.paid === null}
                    <b class="num dim">—</b>
                    <span class="meta">還沒填月費 · <a href={routeHash({ page: 'settings' })}>到設定填</a></span>
                {:else}
                    <b class="num">{formatUsd(sum.paid)}</b>
                    <span class="meta">月費 {formatUsd($monthlyFee ?? 0)}，按天分攤</span>
                {/if}
            </div>
            <div class="tile" data-card="api">
                <span class="lbl">照 API 價估算</span>
                {#if !sum.tokens}
                    <b class="num dim">—</b><span class="meta">這段期間沒有用量</span>
                {:else if sum.pricing === 'none'}
                    <b class="num dim">沒有價格</b><span class="meta">這些模型沒有價格表</span>
                {:else}
                    <b class="num">{formatUsd(sum.apiCost)}</b>
                    <span class="meta">{sum.pricing === 'some' ? `只算有價格的部分 · ${formatTokens(sum.unpricedTokens)} token 沒有價格` : '不是實付金額'}</span>
                {/if}
            </div>
            <div class="tile" data-card="tokens">
                <span class="lbl">用量</span>
                <b class="num">{formatTokens(sum.tokens)}</b>
                <span class="meta">token · {sum.byModel.length} 個模型</span>
            </div>
            <div class="tile" data-card="hours">
                <span class="lbl">agent 工作時數</span>
                {#if hours === null}
                    <b class="num dim">—</b><span class="meta">c9watch 還沒有記錄 agent 的工作時間</span>
                {:else}
                    <b class="num">{hours} 小時</b><span class="meta">所有 agent 加總</span>
                {/if}
            </div>
        </div>

        <section aria-labelledby="u-daily">
            <h2 class="psec" id="u-daily">每日 token</h2>
            {#if period === 'today'}
                <p class="note">今天只有一天，長條圖看 7 天或 30 天。</p>
            {:else}
                <div class="bars" style:--n={sum.daily.length} aria-label="每日 token 長條圖">
                    {#each sum.daily as d (d.date)}
                        <div class="bar" title={barTitle(d)} data-day={d.date} data-tokens={d.tokens}><i style:height={peak ? `${(d.tokens / peak) * 100}%` : '0'}></i></div>
                    {/each}
                </div>
                <div class="axis mono"><span>{md(range.from)}</span><span>最高 {formatTokens(peak)}</span><span>{md(range.to)}</span></div>
            {/if}
        </section>

        <section aria-labelledby="u-ws">
            <h2 class="psec" id="u-ws">依 workspace</h2>
            {#if !sum.byWorkspace.length}
                <p class="state-msg">這段期間沒有用量</p>
            {:else}
                <table class="tbl" data-table="workspace">
                    <thead><tr><th>workspace</th><th class="n">token</th><th class="n">分攤（實付）</th><th class="n">≈ API 價</th></tr></thead>
                    <tbody>
                        {#each sum.byWorkspace as r (r.key)}
                            <tr><td><a href={routeHash({ page: 'ws', id: r.workspaceId ?? r.key })}>{r.label}</a></td><td class="n">{formatTokens(r.tokens)}</td><td class="n">{paidText(r)}</td><td class="n">{apiPriceText(r.apiCost, r.pricing, formatUsd)}</td></tr>
                        {/each}
                    </tbody>
                </table>
            {/if}
        </section>

        <section aria-labelledby="u-model">
            <h2 class="psec" id="u-model">依模型</h2>
            {#if sum.byModel.length}
                <table class="tbl" data-table="model">
                    <thead><tr><th>模型</th><th class="n">token</th><th class="n">分攤（實付）</th><th class="n">≈ API 價</th></tr></thead>
                    <tbody>
                        {#each sum.byModel as r (r.key)}
                            <tr><td>{r.label}</td><td class="n">{formatTokens(r.tokens)}</td><td class="n">{paidText(r)}</td><td class="n">{apiPriceText(r.apiCost, r.pricing, formatUsd)}</td></tr>
                        {/each}
                    </tbody>
                </table>
                <p class="note">分攤 = 實付 × 這一列的 token 占比。</p>
            {:else}
                <p class="state-msg">這段期間沒有用量</p>
            {/if}
        </section>
    {/if}

    <section aria-labelledby="u-quota" data-section="quota">
        <h2 class="psec" id="u-quota">訂閱額度</h2>
        {#if quota === null}
            <p class="state-msg">讀取中</p>
        {:else if !quota.ok}
            <p class="reason" data-quota-reason>讀不到額度：{errText(quota.error)}</p>
        {:else if !quota.value.length}
            <p class="reason" data-quota-reason>沒有讀到任何訂閱方案的額度。</p>
        {:else}
            <div class="plans">
                {#each quota.value as plan (plan.provider + plan.name)}
                    <div class="plan" data-plan={plan.provider}>
                        <div class="plan-h"><span class="t">{plan.name}</span>{#if plan.plan}<span class="meta">{plan.plan}</span>{/if}</div>
                        {#if !plan.windows.length}
                            <p class="reason" data-quota-reason>{plan.message ?? '這個方案沒有額度資料'}</p>
                        {:else}
                            {#each plan.windows as w (w.label)}
                                {@const pct = Math.max(0, Math.min(100, w.usedPercent))}
                                <div class="win" class:warn={w.usedPercent >= QUOTA_WARN} data-window={w.label}>
                                    <span class="wl">{windowLabel(w.label)}</span>
                                    <span class="track"><i style:width={`${pct}%`}></i></span>
                                    <span class="mono pct">{Math.round(w.usedPercent)}%</span>
                                    <span class="mono meta">{resetText(w.resetsAt)}</span>
                                </div>
                            {/each}
                            {#if plan.message}<p class="note">{plan.message}</p>{/if}
                        {/if}
                    </div>
                {/each}
            </div>
        {/if}
    </section>
</div>

<style>
    .usage { display: grid; gap: var(--space-8); min-width: 0; max-width: 960px; }
    header { display: grid; gap: var(--space-1); }
    h1 { margin: 0; font: var(--type-title); color: var(--text-1); }
    .sub { margin: 0; font: var(--type-body); color: var(--text-3); }
    .mono { font: var(--type-meta); }
    .periods { display: flex; flex-wrap: wrap; gap: var(--space-1); margin-top: calc(-1 * var(--space-4)); }
    .periods button {
        min-height: 32px;
        padding: 0 var(--space-3);
        border: 1px solid var(--glass-stroke);
        border-radius: var(--radius-sm);
        background: none;
        font: var(--type-label);
        color: var(--text-2);
        cursor: pointer;
    }
    .periods button:hover { background: var(--fill-hover); }
    .periods button.on { background: var(--fill-active); color: var(--text-1); }
    .periods button:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: 1px; }
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
    .tiles { display: grid; grid-template-columns: repeat(auto-fit, minmax(min(190px, 100%), 1fr)); gap: 10px; }
    .tile { display: grid; align-content: start; gap: var(--space-1); padding: 14px var(--space-4); background: var(--surface-1); }
    .lbl { font: var(--type-label); color: var(--text-3); }
    .num { font: var(--type-display); color: var(--text-1); }
    .num.dim { color: var(--text-3); }
    .meta { font: var(--type-meta); font-size: 12px; color: var(--text-3); }
    .meta a, td a { color: var(--text-2); }
    .note, .reason { margin: 0; font: var(--type-label); color: var(--text-3); }
    .reason { color: var(--text-2); }
    .bars { display: grid; grid-template-columns: repeat(var(--n), minmax(0, 1fr)); align-items: end; gap: 2px; height: 120px; padding: var(--space-3) var(--space-4) 0; background: var(--surface-1); }
    .bar { height: 100%; display: flex; align-items: flex-end; }
    .bar i { display: block; width: 100%; min-height: 1px; background: var(--status-idle); }
    .axis { display: flex; justify-content: space-between; font-size: 12px; color: var(--text-3); }
    .tbl { width: 100%; border-collapse: collapse; background: var(--surface-1); font: var(--type-body); }
    .tbl th { text-align: left; font: var(--type-label); color: var(--text-3); padding: 10px var(--space-4); border-bottom: 1px solid var(--divider); }
    .tbl td { padding: 10px var(--space-4); border-bottom: 1px solid var(--divider); color: var(--text-2); overflow-wrap: anywhere; }
    .tbl tr:last-child td { border-bottom: 0; }
    .tbl .n { text-align: right; font: var(--type-meta); white-space: nowrap; }
    .plans { display: grid; gap: 10px; }
    .plan { display: grid; gap: var(--space-2); padding: 14px var(--space-4); background: var(--surface-1); }
    .plan-h { display: flex; gap: var(--space-2); align-items: baseline; }
    .t { color: var(--text-1); }
    .win { display: grid; grid-template-columns: 72px minmax(0, 1fr) 44px auto; align-items: center; gap: var(--space-3); }
    .wl { font: var(--type-label); color: var(--text-2); }
    .track { height: 6px; background: var(--fill-hover); }
    .track i { display: block; height: 100%; background: var(--status-idle); }
    .win.warn .track i { background: var(--status-permission); }
    .win.warn .pct { color: var(--status-permission-text); }
    .pct { text-align: right; color: var(--text-2); }
    @media (max-width: 700px) {
        .win { grid-template-columns: 64px minmax(0, 1fr) 40px; }
        .win .meta { grid-column: 1 / -1; }
        .tbl th, .tbl td { padding: 8px var(--space-2); }
    }
</style>
