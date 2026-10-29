<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import QRCode from 'qrcode';
	import { getServerInfo, createPairingCode, type ServerInfo, type PairingCode } from '$lib/api';

	let { onclose }: { onclose: () => void } = $props();

	let info = $state<ServerInfo | null>(null);
	let pairing = $state<PairingCode | null>(null);
	let qrDataUrl = $state<string>('');
	let pageUrl = $state('');
	let error = $state<string>('');
	let copied = $state(false);
	let creating = $state(false);
	let now = $state(Date.now());
	const tick = setInterval(() => (now = Date.now()), 1000);
	onDestroy(() => clearInterval(tick));

	let secondsLeft = $derived(pairing ? Math.max(0, Math.ceil((pairing.expiresAt - now) / 1000)) : 0);
	let expired = $derived(pairing !== null && secondsLeft === 0);
	let countdown = $derived(`${Math.floor(secondsLeft / 60)}:${String(secondsLeft % 60).padStart(2, '0')}`);
	/** XXXX-XXXX reads easier; the server ignores the hyphen. */
	let displayCode = $derived(pairing ? `${pairing.code.slice(0, 4)}-${pairing.code.slice(4)}` : '');

	async function renderQr(info: ServerInfo, code: string) {
		qrDataUrl = '';
		pageUrl = '';
		if (!info.lanEnabled) return;
		// QR encodes an HTTP URL so the phone camera opens the browser directly.
		// The code rides in the fragment, which never reaches the server or a log.
		pageUrl = `http://${info.localIp}:${info.port}/#pair=${code}`;
		qrDataUrl = await QRCode.toDataURL(pageUrl, {
			width: 256,
			margin: 2,
			color: { dark: '#ffffff', light: '#000000' }
		});
	}

	async function newCode() {
		if (!info || creating) return;
		creating = true;
		try {
			pairing = await createPairingCode();
			now = Date.now();
			await renderQr(info, pairing.code);
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to create a pairing code';
		} finally {
			creating = false;
		}
	}

	onMount(async () => {
		try {
			info = await getServerInfo();
			await newCode();
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load server info';
		}
	});

	async function copyUrl() {
		if (!pageUrl) return;
		try {
			await navigator.clipboard.writeText(pageUrl);
		} catch {
			const el = document.createElement('textarea');
			el.value = pageUrl;
			document.body.appendChild(el);
			el.select();
			document.execCommand('copy');
			document.body.removeChild(el);
		}
		copied = true;
		setTimeout(() => (copied = false), 2000);
	}

	function handleBackdrop(e: MouseEvent) {
		if (e.target === e.currentTarget) onclose();
	}

	function handleKeydown(e: KeyboardEvent) {
		if (e.key === 'Escape') onclose();
	}
</script>

<svelte:window on:keydown={handleKeydown} />

{#snippet pairingCode()}
	<div class="info-section">
		<span class="info-label">Pairing code</span>
		<div class="code-box" class:expired>
			<code class="pair-code" aria-live="polite">{displayCode || '…'}</code>
			<span class="code-meta">
				<span class="countdown">{expired ? 'Expired' : `Expires in ${countdown}`}</span>
				<button class="new-code-btn" onclick={newCode} disabled={creating}>New code</button>
			</span>
		</div>
	</div>
{/snippet}

<!-- svelte-ignore a11y_click_events_have_key_events -->
<div class="backdrop" onclick={handleBackdrop} role="dialog" aria-modal="true" aria-label="Connect Mobile" tabindex="-1">
	<div class="modal">
		<div class="modal-header">
			<span class="modal-title">Connect Mobile</span>
			<button class="close-btn" onclick={onclose} aria-label="Close">
				<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
					<line x1="18" y1="6" x2="6" y2="18" />
					<line x1="6" y1="6" x2="18" y2="18" />
				</svg>
			</button>
		</div>

		{#if error}
			<div class="error">{error}</div>
		{:else if !info}
			<div class="loading">Loading server info...</div>
		{:else if !info.lanEnabled}
			<div class="instructions remote-off">
				<p class="remote-off-title">Remote access is off. This Mac only accepts connections from itself.</p>
				<p>To connect a phone on this network, turn on LAN access in Settings → Remote access.</p>
				<p>To use c9watch from another computer, forward the port over SSH: <code>ssh -L 9210:127.0.0.1:9210 &lt;this-mac&gt;</code>, then open <code>localhost:9210</code> in a browser there and enter this code.</p>
			</div>
			{@render pairingCode()}
		{:else}
			<div class="qr-container" class:expired>
				{#if qrDataUrl}<img src={qrDataUrl} alt="QR code with the pairing link" class="qr-image" />{/if}
			</div>

			{@render pairingCode()}

			<div class="info-section">
				<span class="info-label">Pairing link</span>
				<button class="url-box" onclick={copyUrl} title="Click to copy">
					<code class="url-text">{pageUrl}</code>
					<span class="copy-hint">{copied ? 'COPIED' : 'COPY'}</span>
				</button>
			</div>

			<div class="instructions">
				<p>Scan the QR with the phone camera, or open this Mac's address on port {info.port} and enter the code. Each code works once, for 5 minutes.</p>
			</div>
		{/if}
	</div>
</div>

<style>
	.backdrop {
		position: fixed;
		inset: 0;
		background: rgba(0, 0, 0, 0.85);
		display: flex;
		align-items: center;
		justify-content: center;
		z-index: 9999;
		animation: fade-in 150ms ease;
	}

	.modal {
		background: var(--bg-card);
		border: 1px solid var(--border-default);
		width: 380px;
		max-width: 90vw;
		padding: var(--space-xl);
		display: flex;
		flex-direction: column;
		gap: var(--space-xl);
		animation: scale-in 150ms ease;
	}

	.modal-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
	}

	.modal-title {
		font-family: var(--font-pixel);
		font-size: 16px;
		font-weight: 600;
		color: var(--text-primary);
		text-transform: uppercase;
		letter-spacing: 0.1em;
	}

	.close-btn {
		display: flex;
		align-items: center;
		justify-content: center;
		width: 28px;
		height: 28px;
		color: var(--text-muted);
		border: 1px solid transparent;
		background: transparent;
		cursor: pointer;
	}

	.close-btn:hover {
		color: var(--text-primary);
		border-color: var(--border-default);
	}

	.qr-container {
		display: flex;
		align-items: center;
		justify-content: center;
		min-height: 280px;
		padding: var(--space-md);
		border: 1px solid var(--border-muted);
		background: #000;
	}

	.qr-image {
		width: 256px;
		height: 256px;
		image-rendering: pixelated;
	}

	.info-section {
		display: flex;
		flex-direction: column;
		gap: var(--space-sm);
	}

	.info-label {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--text-muted);
		text-transform: uppercase;
		letter-spacing: 0.1em;
	}

	.url-box {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--space-sm);
		padding: var(--space-sm) var(--space-md);
		border: 1px solid var(--border-default);
		background: var(--bg-base);
		cursor: pointer;
		text-align: left;
		min-height: 0;
		min-width: 0;
	}

	.url-box:hover {
		border-color: var(--text-muted);
	}

	.qr-container.expired .qr-image,
	.code-box.expired .pair-code {
		opacity: 0.3;
	}

	.code-box {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--space-md);
		padding: var(--space-md);
		border: 1px solid var(--border-default);
		background: var(--bg-base);
	}

	.pair-code {
		font-family: var(--font-mono);
		font-size: 28px;
		font-weight: 600;
		color: var(--text-primary);
		letter-spacing: 0.12em;
	}

	.code-meta {
		display: flex;
		flex-direction: column;
		align-items: flex-end;
		gap: var(--space-sm);
		flex-shrink: 0;
	}

	.countdown {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--text-muted);
		text-transform: uppercase;
		letter-spacing: 0.05em;
	}

	.new-code-btn {
		padding: 4px var(--space-sm);
		border: 1px solid var(--border-default);
		background: transparent;
		color: var(--text-secondary);
		font-family: var(--font-mono);
		font-size: 11px;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		cursor: pointer;
		min-height: 0;
		min-width: 0;
	}

	.new-code-btn:hover:not(:disabled) {
		border-color: var(--text-muted);
		color: var(--text-primary);
	}

	.new-code-btn:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}

	.url-text {
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--text-secondary);
		word-break: break-all;
	}

	.copy-hint {
		font-family: var(--font-mono);
		font-size: 10px;
		color: var(--accent-green);
		letter-spacing: 0.1em;
		flex-shrink: 0;
	}

	.instructions {
		display: flex;
		flex-direction: column;
		gap: var(--space-xs);
	}

	.instructions p {
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--text-muted);
		line-height: 1.6;
	}

	.remote-off {
		gap: var(--space-md);
	}

	.remote-off .remote-off-title {
		color: var(--text-primary);
	}

	.remote-off code {
		font-family: var(--font-mono);
		color: var(--text-primary);
		word-break: break-all;
	}

	.loading, .error {
		font-family: var(--font-mono);
		font-size: 13px;
		color: var(--text-muted);
		text-align: center;
		padding: var(--space-3xl) 0;
	}

	.error {
		color: var(--accent-red);
	}
</style>
