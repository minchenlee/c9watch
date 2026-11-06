<script lang="ts">
    // Glass root: tokens, the side bar, the keyboard contract and an empty
    // shell for each page. Imports stay inside src/lib/data/,
    // src/lib/c9p-1.ts and src/shells/glass/.
    import './tokens.css';
    import { onMount } from 'svelte';
    import { route, routeHash, type GlassRoute } from './nav';
    import { startGlassData, sessions, agents, annotations, fixtureOn, model } from './data';
    import type { WorkspaceView } from './model';
    import { DEMO_NEEDS, DEMO_WORKSPACES } from './model/gaps-demo';
    import { installKeys } from './keys';
    import { glassMode, sidebarCollapsed } from './prefs';
    import Sidebar from './Sidebar.svelte';
    import ExtPages from './ext/ExtPages.svelte';

    // startGlassData returns its stop function, which Svelte runs on unmount.
    onMount(startGlassData);

    // Until the needs model (G2): the fixture's 5 items, or live sessions
    // that wait for a person.
    $: needs = $fixtureOn ? DEMO_NEEDS.length : $sessions.filter(s => s.status === 'NeedsAttention').length;

    // ⌘N and ⌘K show a hint. v1 has no delegate composer (owner D40) and
    // search comes later.
    let hint: 'delegate' | 'search' | null = null;
    const toggleSidebar = () => sidebarCollapsed.update(v => !v);
    onMount(() => installKeys(window, {
        search: () => (hint = 'search'),
        delegate: () => (hint = 'delegate'),
        sidebar: toggleSidebar,
        escape: () => { if (!hint) return false; hint = null; return true; }
    }));

    type PageInfo = { title: string; empty: string };
    // Workspace title: the model's name (demo names only while the fixture is
    // on), then the fixture list while it is on, then the route id. Live mode
    // never shows a fixture name.
    function wsTitle(id: string, fixture: boolean, live: WorkspaceView[]): string {
        return live.find(w => w.id === id)?.name ?? (fixture ? DEMO_WORKSPACES.find(w => w.id === id)?.name : undefined) ?? id;
    }
    function pageInfo(r: GlassRoute, fixture: boolean, live: WorkspaceView[]): PageInfo {
        switch (r.page) {
            case 'needs': return { title: '需要你', empty: '需要你處理的事會出現在這裡' };
            case 'today': return { title: '今天', empty: '你離開之後發生的事會出現在這裡' };
            case 'ws': return { title: wsTitle(r.id, fixture, live), empty: '這個 workspace 的 agent 會出現在這裡' };
            case 'ext': return { title: r.extId, empty: '沒有這個 extension 頁面' };
            case 'usage': return { title: '用量', empty: '花費和 token 用量會出現在這裡' };
            case 'settings': return { title: '設定', empty: '設定會出現在這裡' };
        }
    }
    $: info = pageInfo($route, $fixtureOn, $model.workspaces);
    const pageKey = (r: GlassRoute) => routeHash(r).slice(2);
</script>

<div
    class="glass-root"
    class:collapsed={$sidebarCollapsed}
    data-glass={$glassMode}
    data-page={pageKey($route)}
    data-sessions={$sessions.length}
    data-agents={$agents.length}
    data-annotations={$annotations.size}
>
    {#if $sidebarCollapsed}
        <button type="button" class="reopen" on:click={toggleSidebar} aria-label="打開左欄，需要你 {needs} 件" title="打開左欄 [">
            <span class="need-dot" class:on={needs > 0} aria-hidden="true"></span>需要你 · <span class="need-n" class:on={needs > 0}>{needs}</span>
        </button>
    {:else}
        <Sidebar {needs} onCollapse={toggleSidebar} onDelegate={() => (hint = 'delegate')} onSearch={() => (hint = 'search')} />
    {/if}
    {#if $route.page !== 'ext'}
        <main class="glass-page" aria-label={info.title}>
            <header><h1>{info.title}</h1></header>
            <p class="state-msg">{info.empty}</p>
        </main>
    {/if}
    <!-- Extension pages (G6). Always mounted: it keeps the last one alive. -->
    <ExtPages route={$route} />
    {#if hint}
        <div class="hint glass-float" role="status">
            {#if hint === 'delegate'}
                <p>用 <code>c9watch spawn</code> 交辦</p>
                <p class="sub">這一版的視窗還不能交辦，請在終端機執行。</p>
            {:else}
                <p>搜尋還沒做好</p>
                <p class="sub">之後的版本會加上。</p>
            {/if}
            <button type="button" on:click={() => (hint = null)}>關閉 <kbd>Esc</kbd></button>
        </div>
    {/if}
</div>

<style>
    .glass-root {
        position: fixed;
        inset: 0;
        display: grid;
        grid-template-columns: 240px minmax(0, 1fr);
        background: var(--surface-window);
        color: var(--text-1);
        font: var(--type-body);
    }
    .glass-root.collapsed { grid-template-columns: minmax(0, 1fr); }
    @media (max-width: 1100px) {
        .glass-root { grid-template-columns: 200px minmax(0, 1fr); }
    }
    .glass-page {
        min-width: 0;
        overflow: auto;
        padding: var(--space-5) var(--space-8);
    }
    .collapsed .glass-page { padding-top: 56px; }
    h1 { margin: 0 0 var(--space-6); font: var(--type-title); color: var(--text-1); }
    .reopen {
        position: absolute;
        top: var(--space-3);
        left: var(--space-3);
        display: flex;
        align-items: center;
        gap: var(--space-2);
        min-height: 32px;
        padding: 0 var(--space-3);
        border: 1px solid var(--glass-stroke);
        border-radius: var(--radius-md);
        background: var(--surface-1);
        font: var(--type-label);
        font-size: 13px;
        color: var(--text-2);
        cursor: pointer;
    }
    .reopen:hover { background: var(--surface-1-hover); }
    .hint {
        position: absolute;
        bottom: var(--space-8);
        left: 50%;
        transform: translateX(-50%);
        width: min(420px, calc(100% - 32px));
        padding: var(--space-4) var(--space-5);
    }
    .hint p { margin: 0; }
    .hint .sub { margin-top: var(--space-1); font: var(--type-body); color: var(--text-2); }
    .hint code { font: var(--type-meta); color: var(--text-1); }
    .hint button {
        margin-top: var(--space-3);
        padding: var(--space-1) var(--space-2);
        border: 1px solid var(--glass-stroke);
        border-radius: var(--radius-sm);
        background: none;
        font: var(--type-label);
        color: var(--text-2);
        cursor: pointer;
    }
</style>
