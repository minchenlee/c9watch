<script lang="ts">
	import { onMount } from 'svelte';
	import { setStoredWsUrl, getStoredWsUrl, clearStoredWsUrl, wsClient } from '$lib/ws';
	import { initializeSessionListeners, sessions } from '$lib/stores/sessions';
	import { getSessions } from '$lib/api';

	let { onconnected }: { onconnected: () => void } = $props();

	let mode = $state<'choose' | 'token' | 'url'>('choose');
	let connecting = $state(false);
	let tokenInput = $state('');
	let urlInput = $state('');
	let inputError = $state('');

	const WS_PORT = 9210;

	async function doConnect(wsUrl: string) {
		connecting = true;
		inputError = '';
		setStoredWsUrl(wsUrl);
		try {
			await initializeSessionListeners();
			if (!wsClient.isConnected) {
				clearStoredWsUrl();
				throw new Error('Could not connect — check that the desktop app is running and the token is correct');
			}
			const initial = await getSessions();
			sessions.set(initial);
			onconnected();
		} catch (e) {
			inputError = e instanceof Error ? e.message : 'Connection failed';
			connecting = false;
		}
	}

	function connectWithWsUrl(url: string) {
		const trimmed = url.trim();
		try {
			const parsed = new URL(trimmed);
			if (parsed.protocol !== 'ws:' && parsed.protocol !== 'wss:') {
				inputError = 'URL must start with ws:// or wss://';
				return;
			}
		} catch {
			inputError = 'Invalid URL format';
			return;
		}
		doConnect(trimmed);
	}

	function connectWithToken() {
		const token = tokenInput.trim();
		if (!token) return;
		if (!/^[a-f0-9]+$/i.test(token)) {
			inputError = 'Invalid token format';
			return;
		}
		// Use 127.0.0.1 instead of localhost to avoid IPv6 resolution issues
		let host = window.location.hostname;
		if (host === 'localhost') host = '127.0.0.1';
		const wsUrl = `ws://${host}:${WS_PORT}/ws?token=${token}`;
		doConnect(wsUrl);
	}

	function goBack() {
		mode = 'choose';
		inputError = '';
	}

	onMount(() => {
		// Auto-connect if URL already stored (e.g. from previous session)
		const existing = getStoredWsUrl();
		if (existing) {
			doConnect(existing);
			return;
		}

		const params = new URLSearchParams(window.location.search);

		// Auto-connect from QR code: ?token=hextoken
		const tokenParam = params.get('token');
		if (tokenParam) {
			window.history.replaceState({}, '', window.location.pathname);
			const host = window.location.hostname;
			doConnect(`ws://${host}:${WS_PORT}/ws?token=${tokenParam}`);
			return;
		}

		// Legacy: ?wsUrl=ws://...
		const wsUrlParam = params.get('wsUrl');
		if (wsUrlParam) {
			window.history.replaceState({}, '', window.location.pathname);
			doConnect(wsUrlParam);
		}
	});
</script>

<div class="connection-screen">
	<div class="content">
		<div class="header">
			<div class="logo-box">
				<span class="logo-text">C9</span>
			</div>
			<h1 class="title">c9watch</h1>
			<p class="subtitle">Connect to your desktop session</p>
		</div>

		{#if connecting}
			<div class="connecting">
				<div class="spinner"></div>
				<span class="connecting-text">Connecting...</span>
				{#if inputError}
					<div class="error">{inputError}</div>
					<button class="back-btn" onclick={() => { connecting = false; inputError = ''; }}>Retry</button>
				{/if}
			</div>
		{:else if mode === 'choose'}
			<div class="options">
				<button class="option-btn" onclick={() => (mode = 'token')}>
					<svg width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">
						<rect x="3" y="11" width="18" height="11" rx="2" ry="2" />
						<path d="M7 11V7a5 5 0 0 1 10 0v4" />
					</svg>
					<span class="option-label">Enter Token</span>
					<span class="option-desc">Type the token shown on desktop</span>
				</button>

				<button class="option-btn secondary" onclick={() => (mode = 'url')}>
					<span class="option-label">Advanced: Paste Full URL</span>
				</button>
			</div>
		{:else if mode === 'token'}
			<div class="paste-view">
				<label class="input-label" for="token-input">Token</label>
				<input
					id="token-input"
					type="text"
					class="url-input"
					bind:value={tokenInput}
					placeholder="e.g. a1b2c3d4e5f6..."
					autocomplete="off"
					autocapitalize="off"
					onkeydown={(e) => e.key === 'Enter' && connectWithToken()}
				/>
				{#if inputError}
					<div class="error">{inputError}</div>
				{/if}
				<div class="hint">Connects to <code>ws://{typeof window !== 'undefined' ? window.location.hostname : 'host'}:{WS_PORT}</code></div>
				<div class="paste-actions">
					<button class="back-btn" onclick={goBack}>Back</button>
					<button class="connect-btn" onclick={connectWithToken} disabled={!tokenInput.trim()}>
						Connect
					</button>
				</div>
			</div>
		{:else if mode === 'url'}
			<div class="paste-view">
				<label class="input-label" for="ws-url-input">WebSocket URL</label>
				<input
					id="ws-url-input"
					type="url"
					class="url-input"
					bind:value={urlInput}
					placeholder="ws://192.168.x.x:9210/ws?token=..."
					onkeydown={(e) => e.key === 'Enter' && connectWithWsUrl(urlInput)}
				/>
				{#if inputError}
					<div class="error">{inputError}</div>
				{/if}
				<div class="paste-actions">
					<button class="back-btn" onclick={goBack}>Back</button>
					<button class="connect-btn" onclick={() => connectWithWsUrl(urlInput)} disabled={!urlInput.trim()}>
						Connect
					</button>
				</div>
			</div>
		{/if}
	</div>
</div>

<style>
	.connection-screen {
		display: flex;
		align-items: center;
		justify-content: center;
		height: 100vh;
		height: 100dvh;
		width: 100vw;
		background: var(--bg-base);
		padding: var(--space-xl);
		padding-top: max(var(--space-xl), var(--safe-top));
		padding-right: max(var(--space-xl), var(--safe-right));
		padding-bottom: max(var(--space-xl), var(--safe-bottom));
		padding-left: max(var(--space-xl), var(--safe-left));
	}

	.content {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: var(--space-3xl);
		width: 100%;
		max-width: 360px;
	}

	.header {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: var(--space-md);
	}

	.logo-box {
		width: 56px;
		height: 56px;
		border: 1px solid var(--border-default);
		display: flex;
		align-items: center;
		justify-content: center;
		margin-bottom: var(--space-sm);
	}

	.logo-text {
		font-family: var(--font-pixel);
		font-size: 20px;
		font-weight: 700;
		color: var(--text-primary);
	}

	.title {
		font-family: var(--font-pixel);
		font-size: 24px;
		font-weight: 600;
		color: var(--text-primary);
		text-transform: uppercase;
		letter-spacing: 0.15em;
	}

	.subtitle {
		font-family: var(--font-mono);
		font-size: 13px;
		color: var(--text-muted);
		text-transform: uppercase;
		letter-spacing: 0.05em;
	}

	.options {
		display: flex;
		flex-direction: column;
		gap: var(--space-md);
		width: 100%;
	}

	.option-btn {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: var(--space-sm);
		padding: var(--space-xl);
		border: 1px solid var(--border-default);
		background: var(--bg-card);
		color: var(--text-primary);
		cursor: pointer;
		transition: all 0.15s ease;
		min-height: 0;
		min-width: 0;
	}

	.option-btn:hover {
		border-color: var(--text-muted);
		background: var(--bg-card-hover);
	}

	.option-label {
		font-family: var(--font-mono);
		font-size: 14px;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.1em;
	}

	.option-desc {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--text-muted);
	}

	/* Paste view */
	.paste-view {
		display: flex;
		flex-direction: column;
		gap: var(--space-md);
		width: 100%;
	}

	.input-label {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--text-muted);
		text-transform: uppercase;
		letter-spacing: 0.1em;
	}

	.url-input {
		width: 100%;
		padding: var(--space-md);
		border: 1px solid var(--border-default);
		background: var(--bg-card);
		color: var(--text-primary);
		font-family: var(--font-mono);
		font-size: 13px;
	}

	.url-input::placeholder {
		color: var(--text-muted);
	}

	.url-input:focus {
		outline: none;
		border-color: var(--text-secondary);
	}

	.paste-actions {
		display: flex;
		gap: var(--space-md);
		margin-top: var(--space-sm);
	}

	.back-btn {
		flex: 1;
		padding: var(--space-md);
		border: 1px solid var(--border-default);
		background: transparent;
		color: var(--text-secondary);
		font-family: var(--font-mono);
		font-size: 13px;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		cursor: pointer;
		min-height: 0;
		min-width: 0;
	}

	.back-btn:hover {
		border-color: var(--text-muted);
		color: var(--text-primary);
	}

	.connect-btn {
		flex: 1;
		padding: var(--space-md);
		border: 1px solid var(--text-primary);
		background: var(--text-primary);
		color: var(--bg-base);
		font-family: var(--font-mono);
		font-size: 13px;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		cursor: pointer;
		min-height: 0;
		min-width: 0;
	}

	.connect-btn:hover {
		opacity: 0.9;
	}

	.connect-btn:disabled {
		opacity: 0.3;
		cursor: not-allowed;
	}

	.hint {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--text-muted);
	}

	.hint code {
		color: var(--text-secondary);
	}

	.option-btn.secondary {
		padding: var(--space-md);
		background: transparent;
		border-style: dashed;
		color: var(--text-muted);
	}

	.option-btn.secondary:hover {
		color: var(--text-secondary);
		border-style: solid;
	}

	.option-btn.secondary .option-label {
		font-size: 12px;
		font-weight: 500;
	}

	.connecting {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: var(--space-md);
	}

	.spinner {
		width: 24px;
		height: 24px;
		border: 2px solid var(--border-default);
		border-top-color: var(--text-primary);
		animation: spin 0.8s linear infinite;
	}

	.connecting-text {
		font-family: var(--font-mono);
		font-size: 13px;
		color: var(--text-muted);
		text-transform: uppercase;
		letter-spacing: 0.1em;
	}

	.error {
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--accent-red);
		text-align: center;
	}

	@media (max-width: 768px), (orientation: landscape) and (max-height: 500px) {
		.connection-screen {
			padding: var(--space-lg);
			padding-top: max(var(--space-lg), var(--safe-top));
			padding-right: max(var(--space-lg), var(--safe-right));
			padding-bottom: max(var(--space-lg), var(--safe-bottom));
			padding-left: max(var(--space-lg), var(--safe-left));
		}

		.option-btn,
		.back-btn,
		.connect-btn {
			min-height: var(--touch-min);
		}

		.url-input {
			min-height: var(--touch-min);
			font-size: 16px; /* avoid iOS zoom on focus */
		}
	}

	@media (orientation: landscape) and (max-height: 500px) {
		.connection-screen {
			align-items: flex-start;
			overflow-y: auto;
			padding: var(--space-md);
			padding-top: max(var(--space-md), var(--safe-top));
		}

		.content {
			gap: var(--space-lg);
			max-width: 480px;
		}

		.logo-box {
			width: 40px;
			height: 40px;
			margin-bottom: 0;
		}

		.title {
			font-size: 18px;
		}
	}
</style>
