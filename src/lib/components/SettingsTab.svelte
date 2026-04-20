<script lang="ts">
	import { onMount } from 'svelte';
	import { marked } from 'marked';
	import DOMPurify from 'dompurify';
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
		startDownloadAndInstall,
		skipVersion
	} from '$lib/stores/updater';
	import { flyIn } from '$lib/transitions';

	let checking = $state(false);
	let justChecked = $state(false);

	let version = $derived($currentVersion);
	let update = $derived($updateAvailable);
	let state = $derived($downloadState);
	let progress = $derived($downloadProgress);
	let error = $derived($downloadError);
	let notes = $derived($releaseNotes);
	let notesLoading = $derived($releaseNotesLoading);

	onMount(() => {
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

	function handleSkip() {
		if (update) skipVersion(update.version);
	}

	function handleInstall() {
		startDownloadAndInstall();
	}

	function renderMarkdown(content: string): string {
		const html = marked.parse(content, { async: false, breaks: true, gfm: true });
		return DOMPurify.sanitize(html as string);
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
</script>

<div class="settings-tab">
	<div class="section-header" in:flyIn|global={{ index: 0, duration: 350, stride: 25 }}>
		<span class="section-title">Settings</span>
	</div>

	<section class="block" in:flyIn|global={{ index: 1, duration: 350, stride: 25 }}>
		<div class="block-title">Application</div>
		<div class="row">
			<span class="row-label">Version</span>
			<span class="row-value">{version || '—'}</span>
		</div>
	</section>

	<section class="block" in:flyIn|global={{ index: 2, duration: 350, stride: 25 }}>
		<div class="block-title">Updates</div>

		<div class="row">
			<span class="row-label">Status</span>
			<span class="row-value">
				{#if update}
					<span class="badge badge-amber">New version {update.version} available</span>
				{:else if justChecked}
					<span class="badge badge-muted">You're on the latest</span>
				{:else}
					<span class="row-dim">Last check pending</span>
				{/if}
			</span>
		</div>

		<div class="actions">
			<button class="btn btn-ghost" onclick={handleCheck} disabled={checking}>
				{checking ? 'Checking…' : 'Check for updates'}
			</button>
		</div>

		{#if update}
			<div class="update-panel">
				<div class="update-header">
					<div class="version-row">
						<span class="version-old">{update.currentVersion}</span>
						<span class="version-arrow">→</span>
						<span class="version-new">{update.version}</span>
					</div>
				</div>

				<div class="release-notes">
					<div class="notes-title">Release notes</div>
					{#if notesLoading}
						<div class="state-msg">Loading release notes…</div>
					{:else if notes}
						<div class="markdown-body">{@html renderMarkdown(notes)}</div>
					{:else}
						<div class="state-msg">Release notes unavailable.</div>
					{/if}
				</div>

				{#if state === 'downloading'}
					<div class="progress-block">
						<div class="progress-label">
							Downloading…
							{#if progress.total}
								<span class="row-dim">
									{formatBytes(progress.received)} / {formatBytes(progress.total)}
								</span>
							{:else}
								<span class="row-dim">{formatBytes(progress.received)}</span>
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
				{:else if state === 'ready'}
					<div class="state-msg state-msg--ok">Download ready. Installing…</div>
				{:else if state === 'installing'}
					<div class="state-msg state-msg--ok">Installing — c9watch will relaunch.</div>
				{:else if state === 'error'}
					<div class="state-msg state-msg--err">
						Install failed{error ? `: ${error}` : ''}
					</div>
				{/if}

				<div class="actions">
					<button
						class="btn btn-primary"
						onclick={handleInstall}
						disabled={state === 'downloading' || state === 'ready' || state === 'installing'}
					>
						{#if state === 'downloading' || state === 'ready' || state === 'installing'}
							Installing…
						{:else}
							Download and install
						{/if}
					</button>
					<button
						class="btn btn-ghost"
						onclick={handleSkip}
						disabled={state === 'downloading' || state === 'ready' || state === 'installing'}
					>
						Skip this version
					</button>
				</div>
			</div>
		{/if}
	</section>
</div>

<style>
	.settings-tab {
		display: flex;
		flex-direction: column;
		height: 100%;
		overflow-y: auto;
		gap: var(--space-xl);
		padding-bottom: var(--space-xl);
	}

	.section-header {
		display: flex;
		align-items: center;
		gap: var(--space-md);
		padding-bottom: var(--space-md);
		border-bottom: 1px solid var(--text-primary);
		margin-bottom: var(--space-md);
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

	.block {
		display: flex;
		flex-direction: column;
		gap: var(--space-md);
		padding: var(--space-lg);
		background: var(--bg-card);
		border: 1px solid var(--border-default);
	}

	.block-title {
		font-family: var(--font-pixel);
		font-size: 11px;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.1em;
		color: var(--text-muted);
		padding-bottom: var(--space-sm);
		border-bottom: 1px solid var(--border-default);
	}

	.row {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--space-md);
		font-family: var(--font-mono);
		font-size: 13px;
	}

	.row-label {
		color: var(--text-muted);
		text-transform: uppercase;
		letter-spacing: 0.05em;
		font-size: 11px;
	}

	.row-value {
		color: var(--text-primary);
		display: inline-flex;
		align-items: center;
		gap: var(--space-sm);
	}

	.row-dim {
		color: var(--text-muted);
		font-size: 12px;
	}

	.badge {
		font-family: var(--font-mono);
		font-size: 11px;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		padding: 3px 8px;
		border: 1px solid var(--border-default);
		line-height: 1;
	}

	.badge-amber {
		color: var(--accent-amber);
		background: color-mix(in srgb, var(--accent-amber) 12%, transparent);
		border-color: color-mix(in srgb, var(--accent-amber) 40%, transparent);
	}

	.badge-muted {
		color: var(--text-muted);
	}

	.actions {
		display: flex;
		gap: var(--space-sm);
		flex-wrap: wrap;
	}

	.btn {
		font-family: var(--font-mono);
		font-size: 12px;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		padding: 8px 14px;
		cursor: pointer;
		transition: all var(--transition-fast);
		border: 1px solid var(--border-default);
	}

	.btn:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}

	.btn-ghost {
		background: transparent;
		color: var(--text-primary);
	}

	.btn-ghost:hover:not(:disabled) {
		background: rgba(255, 255, 255, 0.05);
		border-color: var(--text-secondary);
	}

	.btn-primary {
		background: color-mix(in srgb, var(--accent-amber) 15%, transparent);
		border-color: color-mix(in srgb, var(--accent-amber) 40%, transparent);
		color: var(--accent-amber);
	}

	.btn-primary:hover:not(:disabled) {
		background: color-mix(in srgb, var(--accent-amber) 25%, transparent);
		border-color: color-mix(in srgb, var(--accent-amber) 60%, transparent);
	}

	.update-panel {
		display: flex;
		flex-direction: column;
		gap: var(--space-md);
		padding: var(--space-md);
		background: var(--bg-elevated);
		border: 1px solid color-mix(in srgb, var(--accent-amber) 25%, transparent);
	}

	.update-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
	}

	.version-row {
		display: flex;
		align-items: center;
		gap: var(--space-sm);
		font-family: var(--font-mono);
		font-size: 13px;
	}

	.version-old {
		color: var(--text-muted);
		text-decoration: line-through;
	}

	.version-arrow {
		color: var(--text-muted);
	}

	.version-new {
		color: var(--accent-amber);
		font-weight: 600;
	}

	.release-notes {
		display: flex;
		flex-direction: column;
		gap: var(--space-sm);
	}

	.notes-title {
		font-family: var(--font-pixel);
		font-size: 10px;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.1em;
		color: var(--text-muted);
	}

	.state-msg {
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--text-muted);
		padding: var(--space-sm) 0;
	}

	.state-msg--ok {
		color: var(--text-primary);
	}

	.state-msg--err {
		color: var(--status-permission, #ff4444);
	}

	.markdown-body {
		font-family: var(--font-sans);
		font-size: 13px;
		line-height: 1.6;
		color: var(--text-primary);
		max-height: 280px;
		overflow-y: auto;
		padding: var(--space-sm) var(--space-md);
		background: var(--bg-base);
		border: 1px solid var(--border-default);
	}

	.markdown-body :global(h1),
	.markdown-body :global(h2),
	.markdown-body :global(h3) {
		font-family: var(--font-pixel);
		text-transform: uppercase;
		letter-spacing: 0.05em;
		font-size: 13px;
		margin: var(--space-md) 0 var(--space-xs);
		color: var(--text-primary);
	}

	.markdown-body :global(ul),
	.markdown-body :global(ol) {
		padding-left: var(--space-lg);
		margin: var(--space-xs) 0;
	}

	.markdown-body :global(li) {
		margin: 2px 0;
	}

	.markdown-body :global(code) {
		font-family: var(--font-mono);
		font-size: 12px;
		background: var(--bg-elevated);
		padding: 1px 5px;
		border: 1px solid var(--border-default);
	}

	.markdown-body :global(a) {
		color: var(--accent-amber);
		text-decoration: none;
	}

	.markdown-body :global(a):hover {
		text-decoration: underline;
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

	.progress-track {
		height: 6px;
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

	@keyframes pulse {
		0%, 100% { opacity: 0.5; }
		50% { opacity: 1; }
	}
</style>
