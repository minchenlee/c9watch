<script lang="ts">
    // The glass side bar (redesign §8.7–8.10, R7-SPEC §C.1, owner D40):
    // brand + collapse, ⌘N / ⌘K, 需要你 → 今天 → workspaces with lead and
    // talk rows → extension pages → 用量 / 設定. v1 has no goal rows and no
    // office entry. Extension pages (G6): first-party first (the office's
    // place), then third-party, all before 用量.
    import { route, routeHash, type GlassRoute } from './nav';
    import { agents, fixtureOn } from './data';
    import { DEMO_WORKSPACES } from './model/gaps-demo';
    import type { C9pAgentRow } from '$lib/c9p-1';
    import { extensions } from './ext/registry';
    import { sidebarPages } from './ext/manifest';

    export let needs: number;
    export let onCollapse: () => void;
    export let onDelegate: () => void;
    export let onSearch: () => void;

    type Row = { id: string; label: string; meta: string };
    type Workspace = { id: string; name: string; rows: Row[] };

    // Lead rows (an orch with no parent) and talk rows (observed sessions).
    // Fixture only: the derivation from live c9p rows is G2's model.
    function rowsFor(all: C9pAgentRow[], root: string): Row[] {
        const here = all.filter(a => a.cwd === root);
        const leads = here.filter(a => a.role === 'orch' && !a.parentId).map((a): Row => {
            const kids = all.filter(k => k.parentId === a.id);
            const done = kids.length > 0 && kids.every(k => k.goneAt !== null);
            return { id: a.id, label: a.name ?? a.id, meta: done ? '完成' : `${kids.length} 個 agent` };
        });
        const talks = here.filter(a => a.role === 'observed').map((a): Row => ({ id: a.id, label: `對談 · ${a.name ?? a.id}`, meta: '' }));
        return [...leads, ...talks];
    }
    $: workspaces = $fixtureOn
        ? DEMO_WORKSPACES.map((w): Workspace => ({ id: w.id, name: w.name, rows: rowsFor($agents, w.root) }))
        : [];

    $: extPages = sidebarPages($extensions);

    const key = (r: GlassRoute) => routeHash(r).slice(2);
    $: current = key($route);
    const here = (r: GlassRoute, cur: string) => (key(r) === cur ? 'page' : undefined);
</script>

<nav class="glass-side" aria-label="主選單">
    <div class="brand">
        <span>c9watch</span>
        <button type="button" class="icon" on:click={onCollapse} aria-label="收起左欄" title="收起左欄 [">⊟</button>
    </div>
    <div class="cmds">
        <button type="button" class="cmd" on:click={onDelegate}>＋ 交辦<kbd>⌘N</kbd></button>
        <button type="button" class="cmd" on:click={onSearch}>搜尋<kbd>⌘K</kbd></button>
    </div>
    <div class="grp">
        <a class="ni" href={routeHash({ page: 'needs' })} aria-current={here({ page: 'needs' }, current)} data-nav="needs">
            <span class="need-dot" class:on={needs > 0} aria-hidden="true"></span>需要你
            <span class="need-n" class:on={needs > 0}>{needs > 0 ? needs : '0 · 沒有事'}</span>
        </a>
        <a class="ni" href={routeHash({ page: 'today' })} aria-current={here({ page: 'today' }, current)}>今天</a>
    </div>
    <div class="workspaces" aria-label="Workspaces">
        {#each workspaces as w (w.id)}
            {@const r = { page: 'ws', id: w.id } as const}
            <div class="grp">
                <a class="cap" href={routeHash(r)} aria-current={here(r, current)}>{w.name} ›</a>
                {#each w.rows as row (row.id)}
                    <a class="ni" href={routeHash(r)}><span class="name">{row.label}</span>{#if row.meta}<span class="meta">{row.meta}</span>{/if}</a>
                {/each}
            </div>
        {:else}
            <p class="state-msg">還沒有 workspace</p>
        {/each}
    </div>
    <div class="foot">
        <div class="ext-slot" data-slot="extensions">{#each extPages as x (`${x.extId}/${x.page.id}`)}{@const r = { page: 'ext', extId: x.extId, pageId: x.page.id } as const}<a class="ni" href={routeHash(r)} aria-current={here(r, current)} data-ext={x.extId} data-source={x.source}><span class="name">{x.page.title}</span></a>{/each}</div>
        <a class="ni" href={routeHash({ page: 'usage' })} aria-current={here({ page: 'usage' }, current)}>用量</a>
        <a class="ni" href={routeHash({ page: 'settings' })} aria-current={here({ page: 'settings' }, current)}>設定</a>
    </div>
</nav>

<style>
    .glass-side {
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
        min-height: 0;
        overflow: auto;
        padding: var(--space-3) 10px;
        border-right: 1px solid var(--divider);
        background: var(--surface-window);
    }
    .brand {
        display: flex;
        align-items: center;
        justify-content: space-between;
        padding-left: 10px;
        font: var(--type-meta);
        font-size: 14px;
        letter-spacing: 0.08em;
        color: var(--text-1);
    }
    button {
        font: var(--type-label);
        color: var(--text-2);
        background: none;
        border: 0;
        cursor: pointer;
    }
    .icon {
        width: 28px;
        height: 28px;
        border-radius: var(--radius-sm);
        font-size: 14px;
    }
    .icon:hover, .cmd:hover, .ni:hover { background: var(--fill-hover); }
    .cmds { display: flex; gap: 6px; }
    .cmd {
        flex: 1;
        display: flex;
        align-items: center;
        justify-content: space-between;
        min-height: 32px;
        padding: 0 10px;
        border: 1px solid var(--glass-stroke);
        border-radius: var(--radius-md);
        background: var(--surface-1);
        font-size: 13px;
    }
    .grp, .workspaces, .foot, .ext-slot { display: flex; flex-direction: column; gap: 1px; }
    .workspaces { gap: var(--space-3); }
    .foot { margin-top: auto; }
    .ni, .cap {
        display: flex;
        align-items: center;
        gap: var(--space-2);
        padding: 6px 10px;
        border-radius: 6px;
        color: var(--text-2);
        text-decoration: none;
    }
    .ni { font: var(--type-body); font-size: 13.5px; line-height: 20px; }
    .cap { font: var(--type-meta); font-size: 12px; letter-spacing: 0.08em; }
    .cap:hover { color: var(--text-1); }
    [aria-current='page'] { background: var(--fill-active); color: var(--text-1); font-weight: 600; }
    .name { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .meta { font: var(--type-label); font-weight: 400; color: var(--text-3); }
    .ni .need-n { margin-left: auto; font-size: 12px; }
    .state-msg { padding: var(--space-3) 10px; }
    @media (max-width: 1100px) {
        .glass-side { padding-inline: var(--space-2); }
    }
</style>
