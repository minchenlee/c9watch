<script lang="ts">
	import { onMount } from 'svelte';
	import NotificationSettings from './NotificationSettings.svelte';
	import UsageSettings from './UsageSettings.svelte';
	import { marked } from 'marked';
	import DOMPurify from 'dompurify';
	import { openUrl } from '@tauri-apps/plugin-opener';
	import { isTauri } from '$lib/ws';
	import {
		currentVersion,
		updateAvailable,
		downloadState,
		downloadProgress,
		downloadError,
		releaseNotes,
		releaseNotesLoading,
		manualCheck,
		fetchReleaseNotes,
		startDownloadAndInstall
	} from '$lib/stores/updater';

	let checking = $state(false);
	let activeSection = $state<'notifications' | 'usage' | 'about'>('notifications');
	let nativeNotifications = $state(false);
	let justChecked = $state(false);

	let version = $derived($currentVersion);
	let update = $derived($updateAvailable);
	let dlState = $derived($downloadState);
	let progress = $derived($downloadProgress);
	let error = $derived($downloadError);
	let notes = $derived($releaseNotes);
	let notesLoading = $derived($releaseNotesLoading);

	onMount(() => {
		nativeNotifications = isTauri() && /Mac/i.test(navigator.platform);
		if (!nativeNotifications) activeSection = 'usage';
		if (update) fetchReleaseNotes();
	});

	$effect(() => {
		if (update) fetchReleaseNotes();
	});

	async function handleCheck() {
		checking = true;
		justChecked = false;
		await manualCheck();
		checking = false;
		justChecked = true;
		setTimeout(() => (justChecked = false), 2500);
	}

	function handleInstall() {
		startDownloadAndInstall();
	}

	function renderMarkdown(content: string): string {
		const html = marked.parse(content, { async: false, breaks: true, gfm: true });
		return DOMPurify.sanitize(html as string);
	}

	function handleNotesClick(e: MouseEvent) {
		const anchor = (e.target as HTMLElement | null)?.closest('a');
		if (!anchor) return;
		const href = anchor.getAttribute('href');
		if (!href || href.startsWith('#')) return;
		e.preventDefault();
		if (isTauri()) {
			openUrl(href).catch((err) => console.error('[settings] openUrl failed:', err));
		} else {
			window.open(href, '_blank', 'noopener,noreferrer');
		}
	}

	function formatBytes(n: number): string {
		if (n < 1024) return `${n} B`;
		if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
		return `${(n / (1024 * 1024)).toFixed(1)} MB`;
	}

	let progressPct = $derived.by(() => {
		if (!progress.total || progress.total === 0) return null;
		return Math.min(100, Math.round((progress.received / progress.total) * 100));
	});

	let installing = $derived(
		dlState === 'downloading' || dlState === 'ready' || dlState === 'installing'
	);
</script>

<div class="settings-tab">
	<div class="section-header" >
		<span class="section-title">Settings</span>
	</div>

	<div class="settings-shell">
        <nav class="settings-nav" aria-label="Settings sections">
            {#if nativeNotifications}<button class:active={activeSection === 'notifications'} aria-current={activeSection === 'notifications' ? 'page' : undefined} onclick={() => activeSection = 'notifications'}>Notifications</button>{/if}
            <button class:active={activeSection === 'usage'} aria-current={activeSection === 'usage' ? 'page' : undefined} onclick={() => activeSection = 'usage'}>Usage</button>
            <button class:active={activeSection === 'about'} aria-current={activeSection === 'about' ? 'page' : undefined} onclick={() => activeSection = 'about'}>About &amp; updates</button>
        </nav>
        <div class="content"><div class="settings-body">
        {#if nativeNotifications}<div hidden={activeSection !== 'notifications'}><NotificationSettings /></div>{/if}
        <div hidden={activeSection !== 'usage'}><UsageSettings /></div>
        <div class="about-panel" hidden={activeSection !== 'about'}>
		<div class="group" >
			<div class="group-title group-title--lg">Version</div>
			<div class="group-body">
				<span class="mono">c9watch {version || '—'}</span>
			</div>
		</div>

		<div class="group" >
			<div class="group-title group-title--lg">Updates</div>
			<div class="group-body">
				<div class="status-line">
					{#if update}
						<span class="status-text status-text--amber">Update available</span>
						<span class="version-diff">
							<span class="ver-old">{update.currentVersion}</span>
							<span class="ver-arrow">→</span>
							<span class="ver-new">{update.version}</span>
						</span>
					{:else if justChecked}
						<span class="status-text">Up to date</span>
					{:else}
						<span class="status-text status-text--dim">Check for the latest version</span>
					{/if}
					{#if update}
						<button class="btn-primary" onclick={handleInstall} disabled={installing}>
							{installing ? 'Installing…' : 'Download and install'}
						</button>
					{:else}
						<button class="btn-ghost" onclick={handleCheck} disabled={checking}>
							{checking ? 'Checking…' : 'Check now'}
						</button>
					{/if}
				</div>

				{#if update && dlState === 'downloading'}
					<div class="progress-block">
						<div class="progress-label">
							Downloading
							{#if progress.total}
								<span class="progress-bytes">
									{formatBytes(progress.received)} / {formatBytes(progress.total)}
								</span>
							{:else}
								<span class="progress-bytes">{formatBytes(progress.received)}</span>
							{/if}
						</div>
						<div class="progress-track">
							<div
								class="progress-fill"
								style:width={progressPct != null ? `${progressPct}%` : '100%'}
								class:indeterminate={progressPct == null}
							></div>
						</div>
					</div>
				{:else if update && dlState === 'ready'}
					<div class="state-line state-line--ok">Download ready — installing…</div>
				{:else if update && dlState === 'installing'}
					<div class="state-line state-line--ok">Installing — c9watch will relaunch.</div>
				{:else if update && dlState === 'error'}
					<div class="state-line state-line--err">
						Install failed{error ? ` · ${error}` : ''}
					</div>
				{/if}
			</div>
		</div>

		{#if update}
			<div class="group" >
				<div class="group-title">Release notes · {update.version}</div>
				<div class="notes-surface">
					{#if notesLoading}
						<div class="notes-state">Loading…</div>
					{:else if notes}
						<!-- svelte-ignore a11y_no_static_element_interactions -->
						<!-- svelte-ignore a11y_click_events_have_key_events -->
						<div class="markdown-body" onclick={handleNotesClick}>{@html renderMarkdown(notes)}</div>
					{:else}
						<div class="notes-state">Release notes unavailable.</div>
					{/if}
				</div>
			</div>
		{/if}
	</div></div></div></div>
</div>

<style>
	.settings-tab {
		display: flex;
		flex-direction: column;
		height: 100%;
		overflow: hidden;
	}

	.section-header {
		display: flex;
		align-items: center;
		gap: var(--space-md);
		padding-bottom: var(--space-md);
		border-bottom: 1px solid var(--text-primary);
		margin-bottom: var(--space-lg);
		flex-shrink: 0;
	}

	.section-title {
		font-family: var(--font-pixel);
		font-size: 22px;
		font-weight: 600;
		color: var(--text-primary);
		text-transform: uppercase;
		letter-spacing: 0.1em;
		line-height: 1;
	}

	.content {
		flex: 1;
		overflow-y: auto;
		display: flex;
		flex-direction: column;
		gap: var(--space-lg);
		padding-bottom: var(--space-xl);
	}

	.group {
		display: flex;
		flex-direction: column;
		gap: var(--space-sm);
	}

	.group-title {
		font-family: var(--font-pixel);
		font-size: 10px;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.12em;
		color: var(--text-muted);
		padding-bottom: var(--space-xs);
		border-bottom: 1px solid var(--border-default);
	}

	.group-title--lg {
		font-size: 14px;
		letter-spacing: 0.1em;
		color: var(--text-primary);
		padding-bottom: var(--space-sm);
	}

	.group-body {
		display: flex;
		flex-direction: column;
		gap: var(--space-sm);
		padding-top: var(--space-xs);
	}

	.mono {
		font-family: var(--font-mono);
		font-size: 13px;
		color: var(--text-primary);
		letter-spacing: 0.02em;
	}

	.status-line {
		display: flex;
		align-items: center;
		gap: var(--space-md);
		flex-wrap: wrap;
	}

	.status-text {
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--text-primary);
		text-transform: uppercase;
		letter-spacing: 0.08em;
	}

	.status-text--amber {
		color: var(--accent-amber);
	}

	.status-text--dim {
		color: var(--text-muted);
	}

	.version-diff {
		display: inline-flex;
		align-items: center;
		gap: var(--space-xs);
		font-family: var(--font-mono);
		font-size: 12px;
	}

	.ver-old {
		color: var(--text-muted);
		text-decoration: line-through;
	}

	.ver-arrow {
		color: var(--text-muted);
	}

	.ver-new {
		color: var(--accent-amber);
		font-weight: 600;
	}

	.status-line > button {
		margin-left: auto;
	}

	.btn-ghost {
		background: transparent;
		border: 1px solid var(--border-default);
		color: var(--text-primary);
		font-family: var(--font-pixel);
		font-size: 10px;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.1em;
		padding: 5px 10px;
		cursor: pointer;
		transition: all var(--transition-fast);
	}

	.btn-ghost:hover:not(:disabled) {
		background: rgba(255, 255, 255, 0.05);
		border-color: var(--text-secondary);
	}

	.btn-ghost:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}

	.btn-primary {
		background: transparent;
		border: 1px solid color-mix(in srgb, var(--accent-amber) 40%, transparent);
		color: var(--accent-amber);
		font-family: var(--font-pixel);
		font-size: 10px;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.1em;
		padding: 6px 12px;
		cursor: pointer;
		transition: all var(--transition-fast);
	}

	.btn-primary:hover:not(:disabled) {
		background: color-mix(in srgb, var(--accent-amber) 12%, transparent);
		border-color: color-mix(in srgb, var(--accent-amber) 60%, transparent);
	}

	.btn-primary:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}

	.notes-surface {
		border: 1px solid var(--border-default);
	}

	.notes-state {
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--text-muted);
		text-transform: uppercase;
		letter-spacing: 0.05em;
		padding: var(--space-md);
		text-align: center;
	}

	.markdown-body {
		font-family: var(--font-sans);
		font-size: 13px;
		line-height: 1.6;
		color: var(--text-primary);
		max-height: 320px;
		overflow-y: auto;
		padding: var(--space-md) var(--space-lg);
	}

	.markdown-body :global(h1),
	.markdown-body :global(h2),
	.markdown-body :global(h3) {
		font-family: var(--font-pixel);
		text-transform: uppercase;
		letter-spacing: 0.08em;
		font-size: 12px;
		font-weight: 600;
		color: var(--text-muted);
		margin: var(--space-md) 0 var(--space-xs);
	}

	.markdown-body :global(h1):first-child,
	.markdown-body :global(h2):first-child,
	.markdown-body :global(h3):first-child {
		margin-top: 0;
	}

	.markdown-body :global(p) {
		margin: var(--space-xs) 0;
	}

	.markdown-body :global(ul),
	.markdown-body :global(ol) {
		padding-left: var(--space-lg);
		margin: var(--space-xs) 0;
	}

	.markdown-body :global(li) {
		margin: 3px 0;
	}

	.markdown-body :global(code) {
		font-family: var(--font-mono);
		font-size: 11px;
		background: var(--bg-base);
		padding: 1px 5px;
		border: 1px solid var(--border-default);
		color: var(--text-primary);
	}

	.markdown-body :global(pre) {
		background: var(--bg-base);
		border: 1px solid var(--border-default);
		padding: var(--space-sm);
		overflow-x: auto;
		margin: var(--space-xs) 0;
	}

	.markdown-body :global(pre code) {
		background: transparent;
		border: none;
		padding: 0;
	}

	.markdown-body :global(a) {
		color: var(--accent-amber);
		text-decoration: none;
	}

	.markdown-body :global(a):hover {
		text-decoration: underline;
	}

	.markdown-body :global(strong) {
		color: var(--text-primary);
		font-weight: 600;
	}

	.markdown-body :global(hr) {
		border: none;
		border-top: 1px solid var(--border-default);
		margin: var(--space-md) 0;
	}

	.progress-block {
		display: flex;
		flex-direction: column;
		gap: var(--space-xs);
	}

	.progress-label {
		display: flex;
		justify-content: space-between;
		gap: var(--space-sm);
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--text-muted);
		text-transform: uppercase;
		letter-spacing: 0.05em;
	}

	.progress-bytes {
		color: var(--text-secondary);
	}

	.progress-track {
		height: 4px;
		background: var(--bg-base);
		border: 1px solid var(--border-default);
		overflow: hidden;
	}

	.progress-fill {
		height: 100%;
		background: var(--accent-amber);
		transition: width 0.2s ease;
	}

	.progress-fill.indeterminate {
		animation: pulse 1.5s ease-in-out infinite;
	}

	.state-line {
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--text-muted);
		text-transform: uppercase;
		letter-spacing: 0.05em;
	}

	.state-line--ok {
		color: var(--text-primary);
	}

	.state-line--err {
		color: var(--status-permission, #ff4444);
	}

	@keyframes pulse {
		0%, 100% { opacity: 0.5; }
		50% { opacity: 1; }
	}

    .settings-body { width: 100%; max-width: 960px; margin: 0; display: flex; flex-direction: column; gap: var(--space-xl); padding: 0 var(--space-lg) var(--space-2xl); box-sizing: border-box;  }
    .group { border-top: 1px solid var(--border-default); padding-top: var(--space-xl); }
    .group-title, .group-title--lg { font-family: var(--font-pixel); font-size: 13px; letter-spacing: .1em; text-transform: uppercase; border: 0; color: var(--text-primary); }
    .mono, .status-text, .version-diff, .notes-state, .state-line, .progress-label { font-size: 13px; text-transform: none; letter-spacing: normal; }
    .btn-ghost, .btn-primary { font-family: var(--font-pixel); font-size: 11px; text-transform: uppercase; letter-spacing: .05em; padding: 4px var(--space-sm); }
    .markdown-body { font-size: 15px; }
    .markdown-body :global(h1), .markdown-body :global(h2), .markdown-body :global(h3) { font-family: var(--font-sans); font-size: 16px; text-transform: none; letter-spacing: normal; color: var(--text-primary); }
    .status-text--dim, .notes-state, .state-line, .progress-label { color: var(--text-secondary); }
    button:focus-visible { outline: 1px solid var(--border-focus); outline-offset: 0; }
    @media (max-width: 600px) { .settings-body { padding-inline: 0; } }
    @media (prefers-reduced-motion: reduce) { .progress-fill { transition: none; animation: none; } }

    .settings-shell { display: flex; flex: 1; min-height: 0; }
    .settings-nav { width: 200px; min-width: 160px; flex-shrink: 0; border-right: 1px solid var(--border-default); }
    .settings-nav button { display: block; width: 100%; text-align: left; padding: var(--space-sm) var(--space-md); border: 0; border-bottom: 1px solid var(--border-default); background: transparent; color: var(--text-secondary); font: 12px/1.5 var(--font-mono); cursor: pointer; }
    .settings-nav button:hover { background: rgba(255,255,255,.03); color: var(--text-primary); }
    .settings-nav button.active { background: rgba(255,255,255,.08); color: var(--text-primary); }
    .settings-nav button.active { box-shadow: inset 2px 0 var(--border-focus); }
    .content { min-width: 0; }
    .about-panel > .group:first-child { border-top: 0; padding-top: 0; }
    .about-panel > .group + .group { margin-top: var(--space-xl); }
    @media (max-width: 680px) {
        .settings-shell { flex-direction: column; }
        .settings-nav { width: 100%; display: flex; border-right: 0; border-bottom: 1px solid var(--border-default); padding: 0 0 var(--space-sm); margin-bottom: var(--space-md); }
        .settings-nav button { width: auto; }
        .settings-nav button.active { box-shadow: inset 0 -1px var(--border-focus); }
    }
</style>
