<script lang="ts">
	import { onMount } from 'svelte';
	import { invoke } from '@tauri-apps/api/core';
	type ConnectionStatus = { url: string; connected: boolean; error: string | null; sessionCount: number };
	let url = $state('http://127.0.0.1:4096');
	let username = $state('opencode');
	let password = $state('');
	let status = $state<ConnectionStatus | null>(null);
	let error = $state('');
	let busy = $state(false);
	let generation = 0;
	onMount(() => {
		let disposed = false;
		let first = true;
		let timer: ReturnType<typeof setTimeout>;
		async function refresh() {
			const requestGeneration = generation;
			try {
				if (busy) return;
				const next = await invoke<ConnectionStatus>('opencode_connection_status');
				if (disposed || requestGeneration !== generation) return;
				status = next;
				if (first && next.url) url = next.url;
				first = false;
			} catch (e) { if (!disposed && requestGeneration === generation) error = String(e); }
			finally { if (!disposed) timer = setTimeout(refresh, 2000); }
		}
		void refresh();
		return () => { disposed = true; generation++; clearTimeout(timer); };
	});
	async function connect(disconnect = false) {
		if (busy) return;
		const requestGeneration = ++generation;
		busy = true;
		error = '';
		try {
			await invoke('opencode_connect', { url: disconnect ? '' : url, username, password: password || null });
			password = '';
			const next = await invoke<ConnectionStatus>('opencode_connection_status');
			if (requestGeneration === generation) status = next;
		} catch (e) { if (requestGeneration === generation) error = String(e); }
		finally { if (requestGeneration === generation) busy = false; }
	}
</script>

<section aria-labelledby="opencode-heading">
	<h3 id="opencode-heading">OpenCode <span>PREVIEW</span></h3>
	<p>Connect to the server used by your OpenCode session. For a terminal session, start it with <code>opencode --port 4096</code>.</p>
	<form onsubmit={(event) => { event.preventDefault(); void connect(); }}>
		<label>Server URL <input bind:value={url} type="url" required placeholder="http://127.0.0.1:4096" disabled={busy} /></label>
		<div class="credentials">
			<label>Username <input bind:value={username} autocomplete="off" disabled={busy} /></label>
			<label>Password (if required) <input bind:value={password} type="password" autocomplete="off" disabled={busy} /></label>
		</div>
		<div class="actions">
			<button type="submit" disabled={busy}>{busy ? 'CONNECTING…' : 'CONNECT'}</button>
			{#if status?.url}<button type="button" disabled={busy} onclick={() => connect(true)}>DISCONNECT</button>{/if}
		</div>
	</form>
	<p class="status" role="status">{error || status?.error || (status?.connected ? `Connected · ${status.sessionCount} sessions` : status?.url ? 'Connecting…' : 'Not connected')}</p>
	<p class="hint">Connection settings apply until c9watch quits. Sessions refresh every two seconds. OpenCode controls and usage totals are not included in this preview.</p>
</section>

<style>
	section { border: 1px solid var(--border-default); background: var(--bg-card); padding: 16px; border-radius: var(--radius-sm); }
	h3 { margin: 0 0 8px; font-size: 14px; color: var(--text-primary); }
	h3 span { margin-left: 6px; font: 9px var(--font-mono); color: var(--text-muted); }
	p { margin: 8px 0; color: var(--text-secondary); font-size: 12px; line-height: 1.5; }
	code { font-size: 11px; }
	label { display: block; font-size: 11px; color: var(--text-secondary); }
	input { box-sizing: border-box; width: 100%; margin: 5px 0 10px; padding: 8px; background: var(--bg-elevated); color: var(--text-primary); border: 1px solid var(--border-default); border-radius: var(--radius-sm); font: 12px var(--font-mono); }
	.credentials { display: grid; grid-template-columns: 1fr 1fr; gap: 12px; }
	.actions { display: flex; gap: 8px; }
	button { padding: 8px 12px; border: 1px solid var(--border-default); border-radius: var(--radius-sm); background: var(--bg-elevated); color: var(--text-primary); font: 11px var(--font-mono); cursor: pointer; }
	button:disabled { opacity: .5; cursor: default; }
	.status { color: var(--text-primary); }
	.hint { color: var(--text-muted); font-size: 11px; }
</style>
