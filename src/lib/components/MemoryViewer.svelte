<script lang="ts">
	import { onMount, tick } from 'svelte';
	import { slide } from 'svelte/transition';
	import { cubicOut } from 'svelte/easing';
	import { marked } from 'marked';
	import DOMPurify from 'dompurify';
	import { getMemoryFiles, revealInFileManager } from '$lib/api';
	import type { ProjectMemory } from '$lib/types';
	import { providerFilter } from '$lib/stores/provider-filter';
	import { matchesProvider, providerFilterLabel, providerOf } from '$lib/provider';
	import ProviderBadge from '$lib/components/ProviderBadge.svelte';
	import { flyIn } from '$lib/transitions';

	let allProjects = $state<ProjectMemory[]>([]);
	let loading = $state(true);
	let selectedKey = $state<string | null>(null);

	onMount(async () => {
		try {
			allProjects = await getMemoryFiles();
		} finally {
			loading = false;
		}
	});

	let projects = $derived(allProjects.filter((project) => matchesProvider(project, $providerFilter)));

	function projectKey(project: ProjectMemory): string {
		return `${providerOf(project)}:${project.memoryDirPath}`;
	}

	let selectedProject = $derived(
		projects.find((project) => projectKey(project) === selectedKey) ?? projects[0] ?? null
	);

	$effect(() => {
		const nextKey = selectedProject ? projectKey(selectedProject) : null;
		if (selectedKey !== nextKey) selectedKey = nextKey;
	});

	let copied = $state(false);

	let expanded = $state<string | null>(null);
	let fileHeaders: Record<string, HTMLButtonElement | null> = {};
	let contentPane = $state<HTMLDivElement | null>(null);

	$effect(() => {
		if (!selectedProject) return;
		expanded = selectedProject.files[0]?.filename ?? null;
	});

	async function toggleFile(filename: string) {
		expanded = expanded === filename ? null : filename;
		if (expanded !== filename || !contentPane) return;
		await tick();
		const header = fileHeaders[filename];
		if (!header) return;
		const paneRect = contentPane.getBoundingClientRect();
		const headerRect = header.getBoundingClientRect();
		const targetTop = contentPane.scrollTop + (headerRect.top - paneRect.top);
		contentPane.scrollTo({ top: targetTop, behavior: 'smooth' });
	}

	function copyClaudeCommand() {
		if (!selectedProject || providerOf(selectedProject) !== 'claudeCode') return;
		const cmd = `claude "Review my memory files and suggest improvements" --project-dir ${selectedProject.projectPath}`;
		navigator.clipboard.writeText(cmd);
		copied = true;
		setTimeout(() => (copied = false), 2000);
	}

	function renderMarkdown(content: string): string {
		const renderer = new marked.Renderer();

		renderer.code = function ({ text, lang }) {
			const language = lang || 'code';
			return `
				<div class="code-block-wrapper">
					<div class="code-header">
						<span class="code-lang">${language}</span>
					</div>
					<pre><code class="language-${language}">${text}</code></pre>
				</div>
			`;
		};

		const rawHtml = marked.parse(content, {
			async: false,
			breaks: true,
			gfm: true,
			renderer
		});
		return DOMPurify.sanitize(rawHtml as string);
	}

	async function handleReveal() {
		if (!selectedProject) return;
		await revealInFileManager(selectedProject.memoryDirPath);
	}
</script>

<div class="memory-viewer">
	<div class="section-header" in:flyIn|global={{ index: 0, duration: 350, stride: 25 }}>
		<span class="section-title">Memory</span>
		<span class="section-count">{projects.length}</span>
	</div>

	{#if loading}
		<div class="state-msg"><span class="loading-spinner">◌</span> Scanning memory files…</div>
	{:else if projects.length === 0}
		<div class="state-msg">
			{$providerFilter === 'all' ? 'No memory files found.' : `No ${providerFilterLabel($providerFilter)} memory files found.`}
		</div>
		{#if $providerFilter === 'codex'}
			<div class="empty-hint">Codex stores durable memory in ~/.codex/memories/</div>
		{:else if $providerFilter === 'cursor'}
			<div class="empty-hint">Cursor Agent memory is not monitored yet.</div>
		{:else if $providerFilter === 'claudeCode'}
			<div class="empty-hint">Claude Code stores memory in ~/.claude/projects/*/memory/</div>
		{:else}
			<div class="empty-hint">Memory is discovered from ~/.claude/projects/*/memory/ and ~/.codex/memories/</div>
		{/if}
	{:else}
		<div class="two-panel">
			<aside class="project-list">
				{#each projects as project, i (projectKey(project))}
					<button
						class="project-item"
						class:selected={projectKey(project) === selectedKey}
						onclick={() => (selectedKey = projectKey(project))}
						in:flyIn|global={{ index: i + 1, duration: 350, stride: 25 }}
					>
						<span class="project-item-main">
							<ProviderBadge provider={providerOf(project)} compact noun="memory" />
							<span class="project-item-name">{project.projectName}</span>
						</span>
						<span class="project-item-count">{project.files.length}</span>
					</button>
				{/each}
			</aside>

			<div class="memory-content" bind:this={contentPane}>
				{#if selectedProject}
					{#key projectKey(selectedProject)}
						<div class="content-header" in:flyIn|global={{ index: 0, duration: 800, stride: 120 }}>
							<div class="content-path-row">
								<ProviderBadge provider={providerOf(selectedProject)} compact noun="memory" />
								<span class="content-path">{selectedProject.projectPath}</span>
								<button class="reveal-btn" onclick={handleReveal} title="Reveal in Finder">
									<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
										<path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6" />
										<polyline points="15 3 21 3 21 9" />
										<line x1="10" y1="14" x2="21" y2="3" />
									</svg>
								</button>
							</div>
							{#if providerOf(selectedProject) === 'claudeCode'}
								<button class="claude-cmd" onclick={copyClaudeCommand} title="Copy command to discuss memory with Claude Code">
									<span class="cmd-text">claude "Review my memory files" --project-dir {selectedProject.projectPath}</span>
									<span class="cmd-copy">{copied ? '✓ Copied' : 'Copy'}</span>
								</button>
							{:else}
								<div class="provider-note">Codex reads these durable memory files automatically.</div>
							{/if}
						</div>
						{#each selectedProject.files as file, i (file.filename)}
							<div class="file-section" in:flyIn|global={{ index: i + 1, duration: 800, stride: 120 }}>
								<button
									bind:this={fileHeaders[file.filename]}
									class="file-header"
									class:expanded={expanded === file.filename}
									onclick={() => toggleFile(file.filename)}
									aria-expanded={expanded === file.filename ? 'true' : 'false'}
								>
									<span class="file-header-name">{file.filename}</span>
									<span class="file-header-chevron">{expanded === file.filename ? '▾' : '▸'}</span>
								</button>
								{#if expanded === file.filename}
									<div class="markdown-body" transition:slide|local={{ duration: 250, easing: cubicOut }}>
										{@html renderMarkdown(file.content)}
									</div>
								{/if}
							</div>
						{/each}
					{/key}
				{/if}
			</div>
		</div>
	{/if}
</div>

<style>
	.memory-viewer {
		display: flex;
		flex-direction: column;
		height: 100%;
		overflow: hidden;
	}

	/* ── Section header (matches HISTORY / COST tabs) ────────────── */

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

	.section-count {
		font-family: var(--font-pixel);
		font-size: 18px;
		font-weight: 500;
		line-height: 1;
		color: var(--text-secondary);
	}

	.reveal-btn {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		background: none;
		border: none;
		color: var(--text-muted);
		cursor: pointer;
		padding: 0;
		transition: color 0.15s ease;
	}

	.reveal-btn:hover {
		color: var(--text-primary);
	}

	/* ── Loading / empty states (matches HISTORY / COST pattern) ── */

	.state-msg {
		font-family: var(--font-mono);
		font-size: 13px;
		color: var(--text-muted);
		text-transform: uppercase;
		letter-spacing: 0.05em;
		padding: var(--space-xl) 0;
		text-align: center;
	}

	.state-msg .loading-spinner {
		animation: spin 1s linear infinite;
		display: inline-block;
		margin-right: var(--space-xs);
	}

	@keyframes spin {
		from { transform: rotate(0deg); }
		to { transform: rotate(360deg); }
	}

	.empty-hint {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--text-muted);
		margin-top: var(--space-sm);
		text-align: center;
	}

	/* ── Two-panel layout ────────────────────────────────────────── */

	.two-panel {
		display: flex;
		flex: 1;
		overflow: hidden;
	}

	.project-list {
		width: 200px;
		min-width: 160px;
		border-right: 1px solid var(--border-default);
		overflow-y: auto;
		flex-shrink: 0;
	}

	.project-item {
		display: flex;
		align-items: center;
		justify-content: space-between;
		width: 100%;
		padding: var(--space-sm) var(--space-md);
		background: none;
		border: none;
		border-bottom: 1px solid var(--border-default);
		color: var(--text-muted);
		cursor: pointer;
		text-align: left;
		font-family: var(--font-mono);
		font-size: 12px;
		transition: all 0.15s ease;
	}

	.project-item:hover {
		background: rgba(255, 255, 255, 0.03);
		color: var(--text-primary);
	}

	.project-item.selected {
		background: rgba(255, 255, 255, 0.08);
		color: var(--text-primary);
		border-left: 2px solid var(--accent);
	}

	.project-item-name {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.project-item-main {
		display: flex;
		align-items: center;
		gap: var(--space-sm);
		min-width: 0;
		overflow: hidden;
	}

	.project-item-count {
		font-family: var(--font-mono);
		font-size: 10px;
		color: var(--text-muted);
		flex-shrink: 0;
		margin-left: var(--space-sm);
	}

	/* ── Content pane ────────────────────────────────────────────── */

	.memory-content {
		flex: 1;
		overflow-y: auto;
		padding: var(--space-lg);
	}

	.content-header {
		margin-bottom: var(--space-lg);
		padding-bottom: var(--space-sm);
		border-bottom: 1px solid var(--border-default);
	}

	.content-path-row {
		display: flex;
		align-items: center;
		gap: var(--space-sm);
	}

	.content-path {
		font-size: 11px;
		color: var(--text-muted);
		font-family: var(--font-mono);
	}

	.claude-cmd {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--space-md);
		width: 100%;
		margin-top: var(--space-sm);
		padding: var(--space-sm) var(--space-md);
		background: rgba(255, 255, 255, 0.03);
		border: 1px solid var(--border-default);
		border-radius: 6px;
		cursor: pointer;
		transition: all 0.15s ease;
	}

	.claude-cmd:hover {
		border-color: var(--text-secondary);
		background: rgba(255, 255, 255, 0.05);
	}

	.cmd-text {
		font-size: 11px;
		font-family: var(--font-mono);
		color: var(--text-muted);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.cmd-copy {
		font-family: var(--font-mono);
		font-size: 10px;
		font-weight: 600;
		color: var(--accent);
		flex-shrink: 0;
		text-transform: uppercase;
		letter-spacing: 0.05em;
	}

	.provider-note {
		margin-top: var(--space-sm);
		padding: var(--space-sm) var(--space-md);
		border: 1px solid var(--border-default);
		border-radius: 6px;
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--text-muted);
	}

	/* ── File sections ───────────────────────────────────────────── */

	.file-section {
		margin-bottom: var(--space-md);
	}

	.file-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		width: 100%;
		font-family: var(--font-pixel);
		font-size: 13px;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.1em;
		color: var(--accent);
		margin: 0;
		padding: var(--space-sm) 0;
		background: none;
		border: none;
		border-bottom: 1px solid var(--border-default);
		cursor: pointer;
		text-align: left;
		transition: color 0.15s ease;
	}

	.file-header:hover {
		color: var(--text-primary);
	}

	.file-header-name {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.file-header-chevron {
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--text-muted);
		flex-shrink: 0;
		margin-left: var(--space-sm);
	}

	.file-header.expanded {
		margin-bottom: var(--space-md);
	}

	/* ── Markdown body ───────────────────────────────────────────── */

	.markdown-body {
		font-family: var(--font-sans);
		font-size: 15px;
		line-height: 1.65;
		color: var(--text-primary);
	}

	.markdown-body :global(h1),
	.markdown-body :global(h2),
	.markdown-body :global(h3) {
		margin: var(--space-lg) 0 var(--space-sm);
		font-weight: 600;
		color: var(--text-primary);
	}

	.markdown-body :global(h1) { font-size: 20px; }
	.markdown-body :global(h2) { font-size: 17px; }
	.markdown-body :global(h3) { font-size: 15px; }

	.markdown-body :global(p) {
		margin: var(--space-sm) 0;
	}

	.markdown-body :global(ul),
	.markdown-body :global(ol) {
		margin: var(--space-sm) 0;
		padding-left: 20px;
	}

	.markdown-body :global(li) {
		margin: var(--space-xs) 0;
	}

	.markdown-body :global(code) {
		background: rgba(255, 255, 255, 0.06);
		padding: 2px 5px;
		border-radius: 3px;
		font-size: 13px;
		font-family: var(--font-mono);
	}

	.markdown-body :global(strong) {
		color: var(--text-primary);
		font-weight: 600;
	}

	.markdown-body :global(.code-block-wrapper) {
		margin: var(--space-md) 0;
		border: 1px solid var(--border-default);
		border-radius: 6px;
		overflow: hidden;
	}

	.markdown-body :global(.code-header) {
		padding: 6px var(--space-md);
		font-size: 11px;
		color: var(--text-muted);
		border-bottom: 1px solid var(--border-default);
		background: rgba(255, 255, 255, 0.02);
	}

	.markdown-body :global(pre) {
		margin: 0;
		padding: var(--space-md);
		overflow-x: auto;
		font-size: 13px;
		line-height: 1.55;
	}

	.markdown-body :global(pre code) {
		background: none;
		padding: 0;
		border-radius: 0;
	}

	.markdown-body :global(a) {
		color: var(--accent);
		text-decoration: none;
	}

	.markdown-body :global(a:hover) {
		text-decoration: underline;
	}

	.markdown-body :global(hr) {
		border: none;
		border-top: 1px solid var(--border-default);
		margin: var(--space-lg) 0;
	}

	.markdown-body :global(blockquote) {
		margin: var(--space-sm) 0;
		padding: var(--space-xs) var(--space-md);
		border-left: 2px solid var(--border-default);
		color: var(--text-secondary);
	}

	.markdown-body :global(table) {
		width: 100%;
		border-collapse: collapse;
		margin: var(--space-md) 0;
		font-size: 14px;
	}

	.markdown-body :global(th),
	.markdown-body :global(td) {
		padding: 6px 10px;
		border: 1px solid var(--border-default);
		text-align: left;
	}

	.markdown-body :global(th) {
		font-weight: 600;
		background: rgba(255, 255, 255, 0.03);
	}
</style>
