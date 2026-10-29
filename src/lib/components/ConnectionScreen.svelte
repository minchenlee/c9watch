<script lang="ts">
	import { onMount } from 'svelte';
	import {
		wsClient,
		getStoredConnection,
		getStoredHost,
		migrateLegacyWsUrl,
		type WsAuth
	} from '$lib/ws';
	import { initializeSessionListeners, sessions } from '$lib/stores/sessions';
	import { getSessions } from '$lib/api';

	let { onconnected }: { onconnected: () => void } = $props();

	let mode = $state<'choose' | 'code' | 'advanced'>('choose');
	let connecting = $state(false);
	let codeInput = $state('');
	let hostInput = $state('');
	let inputError = $state('');
	/** Shown when an old token link was dropped or the desktop forgot this device. */
	let pairAgain = $state(false);

	const WS_PORT = 9210;
	const CONNECT_ERROR =
		'Could not connect. Check that remote access is on and the code has not expired.';

	/** This page's own server, as "host:port". The socket goes to the server that served the page. */
	function pageHost(): string {
		const { hostname, port } = window.location;
		// 127.0.0.1 instead of localhost avoids IPv6 resolution issues.
		const name = hostname === 'localhost' ? '127.0.0.1' : hostname;
		const host = name.includes(':') && !name.startsWith('[') ? `[${name}]` : name;
		return `${host}:${port || WS_PORT}`;
	}

	/**
	 * Crockford base32: case-insensitive, hyphens and spaces ignored, I/L read
	 * as 1 and O as 0. Returns the 8-character code or null.
	 */
	function normalizeCode(raw: string): string | null {
		const code = raw
			.replace(/[\s-]/g, '')
			.toUpperCase()
			.replace(/[IL]/g, '1')
			.replace(/O/g, '0');
		return /^[0-9A-HJKMNP-TV-Z]{8}$/.test(code) ? code : null;
	}

	/** "host" or "host:port"; no scheme, path or query. */
	function normalizeHost(raw: string): string | null {
		const value = raw.trim();
		if (/^[a-z0-9.-]+$/i.test(value) || /^\[[0-9a-f:]+\]$/i.test(value)) return `${value}:${WS_PORT}`;
		if (/^([a-z0-9.-]+|\[[0-9a-f:]+\]):\d{1,5}$/i.test(value)) return value;
		return null;
	}

	async function doConnect(host: string, auth: WsAuth) {
		connecting = true;
		inputError = '';
		try {
			await wsClient.connect(host, auth);
			await initializeSessionListeners();
			const initial = await getSessions();
			sessions.set(initial);
			onconnected();
		} catch {
			inputError = CONNECT_ERROR;
			pairAgain = !getStoredConnection() && !!getStoredHost();
		}
	}

	function connectWithCode() {
		const code = normalizeCode(codeInput);
		if (!code) {
			inputError = 'A pairing code is 8 letters and digits, as shown on the desktop.';
			return;
		}
		doConnect(pageHost(), { code });
	}

	function connectAdvanced() {
		if (/[?&]token=/i.test(hostInput)) {
			inputError = 'Old token links no longer work. Enter the host and a pairing code.';
			return;
		}
		const host = normalizeHost(hostInput);
		if (!host) {
			inputError = 'Enter a host or host:port, without ws:// or a path.';
			return;
		}
		const code = normalizeCode(codeInput);
		if (!code) {
			inputError = 'A pairing code is 8 letters and digits, as shown on the desktop.';
			return;
		}
		doConnect(host, { code });
	}

	function show(next: 'choose' | 'code' | 'advanced') {
		mode = next;
		inputError = '';
	}

	onMount(() => {
		const hadLegacyToken = migrateLegacyWsUrl();

		// QR scan opens this page with #pair=CODE. The fragment never reaches the
		// server or a log; drop it from the address bar right away.
		const hash = new URLSearchParams(window.location.hash.replace(/^#/, ''));
		const pairParam = hash.get('pair');
		if (pairParam !== null) {
			window.history.replaceState({}, '', window.location.pathname + window.location.search);
			const code = normalizeCode(pairParam);
			if (code) {
				doConnect(pageHost(), { code });
				return;
			}
		}

		const stored = getStoredConnection();
		if (stored) {
			doConnect(stored.host, { token: stored.token });
			return;
		}
		pairAgain = hadLegacyToken || !!getStoredHost();
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
				{#if inputError}
					<div class="error" role="alert">{inputError}</div>
					{#if pairAgain}<div class="pair-again">Pair again: open MOBILE on the desktop for a new code.</div>{/if}
					<button class="back-btn" onclick={() => { connecting = false; inputError = ''; mode = 'choose'; }}>Back</button>
				{:else}
					<div class="spinner"></div>
					<span class="connecting-text">Connecting...</span>
				{/if}
			</div>
		{:else if mode === 'choose'}
			{#if pairAgain}
				<div class="pair-again" role="status">Pair again: this device is no longer paired. Open MOBILE on the desktop for a new code.</div>
			{/if}
			<div class="options">
				<button class="option-btn" onclick={() => show('code')}>
					<svg width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">
						<rect x="3" y="11" width="18" height="11" rx="2" ry="2" />
						<path d="M7 11V7a5 5 0 0 1 10 0v4" />
					</svg>
					<span class="option-label">Enter pairing code</span>
					<span class="option-desc">Type the code shown on desktop</span>
				</button>

				<button class="option-btn secondary" onclick={() => show('advanced')}>
					<span class="option-label">Advanced: Other host</span>
				</button>
			</div>
		{:else if mode === 'code'}
			<div class="paste-view">
				<label class="input-label" for="code-input">Enter pairing code</label>
				<input
					id="code-input"
					type="text"
					class="url-input code-input"
					bind:value={codeInput}
					placeholder="XXXX-XXXX"
					maxlength="11"
					autocomplete="off"
					autocapitalize="characters"
					spellcheck="false"
					onkeydown={(e) => e.key === 'Enter' && connectWithCode()}
				/>
				{#if inputError}
					<div class="error" role="alert">{inputError}</div>
				{/if}
				<div class="hint">Connects to <code>{typeof window !== 'undefined' ? pageHost() : 'host'}</code></div>
				<div class="paste-actions">
					<button class="back-btn" onclick={() => show('choose')}>Back</button>
					<button class="connect-btn" onclick={connectWithCode} disabled={!codeInput.trim()}>
						Connect
					</button>
				</div>
			</div>
		{:else if mode === 'advanced'}
			<div class="paste-view">
				<label class="input-label" for="host-input">Host</label>
				<input
					id="host-input"
					type="text"
					class="url-input"
					bind:value={hostInput}
					placeholder="192.168.x.x:9210"
					autocomplete="off"
					autocapitalize="off"
					spellcheck="false"
				/>
				<label class="input-label" for="adv-code-input">Pairing code</label>
				<input
					id="adv-code-input"
					type="text"
					class="url-input code-input"
					bind:value={codeInput}
					placeholder="XXXX-XXXX"
					maxlength="11"
					autocomplete="off"
					autocapitalize="characters"
					spellcheck="false"
					onkeydown={(e) => e.key === 'Enter' && connectAdvanced()}
				/>
				{#if inputError}
					<div class="error" role="alert">{inputError}</div>
				{/if}
				<div class="paste-actions">
					<button class="back-btn" onclick={() => show('choose')}>Back</button>
					<button class="connect-btn" onclick={connectAdvanced} disabled={!hostInput.trim() || !codeInput.trim()}>
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
		width: 100vw;
		background: var(--bg-base);
		padding: var(--space-xl);
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

	.code-input {
		font-size: 18px;
		letter-spacing: 0.15em;
		text-transform: uppercase;
	}

	.pair-again {
		width: 100%;
		padding: var(--space-md);
		border: 1px solid var(--border-default);
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--text-muted);
		text-align: center;
	}

	.error {
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--accent-red);
		text-align: center;
	}

	@media (max-width: 768px) {
		.connection-screen {
			padding: var(--space-lg);
		}
	}
</style>
