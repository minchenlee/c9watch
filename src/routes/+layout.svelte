<script lang="ts">
    import '../app.css';
    import { onMount } from 'svelte';
    import { shell, initializeSettings } from '$lib/data/stores/settings';
    import Glass from '../shells/glass/Shell.svelte';
    onMount(initializeSettings);
</script>

{#if $shell === 'glass'}<Glass />{:else}<slot />{/if}
