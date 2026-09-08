<script module lang="ts">
	import { SvelteMap } from 'svelte/reactivity';
	type Attachment = { name: string; url: string };
	type Draft = { images?: Attachment[]; text: string; pending: boolean; notice: string; unknown: boolean };
	// Bounded, provider-qualified drafts survive closing/reopening a detail view.
	const drafts = new SvelteMap<string, Draft>();
</script>

<script lang="ts">
	import { onMount } from 'svelte';
	import { invoke } from '@tauri-apps/api/core';
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
			const receipt = await invoke<{ status: string; detail: string }>('send_codex_message', { sessionId: id, text, ...(draft.images?.length ? { images: draft.images.map(image => image.url) } : {}) });
			update({ pending: false, notice: receipt.detail, unknown: receipt.status === 'unknown',
				...((receipt.status === 'accepted' || receipt.status === 'queued') ? { text: '', images: [] } : {}) }, target);
		} catch (error) {
			// The IPC channel itself may have failed after the backend sent the request.
			update({ pending: false, unknown: true, notice: `${String(error)} — check the original session before retrying.` }, target);
		}
	}
</script>

	<section class="composer" aria-label="Send a message to Codex">
		<div class="heading"><span class="square" class:connected={available}></span><label for="codex-message" title="Send to this session. While working, Codex receives an additional instruction.">MESSAGE CODEX</label>{#if available && !checking}<span class="shortcut">⌘ / Ctrl Enter to send</span>{/if}</div>
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
				<textarea id="codex-message" onpaste={paste} use:autosize rows="1" maxlength="32768" value={draft.text} disabled={draftFull || draft.pending} placeholder="Message this session…" oninput={(event) => update({ text: event.currentTarget.value })}
				onkeydown={(event) => { if (!event.isComposing && (event.metaKey || event.ctrlKey) && event.key === 'Enter') { event.preventDefault(); void send(); } }}></textarea>
			<button class="send" onclick={send} disabled={draftFull || attaching || draft.pending || draft.unknown || tooLong || (!draft.text.trim() && !draft.images?.length)}>{draft.pending ? 'SENDING…' : 'SEND'}</button>
		</div>
		{#if tooLong}<p role="status">Message exceeds 32 KiB</p>{/if}
	{/if}
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
	.heading { display: flex; align-items: center; gap: 8px; font: 11px var(--font-mono, monospace); letter-spacing: .08em; color: var(--text-secondary, #888); margin-bottom: 6px; }
	.square { width: 6px; height: 6px; background: #555; }
	.square.connected { background: var(--accent-green, #00ff88); }
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
	.shortcut { margin-left: auto; font-size: 10px; letter-spacing: 0; color: var(--text-muted); }
	.send { min-height: 36px; color: var(--text-primary, #eee); }
	p { margin: 4px 0 0; overflow-wrap: anywhere; }
	.setup-toggle { margin-top: 6px; }
</style>
