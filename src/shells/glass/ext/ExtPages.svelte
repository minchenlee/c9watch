<script lang="ts">
    // The `#/ext/<extId>/<pageId>` route (G6). It stays mounted so the last
    // extension page lives on while another glass page shows: leaving sends
    // `suspend`, coming back sends `resume`, another extension page
    // replaces it (`destroy`). It also reads the extension list once.
    import { onMount } from 'svelte';
    import tokensCss from '../tokens.css?raw';
    import { transport } from '$lib/data/client';
    import type { Transport } from '$lib/data/transport';
    import { navigate, parseHash, routeHash, type GlassRoute } from '../nav';
    import { forwardKey, usePageKeys } from '../keys';
    // The mode .glass-root shows (Reduce Transparency forces off, G10b).
    import { effectiveGlass as glassMode } from '../native';
    import { model } from '../data';
    import type { HostContext, HostTheme } from './host';
    import { extensions, extensionsLoaded, loadExtensions } from './registry';
    import Mount from './Mount.svelte';

    export let route: GlassRoute;

    onMount(() => { void loadExtensions(); });

    type Target = { extId: string; pageId: string };
    let target: Target | null = null;
    $: showing = route.page === 'ext';
    $: if (route.page === 'ext' && (route.extId !== target?.extId || route.pageId !== target?.pageId)) target = { extId: route.extId, pageId: route.pageId };
    $: entry = target ? $extensions.find(e => e.dir === target!.extId) : undefined;
    $: page = entry?.status === 'ok' ? entry.manifest?.ui?.pages.find(p => p.id === target!.pageId) : undefined;
    $: title = page?.title ?? entry?.manifest?.name ?? target?.extId ?? '';

    // Theme: token values read from .glass-root, for SVG and canvas code.
    // CSS inside the shadow root inherits the custom properties directly.
    const TOKEN_NAMES = [...new Set([...tokensCss.matchAll(/^\s*(--[\w-]+)\s*:/gm)].map(m => m[1]))];
    let el: HTMLElement;
    function readTheme(mode: HostTheme['glass']): HostTheme {
        const style = el?.closest('.glass-root') ? getComputedStyle(el.closest('.glass-root')!) : null;
        const tokens: Record<string, string> = {};
        for (const n of TOKEN_NAMES) tokens[n] = style?.getPropertyValue(n).trim() ?? '';
        return Object.freeze({ tokens: Object.freeze(tokens), glass: mode });
    }
    let theme: HostTheme = { tokens: {}, glass: $glassMode };
    // Wait one frame after a mode change: data-glass on .glass-root changes first.
    $: { const m = $glassMode; if (el) requestAnimationFrame(() => (theme = readTheme(m))); }
    onMount(() => { theme = readTheme($glassMode); });

    // Context. v1 fills needsItemId only: J / K on an extension page step
    // through the needs queue (the host owns J / K, D33 Q9).
    let context: HostContext = {};
    function step(d: 1 | -1) {
        const ids = $model.needs.map(n => n.id);
        if (!ids.length) return;
        const i = context.needsItemId ? ids.indexOf(context.needsItemId) : -1;
        const next = i < 0 ? (d > 0 ? 0 : ids.length - 1) : Math.min(ids.length - 1, Math.max(0, i + d));
        if (ids[next] !== context.needsItemId) context = { ...context, needsItemId: ids[next] };
    }
    let offKeys: (() => void) | null = null;
    $: if (showing && !offKeys) offKeys = usePageKeys({ next: () => step(1), prev: () => step(-1) });
    $: if (!showing && offKeys) { offKeys(); offKeys = null; }
    onMount(() => () => offKeys?.());

    // The host's transport, picked per call: demo mode is set by the shell's
    // onMount, after this component starts.
    const c9p: Transport = {
        request: (method, params, options) => transport().request(method, params, options),
        subscribe: (params, onFrame) => transport().subscribe(params, onFrame)
    };
    const deps = {
        c9p,
        // Only hashes that are a glass page: the round trip must match.
        navigate(hash: string): boolean {
            const r = parseHash(hash);
            if (routeHash(r) !== hash) return false;
            navigate(r);
            return true;
        },
        forwardKey: (e: KeyboardEvent) => { forwardKey(e); }
    };
</script>

<main class="ext-page" aria-label={title} hidden={!showing} bind:this={el} data-ext={target ? `${target.extId}/${target.pageId}` : undefined}>
    <header><h1>{title}</h1></header>
    {#if target && entry && page}
        {#key `${entry.source}/${entry.dir}/${page.id}`}
            <Mount {entry} {page} suspended={!showing} {context} {theme} {deps} />
        {/key}
    {:else if target && entry && entry.status !== 'ok'}
        <p class="state-msg" role="status">{entry.reason ?? '這個 extension 不能載入'}</p>
    {:else if target && !$extensionsLoaded}
        <p class="state-msg">載入中…</p>
    {:else if target}
        <p class="state-msg">沒有這個 extension 頁面</p>
    {/if}
</main>

<style>
    /* Same box as the shell's .glass-page (a different class, so the shell's
     * own page selectors never match this one). */
    .ext-page {
        min-width: 0;
        overflow: auto;
        padding: var(--space-5) var(--space-8);
    }
    .ext-page[hidden] { display: none; }
    :global(.collapsed) > .ext-page { padding-top: 56px; }
    h1 { margin: 0 0 var(--space-6); font: var(--type-title); color: var(--text-1); }
</style>
