<script lang="ts">
    // Glass root. G0 is an empty frame: a side bar, a page slot and the data
    // feeds. Design and tokens start in G1. Imports stay inside
    // src/lib/data/, src/lib/c9p-1.ts and src/shells/glass/.
    import { onMount } from 'svelte';
    import { route, routeHash, type GlassRoute } from './nav';
    import { startGlassData, sessions, agents, annotations } from './data';
    import { DEMO_WORKSPACES } from './model/gaps-demo';

    onMount(startGlassData);

    const pages: { label: string; route: GlassRoute }[] = [
        { label: 'Needs you', route: { page: 'needs' } },
        { label: 'Today', route: { page: 'today' } },
        ...DEMO_WORKSPACES.map(w => ({ label: w.name, route: { page: 'ws', id: w.id } as GlassRoute })),
        { label: 'Usage', route: { page: 'usage' } },
        { label: 'Settings', route: { page: 'settings' } }
    ];
    const pageKey = (r: GlassRoute) => routeHash(r).slice(2);
</script>

<div
    class="glass-root"
    data-page={pageKey($route)}
    data-sessions={$sessions.length}
    data-agents={$agents.length}
    data-annotations={$annotations.size}
>
    <nav class="glass-side" aria-label="Glass pages">
        {#each pages as p (pageKey(p.route))}
            <a href={routeHash(p.route)} aria-current={pageKey(p.route) === pageKey($route) ? 'page' : undefined}>{p.label}</a>
        {/each}
    </nav>
    <main class="glass-page" aria-label={pageKey($route)}></main>
</div>

<style>
    .glass-root {
        position: fixed;
        inset: 0;
        display: grid;
        grid-template-columns: 220px 1fr;
        background: #000;
        color: #ddd;
        font-family: system-ui, sans-serif;
        font-size: 13px;
    }
    .glass-side {
        display: flex;
        flex-direction: column;
        gap: 2px;
        padding: 12px 8px;
        border-right: 1px solid #222;
    }
    .glass-side a {
        padding: 6px 8px;
        color: inherit;
        text-decoration: none;
        border-radius: 6px;
    }
    .glass-side a[aria-current='page'] {
        background: #1a1a1a;
    }
</style>
