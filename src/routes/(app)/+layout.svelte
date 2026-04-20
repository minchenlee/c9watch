<script lang="ts">
	import { onMount } from 'svelte';
	import { initializeSessionListeners, sessions } from '$lib/stores/sessions';
	import { initializeSubagentPolling } from '$lib/stores/subagents';
	import { initializeTasksPolling } from '$lib/stores/tasks';
	import { getSessions } from '$lib/api';
	import { loadDemoDataIfActive } from '$lib/demo';
	import { checkForUpdates } from '$lib/updater';
	import { isTauri } from '$lib/ws';

	onMount(async () => {
		// If demo mode was persisted, load demo data and skip real fetch
		const demoActive = loadDemoDataIfActive();

		// Desktop (Tauri): initialize listeners and fetch sessions here
		// Browser/mobile: ConnectionScreen handles initialization after user connects
		if (isTauri()) {
			await initializeSessionListeners();
			initializeSubagentPolling();
			initializeTasksPolling();

			if (!demoActive) {
				const initialSessions = await getSessions();
				sessions.set(initialSessions);
			}

			checkForUpdates();
		}
	});
</script>

<slot />
