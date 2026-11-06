<script lang="ts">
    import '../app.css';
    import { onDestroy } from 'svelte';
    import { page } from '$app/stores';
    import { shell, initializeSettings } from '$lib/data/stores/settings';
    import Glass from '../shells/glass/Shell.svelte';
    // Read `ui.shell` before the first render (ssr is off), so a persisted
    // glass never mounts Classic first and leaves its feeds running.
    onDestroy(initializeSettings());
    // The menu bar popover window is always Classic, in either shell.
    const popover = $page.url.pathname.startsWith('/popover');
    // Classic's session listeners have no teardown, so once Classic ran in
    // this window it stays until a reload. Glass stops its own feeds when it
    // unmounts, so glass → classic switches at once.
    let classicStarted = false;
    $: if (popover || $shell === 'classic') classicStarted = true;
    $: glass = !classicStarted && $shell === 'glass';
</script>

{#if glass}<Glass />{:else}<slot />{/if}
