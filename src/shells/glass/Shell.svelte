<script lang="ts">
    // Glass root. G0 is an empty frame: a side bar, a page slot and the data
    // feeds. Design and tokens start in G1. Imports stay inside
    // src/lib/data/, src/lib/c9p-1.ts and src/shells/glass/.
    import { onMount } from 'svelte';
    import { route, routeHash, type GlassRoute } from './nav';
    import { startGlassData, sessions, agents, annotations, fixtureOn } from './data';
    import { DEMO_WORKSPACES } from './model/gaps-demo';

    // startGlassData returns its stop function, which Svelte runs on unmount.
    onMount(startGlassData);

    type Link = { label: string; route: GlassRoute };
    const top: Link[] = [{ label: 'Needs you', route: { page: 'needs' } }, { label: 'Today', route: { page: 'today' } }];
    const bottom: Link[] = [{ label: 'Usage', route: { page: 'usage' } }, { label: 'Settings', route: { page: 'settings' } }];
    // Demo workspaces only on the glass fixture. Live workspaces need the
    // derivation from c9p rows (G2); until then live mode lists none.
    $: workspaces = $fixtureOn ? DEMO_WORKSPACES.map((w): Link => ({ label: w.name, route: { page: 'ws', id: w.id } })) : [];
    const pageKey = (r: GlassRoute) => routeHash(r).slice(2);
</script>

{#snippet link(p: Link)}
    <a href={routeHash(p.route)} aria-current={pageKey(p.route) === pageKey($route) ? 'page' : undefined}>{p.label}</a>
{/snippet}

<div
    class="glass-root"
    data-page={pageKey($route)}
    data-sessions={$sessions.length}
    data-agents={$agents.length}
    data-annotations={$annotations.size}
>
    <nav class="glass-side" aria-label="Glass pages">
        {#each top as p (pageKey(p.route))}{@render link(p)}{/each}
        <div class="glass-workspaces" aria-label="Workspaces">
            {#each workspaces as p (pageKey(p.route))}{@render link(p)}{:else}<p class="state-msg">No workspaces yet</p>{/each}
        </div>
        {#each bottom as p (pageKey(p.route))}{@render link(p)}{/each}
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
    .glass-workspaces {
        display: flex;
        flex-direction: column;
        gap: 2px;
    }
    .state-msg {
        margin: 0;
        padding: 12px 8px;
        font-family: var(--font-mono);
        font-size: 13px;
        text-transform: uppercase;
        color: var(--text-muted);
        text-align: center;
    }
    .glass-side a[aria-current='page'] {
        background: #1a1a1a;
    }
</style>
