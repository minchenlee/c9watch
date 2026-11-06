<script lang="ts">
    import '../app.css';
    import { onMount } from 'svelte';
    import { page } from '$app/stores';
    import { shell, initializeSettings } from '$lib/data/stores/settings';
    import Glass from '../shells/glass/Shell.svelte';
    onMount(initializeSettings);
    // The menu bar popover window is always Classic, in either shell.
    $: popover = $page.url.pathname.startsWith('/popover');
</script>

{#if $shell === 'glass' && !popover}<Glass />{:else}<slot />{/if}
