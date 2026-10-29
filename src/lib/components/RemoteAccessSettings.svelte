<script lang="ts">
	import { onMount } from 'svelte';
	import {
		getRemoteAccess,
		setRemoteAccess,
		getServerInfo,
		forgetPairedClients,
		type RemoteAccess
	} from '$lib/api';

	let remote = $state<RemoteAccess | null>(null);
	let hostsText = $state('');
	let addresses = $state<string[]>([]);
	let port = $state(9210);
	let busy = $state(false);
	let error = $state<string | null>(null);
	let savedHosts = $state(false);
	let forgetting = $state(false);
	let forgotten = $state(false);

	let hostsDirty = $derived(
		remote !== null && hostsText.trim() !== remote.extraAllowedHosts.join('\n')
	);

	async function refreshAddresses() {
		try {
			const info = await getServerInfo();
			addresses = info.lanAddresses;
			port = info.port;
		} catch {
			addresses = [];
		}
	}

	onMount(async () => {
		try {
			remote = await getRemoteAccess();
			hostsText = remote.extraAllowedHosts.join('\n');
			await refreshAddresses();
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		}
	});

	async function save(patch: Partial<RemoteAccess>): Promise<boolean> {
		if (!remote || busy) return false;
		busy = true;
		error = null;
		try {
			remote = await setRemoteAccess({ ...remote, ...patch });
			hostsText = remote.extraAllowedHosts.join('\n');
			await refreshAddresses();
			return true;
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
			return false;
		} finally {
			busy = false;
		}
	}

	async function forgetDevices() {
		if (forgetting) return;
		forgetting = true;
		error = null;
		try {
			await forgetPairedClients();
			forgotten = true;
			setTimeout(() => (forgotten = false), 2000);
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			forgetting = false;
		}
	}

	async function saveHosts() {
		const ok = await save({ extraAllowedHosts: hostsText.split('\n') });
		if (!ok) return;
		savedHosts = true;
		setTimeout(() => (savedHosts = false), 2000);
	}
</script>

<section class="remote-settings" aria-labelledby="remote-settings-title">
	<header>
		<h2 id="remote-settings-title">Remote access</h2>
		<p>Choose which devices can reach the c9watch server on port {port}.</p>
	</header>
	{#if !remote}
		{#if !error}<p class="state-msg">Loading…</p>{/if}
	{:else}
		<div class="setting-row">
			<span>LAN access<small>Off: only this Mac can connect. On: devices on your network can connect after pairing with a code from Connect Mobile.</small></span>
			<button
				class:active={remote.lanEnabled}
				aria-label="LAN access"
				aria-pressed={remote.lanEnabled}
				disabled={busy}
				onclick={() => save({ lanEnabled: !remote!.lanEnabled })}
			>{busy ? '…' : remote.lanEnabled ? 'On' : 'Off'}</button>
		</div>
		{#if remote.lanEnabled}
			<div class="warning">
				<strong>Not encrypted.</strong> LAN access uses plain <code>ws://</code>. Anyone on this network can read the traffic between a device and this Mac, including the pairing code, the device token and session content. Turn it on only on a network you trust, and turn it off when you are done.
			</div>
			<div class="addresses">
				<span class="label">This Mac's addresses</span>
				{#if addresses.length}
					<span class="mono">{addresses.join(' · ')} · port {port}</span>
				{:else}
					<span class="mono dim">No network address found</span>
				{/if}
			</div>
		{/if}
		<div class="setting-row">
			<span>Conversations over remote<small>Off: remote devices see session cards only. No transcripts, prompts, replies or pending tool input leave this Mac.</small></span>
			<button
				class:active={remote.allowConversation}
				aria-label="Conversations over remote"
				aria-pressed={remote.allowConversation}
				disabled={busy}
				onclick={() => save({ allowConversation: !remote!.allowConversation })}
			>{busy ? '…' : remote.allowConversation ? 'On' : 'Off'}</button>
		</div>
		<div class="setting-row">
			<span>Open terminal from remote<small>On: a remote device can bring a session's terminal or editor window to the front on this Mac.</small></span>
			<button
				class:active={remote.allowOpen}
				aria-label="Open terminal from remote"
				aria-pressed={remote.allowOpen}
				disabled={busy}
				onclick={() => save({ allowOpen: !remote!.allowOpen })}
			>{busy ? '…' : remote.allowOpen ? 'On' : 'Off'}</button>
		</div>
		<div class="hosts-group">
			<label for="remote-extra-hosts">Extra allowed hosts<small>Other host names this server answers to, one per line, as host or host:port. Leave empty unless you reach c9watch through a proxy you run.</small></label>
			<textarea
				id="remote-extra-hosts"
				rows="3"
				spellcheck="false"
				autocomplete="off"
				placeholder="mac.example.lan"
				bind:value={hostsText}
				disabled={busy}
			></textarea>
			<div class="hosts-actions">
				{#if savedHosts}<span class="saved">Saved</span>{/if}
				<button onclick={saveHosts} disabled={busy || !hostsDirty}>Save hosts</button>
			</div>
		</div>
		<div class="setting-row">
			<span>Paired devices<small>Disconnect every paired phone or browser now. Each one must pair again with a new code; a code shown now stops working. Restarting c9watch also forgets them.</small></span>
			<span class="row-actions">
				{#if forgotten}<span class="saved">Forgotten</span>{/if}
				<button aria-label="Forget paired devices" disabled={forgetting} onclick={forgetDevices}>{forgetting ? '…' : 'Forget'}</button>
			</span>
		</div>
		<p class="save-note">Changing a setting restarts the server. Connected devices reconnect.</p>
	{/if}
	{#if error}<p class="error" role="alert">{error}</p>{/if}
</section>

<style>
	.remote-settings { width: 100%; color: var(--text-primary); font: 14px/1.5 var(--font-sans); }
	header { margin-bottom: var(--space-xl); }
	h2 { margin: 0 0 var(--space-sm); font: 600 13px/1.5 var(--font-pixel); text-transform: uppercase; letter-spacing: .1em; }
	p { margin: 0; }
	header p, small, .save-note { color: var(--text-description); font-size: 13px; line-height: 1.55; }
	.state-msg { font: 13px var(--font-mono); text-transform: uppercase; color: var(--text-muted); text-align: center; padding: var(--space-xl) 0; }
	.setting-row { display: flex; align-items: center; justify-content: space-between; gap: var(--space-xl); padding: var(--space-lg) 0; border-bottom: 1px solid var(--border-default); }
	small { display: block; max-width: 52ch; margin-top: var(--space-xs); }
	button { flex-shrink: 0; min-width: 42px; padding: 4px var(--space-sm); border: 1px solid var(--border-default); border-radius: 0; background: transparent; color: var(--text-description); font: 11px/1.5 var(--font-pixel); text-transform: uppercase; letter-spacing: .05em; cursor: pointer; transition: color var(--transition-fast), background var(--transition-fast); }
	button:hover:not(:disabled) { background: rgba(255,255,255,.08); color: var(--text-primary); }
	button.active { background: rgba(255,255,255,.1); color: var(--text-primary); }
	button:disabled { opacity: .5; cursor: not-allowed; }
	button:focus-visible, textarea:focus-visible { outline: 1px solid var(--border-focus); outline-offset: 0; }
	.warning { margin-top: var(--space-lg); padding: var(--space-md) var(--space-lg); border: 1px solid color-mix(in srgb, var(--accent-amber) 40%, transparent); background: color-mix(in srgb, var(--accent-amber) 8%, transparent); color: var(--accent-amber); font-size: 13px; line-height: 1.55; }
	.warning strong { font-weight: 600; }
	code, .mono { font-family: var(--font-mono); font-size: 12px; }
	.addresses { display: flex; flex-wrap: wrap; align-items: baseline; justify-content: space-between; gap: var(--space-sm) var(--space-lg); padding: var(--space-lg) 0; border-bottom: 1px solid var(--border-default); }
	.label { font-size: 14px; }
	.mono { color: var(--text-primary); overflow-wrap: anywhere; }
	.dim { color: var(--text-muted); }
	.hosts-group { display: flex; flex-direction: column; gap: var(--space-sm); padding: var(--space-lg) 0; border-bottom: 1px solid var(--border-default); }
	textarea { width: 100%; box-sizing: border-box; resize: vertical; padding: var(--space-sm) var(--space-md); border: 1px solid var(--border-default); border-radius: 0; background: var(--bg-base); color: var(--text-primary); font: 12px/1.6 var(--font-mono); }
	textarea::placeholder { color: var(--text-muted); }
	.row-actions { display: flex; align-items: center; gap: var(--space-md); flex-shrink: 0; }
	.hosts-actions { display: flex; align-items: center; justify-content: flex-end; gap: var(--space-md); }
	.saved { font: 11px var(--font-mono); color: var(--accent-green); text-transform: uppercase; letter-spacing: .1em; }
	.save-note { padding-top: var(--space-lg); }
	.error { margin-top: var(--space-md); color: var(--accent-red); font-size: 14px; overflow-wrap: anywhere; }
	@media(max-width: 640px) { .setting-row { align-items: flex-start; flex-wrap: wrap; gap: var(--space-md); } }
	@media(prefers-reduced-motion: reduce) { button { transition: none; } }
</style>
