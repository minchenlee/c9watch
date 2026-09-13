<script module lang="ts">
	import { SvelteMap } from 'svelte/reactivity';
	type Attachment = { name: string; url: string };
	type Draft = { images?: Attachment[]; text: string; pending: boolean; notice: string; unknown: boolean };
	// Bounded, provider-qualified drafts survive closing/reopening a detail view.
	const drafts = new SvelteMap<string, Draft>();
	const stops = new SvelteMap<string, { status: string; notice: string }>();
</script>

<script lang="ts">
	import { onMount } from 'svelte';
	import { invoke } from '@tauri-apps/api/core';
	import { codexInteractions, hasCodexThread, refreshCodexInteractions } from '$lib/stores/codex-interactions';
	import CodexDesktopSupport from './CodexDesktopSupport.svelte';
	let { sessionId }: { sessionId: string } = $props();
	const key = $derived(`codex:${sessionId}`);
	const empty: Draft = { text: '', pending: false, notice: '', unknown: false };
	let draft = $derived(drafts.get(key) ?? empty);
	let checking = $state(true);
	let checkedAt = $state('');
	let available = $state(false);
	let reason = $state('Checking connection…');
	let disposed = false;
	let showSetup = $state(false);
	let attaching = $state(false);
	let fileInput = $state<HTMLInputElement>(undefined!);
	const draftFull = $derived(!drafts.has(key) && drafts.size >= 20 && ![...drafts.values()].some(value => !value.pending && !value.unknown && !value.text.length && !value.images?.length));
	const tooLong = $derived(new TextEncoder().encode(draft.text).length > 32768);

 const relevant = $derived($codexInteractions.filter(s => hasCodexThread(s, sessionId)));
 const live = $derived(relevant.filter(s => s.connected));
 const snapshot = $derived(live.length === 1 ? live[0] : relevant[0]);
 const turn = $derived(snapshot?.turns?.[sessionId]);
 const stopKey = $derived(`${snapshot?.endpoint}:${sessionId}:${turn?.turnId}`);
 const stopState = $derived(stops.get(stopKey));
 const active = $derived(turn?.status === 'inProgress');
 const waiting = $derived(snapshot?.statuses[sessionId] === 'waiting' || snapshot?.pending.some(p => p.threadId === sessionId && !p.submitted));
 const stopping = $derived(active && (turn?.stopping || ['sending', 'submitted'].includes(stopState?.status ?? '')));
 const hasContent = $derived(!!draft.text.trim() || !!draft.images?.length);
 const stopLocked = $derived(['sending', 'submitted', 'unknown'].includes(stopState?.status ?? ''));
 const stopDisabled = $derived(live.length !== 1 || !snapshot?.connected || !active || !!turn?.stopping || stopLocked);
 const turnLabel = $derived(live.length > 1 ? 'Multiple connections · check Codex' : snapshot && !snapshot.connected ? 'Disconnected · status may be outdated' : stopping ? 'Stopping…' : active && stopState?.status === 'unknown' ? 'Stop delivery unknown · check Codex' : waiting ? 'Waiting for your response' : active ? 'Running' : turn?.status === 'failed' ? 'Failed' : turn?.status === 'interrupted' ? 'Stopped' : available ? 'Ready' : 'Disconnected');
 async function stop() {
  if (stopDisabled || !turn || !snapshot || ['sending', 'submitted', 'unknown'].includes(stops.get(stopKey)?.status ?? '')) return;
  if (stops.size >= 1024) {
   update({notice: 'Stop history is full. Stop this turn in Codex, then restart c9watch to reset the local history.'});
   return;
  }
  const target = stopKey;
  stops.set(target, {status: 'sending', notice: ''});
  try {
   const receipt = await invoke<{status: string; detail: string}>('interrupt_codex_turn', {endpoint: snapshot.endpoint, threadId: sessionId, turnId: turn.turnId});
   stops.set(target, {status: receipt.status, notice: receipt.detail});
  } catch { stops.set(target, {status: 'unknown', notice: 'Stop delivery is unknown. Check the current turn in Codex.'}); }
  finally { void refreshCodexInteractions(); }
 }

	function autosize(node: HTMLTextAreaElement) {
		$effect(() => {
			draft.text;
			node.style.height = 'auto';
			node.style.height = `${Math.min(node.scrollHeight, 140)}px`;
		});
	}

	async function attach(files: File[]) {
		if (draftFull || attaching || draft.pending || draft.unknown) return;
		const target = key;
		attaching = true;
		try {
			const images = [...(draft.images ?? [])];
			for (const file of files) {
				if (!['image/png', 'image/jpeg', 'image/webp'].includes(file.type)) throw new Error('Choose PNG, JPEG or WebP images.');
				if (images.length >= 4 || file.size > 4 * 1024 * 1024) throw new Error('Up to four images, 4 MiB total.');
				const url = await new Promise<string>((resolve, reject) => {
					const reader = new FileReader();
					reader.onload = () => resolve(String(reader.result));
					reader.onerror = () => reject(new Error('Could not read image.'));
					reader.readAsDataURL(file);
				});
				images.push({ name: file.name || 'Pasted image', url });
				const size = images.reduce((n, image) => n + image.url.split(',')[1].length * 3 / 4, 0);
				if (size > 4 * 1024 * 1024) throw new Error('Images exceed 4 MiB total.');
			}
			await invoke('validate_codex_images', { images: images.map(image => image.url) });
			const otherBytes = [...drafts].filter(([id]) => id !== target).reduce((n, [, d]) => n + (d.images ?? []).reduce((sum, image) => sum + image.url.length, 0), 0);
			if (otherBytes + images.reduce((n, image) => n + image.url.length, 0) > 16 * 1024 * 1024) throw new Error('Image drafts are full. Remove images from another draft.');
			update({ images, notice: '' }, target);
		} catch (error) { update({ notice: String(error) }, target); }
		finally { attaching = false; }
	}

	function paste(event: ClipboardEvent) {
		const files = Array.from(event.clipboardData?.items ?? []).filter(item => item.kind === 'file' && item.type.startsWith('image/')).map(item => item.getAsFile()).filter((file): file is File => !!file);
		if (files.length) { event.preventDefault(); void attach(files); }
	}

	function update(patch: Partial<Draft>, target = key) {
		if (!drafts.has(target) && drafts.size >= 20) {
			const candidate = [...drafts].find(([, value]) => !value.pending && !value.unknown && !value.text.length && !value.images?.length);
			if (candidate) drafts.delete(candidate[0]);
			else return false;
		}
		drafts.set(target, { ...(drafts.get(target) ?? empty), ...patch });
		return true;
	}

	async function check() {
		checking = true;
		try {
			const result = await invoke<{ available: boolean; reason: string | null }>('codex_message_capability', { sessionId });
			if (!disposed) { available = result.available; reason = result.reason ?? ''; }
		} catch {
			if (!disposed) { available = false; reason = 'Connection check failed. Try again.'; }
		} finally { if (!disposed) { checking = false; checkedAt = new Date().toLocaleTimeString(); } }
	}
	onMount(() => { void check(); return () => { disposed = true; }; });


	async function send() {
		if (!available || checking || attaching || draft.pending || draft.unknown || tooLong || (!draft.text.trim() && !draft.images?.length)) return;
		const target = key;
		const id = sessionId;
		const text = draft.text;
		if (!update({ pending: true, notice: 'Sending…' }, target)) return;
		try {
			// Keep the observed turn identity even if its interaction socket went
			// stale. The backend rechecks unique loaded ownership; a stale steer
			// must reject, never silently become turn/start for a successor.
			const receipt = await invoke<{ status: string; detail: string }>('send_codex_message', { sessionId: id, text, ...(active && turn ? { expectedTurnId: turn.turnId } : {}), ...(draft.images?.length ? { images: draft.images.map(image => image.url) } : {}) });
			update({ pending: false, notice: receipt.detail, unknown: receipt.status === 'unknown',
				...((receipt.status === 'accepted' || receipt.status === 'queued') ? { text: '', images: [] } : {}) }, target);
		} catch (error) {
			// The IPC channel itself may have failed after the backend sent the request.
			update({ pending: false, unknown: true, notice: `${String(error)} — check the original session before retrying.` }, target);
		}
	}
</script>

	<section class="composer" aria-label="Send a message to Codex">

	{#if checking || !available}
		<div class="connection"><span>{checking ? 'Checking connection…' : reason}</span><button onclick={check} disabled={checking}>RECHECK</button></div>
		{#if !checking && checkedAt}<p role="status">Checked at {checkedAt} · Not connected to this task</p>{/if}
			{#if !checking}<button class="setup-toggle" onclick={() => showSetup = !showSetup} aria-expanded={showSetup}>DESKTOP SUPPORT</button>{/if}
			{#if showSetup}
				<CodexDesktopSupport />
			{/if}
		{:else}
			<input class="file-input" bind:this={fileInput} type="file" accept="image/png,image/jpeg,image/webp" multiple onchange={(event) => { void attach(Array.from(event.currentTarget.files ?? [])); event.currentTarget.value = ''; }} />
			{#if draft.images?.length}<div class="attachments">{#each draft.images as image, i}<div class="attachment"><img src={image.url} alt={image.name} /><button title={`Remove ${image.name}`} aria-label={`Remove ${image.name}`} disabled={draftFull || attaching || draft.pending || draft.unknown} onclick={() => update({ images: draft.images?.filter((_, index) => index !== i) })}>×</button></div>{/each}</div>{/if}
			<div class="input-row">
				<button title="Attach images (PNG, JPEG, WebP; 4 MiB total)" aria-label="Attach images" disabled={draftFull || attaching || draft.pending || draft.unknown} onclick={() => fileInput.click()}>{attaching ? '…' : '+'}</button>
				<textarea id="codex-message" onpaste={paste} use:autosize rows="1" maxlength="32768" value={draft.text} disabled={draftFull || draft.pending} aria-label="Message this Codex session" placeholder="Message this session… · ⌘ / Ctrl Enter to send" oninput={(event) => update({ text: event.currentTarget.value })}
				onkeydown={(event) => { if (!event.isComposing && (event.metaKey || event.ctrlKey) && event.key === 'Enter') { event.preventDefault(); void send(); } }}></textarea>
			{#if !active || hasContent}
			<button class="send" aria-label={draft.pending ? 'Sending message' : 'Send message'} title={`Send message · ${turnLabel}`} onclick={send} disabled={draftFull || attaching || draft.pending || draft.unknown || tooLong || (!draft.text.trim() && !draft.images?.length)}>{#if draft.pending}…{:else}<svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" aria-hidden="true"><path d="M12 20V4m-7 7 7-7 7 7" /></svg>{/if}</button>
 {/if}
 {#if active}<button class="stop" class:running={!waiting && snapshot?.connected && live.length === 1} class:secondary={hasContent} onclick={stop} disabled={stopDisabled} aria-label={stopping ? 'Stopping current turn' : 'Stop current turn'} title={`Stop current turn · ${turnLabel}`}>{#if stopping}…{:else}<svg width="18" height="18" viewBox="0 0 18 18" aria-hidden="true"><rect x="4" y="4" width="10" height="10" fill="currentColor" /></svg>{/if}</button>{/if}
		</div>
		{#if tooLong}<p role="status">Message exceeds 32 KiB</p>{/if}
	{/if}
	{#if active && stopState?.notice}<p role="status">{stopState.notice}</p>{/if}
	{#if draftFull}<p role="status">Draft limit reached (20 sessions). Open another draft and clear its text and images to make room.</p>{/if}
	{#if draft.notice}<p role="status">{draft.notice}</p>{/if}
	{#if draft.unknown}<button onclick={() => update({ unknown: false, notice: 'Check complete. You can edit or send the message again.' })}>I CHECKED THE ORIGINAL SESSION</button>{/if}
</section>

<style>
	.file-input { display: none; }
	.attachments { display: flex; gap: 8px; margin-bottom: 6px; }
	.attachment { position: relative; border: 1px solid var(--border-default); }
	.attachment img { display: block; width: 56px; height: 44px; object-fit: cover; }
	.attachment button { position: absolute; top: 0; right: 0; padding: 0 4px; background: var(--bg-card); }

	.composer { flex-shrink: 0; border-top: 1px solid var(--border-default); padding: 8px 16px; background: var(--bg-card); }
 .stop.running svg { animation: pulse 3.6s ease-in-out infinite; }
 @keyframes pulse { 0%, 100% { opacity: 1; } 50% { opacity: .65; } }
 @media (prefers-reduced-motion: reduce) { .stop.running svg { animation: none; } }
 .send, .stop { width: 36px; }
 .stop.secondary { width: 30px; }
 button:focus-visible { outline: 1px solid var(--text-primary); outline-offset: 2px; }
	p, .connection { font-size: 12px; color: var(--text-secondary, #888); line-height: 1.5; }
	textarea { box-sizing: border-box; width: 100%; resize: none; min-width: 0; min-height: 36px; max-height: 140px; padding: 8px 10px; background: transparent; color: var(--text-primary); border: 1px solid var(--border-default); border-radius: 0; font: inherit; font-size: 13px; line-height: 18px; }
	textarea:focus { outline: 1px solid var(--text-secondary, #888); }
	.connection { display: flex; align-items: center; justify-content: space-between; gap: 12px; }
	button { flex-shrink: 0; padding: 7px 10px; color: var(--text-secondary); border: 1px solid var(--border-default); background: transparent; border-radius: 0; font: 11px var(--font-mono); cursor: pointer; }
	button:hover:not(:disabled) { color: var(--text-primary, #eee); border-color: #888; }
	button:disabled { opacity: .4; cursor: default; }
	.input-row { display: flex; align-items: flex-end; gap: 8px; }
	.input-row > button { box-sizing: border-box; height: 36px; min-height: 36px; padding: 0 10px; display: inline-flex; align-items: center; justify-content: center; }
	.input-row > button[aria-label="Attach images"] { width: 36px; }
	.send { min-height: 36px; color: var(--text-primary, #eee); }
	p { margin: 4px 0 0; overflow-wrap: anywhere; }
	.setup-toggle { margin-top: 6px; }
</style>
