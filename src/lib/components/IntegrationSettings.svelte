<script lang="ts">
	import ProviderBadge from './ProviderBadge.svelte';
	import OpenCodeConnection from './OpenCodeConnection.svelte';
	import CodexDesktopSupport from './CodexDesktopSupport.svelte';
	import type { SessionProvider } from '$lib/types';

	type LocalIntegration = {
		provider: SessionProvider;
		name: string;
		description: string;
		detail: string;
	};

	const localIntegrations: LocalIntegration[] = [
		{
			provider: 'claudeCode',
			name: 'Claude Code',
			description: 'Local Claude Code sessions are detected automatically.',
			detail: 'No endpoint or token is required. Full Disk Access may be needed to read every session.'
		},
		{
			provider: 'codex',
			name: 'Codex',
			description: 'Local Codex sessions are detected automatically.',
			detail: 'Archive monitoring is automatic. Optional Desktop messaging support is available below.'
		},
		{
			provider: 'cursor',
			name: 'Cursor',
			description: 'Local Cursor Agent sessions are detected automatically.',
			detail: 'c9watch reads the local Cursor Agent store; there is no separate connection to configure.'
		},
		{
			provider: 'pi',
			name: 'Pi',
			description: 'Local Pi sessions are detected automatically.',
			detail: 'c9watch reads the local Pi session files; there is no separate connection to configure.'
		}
	];
</script>

<section class="integration-settings" aria-labelledby="integration-settings-title">
	<header>
		<div>
			<h2 id="integration-settings-title">Integrations</h2>
			<p>Connect c9watch to the agent sessions you want to monitor.</p>
		</div>
		<span class="scope">SESSION SOURCES</span>
	</header>

	<div class="section-block" aria-labelledby="local-integrations-title">
		<div class="section-heading">
			<h3 id="local-integrations-title">Local integrations</h3>
			<span>Automatic detection</span>
		</div>
		<div class="integration-grid">
			{#each localIntegrations as integration}
				<article class="integration-card">
					<div class="card-topline">
						<ProviderBadge provider={integration.provider} noun="integration" />
						<span class="state"><span class="state-marker"></span>AUTO</span>
					</div>
					<h4>{integration.name}</h4>
					<p>{integration.description}</p>
					<span class="detail">{integration.detail}</span>
				</article>
			{/each}
		</div>
		<CodexDesktopSupport />
	</div>

	<div class="section-block remote-block" aria-labelledby="remote-integrations-title">
		<div class="section-heading">
			<h3 id="remote-integrations-title">Remote integrations</h3>
			<span>Explicit connection</span>
		</div>
		<OpenCodeConnection />
	</div>
</section>

<style>
	.integration-settings { width: 100%; color: var(--text-primary); font: 14px/1.5 var(--font-sans); }
	header { display: flex; align-items: baseline; justify-content: space-between; gap: var(--space-lg); margin-bottom: var(--space-2xl); }
	h2, h3 { margin: 0 0 var(--space-sm); font: 600 13px/1.5 var(--font-pixel); text-transform: uppercase; letter-spacing: .1em; }
	h4 { margin: var(--space-md) 0 var(--space-xs); font: 600 15px/1.35 var(--font-sans); }
	p { margin: 0; }
	header p, .detail, .scope, .section-heading span { color: var(--text-secondary); font-size: 13px; line-height: 1.55; }
	.scope { flex-shrink: 0; border: 1px solid var(--border-default); padding: 4px 8px; font: 11px/1.4 var(--font-mono); letter-spacing: .06em; }
	.section-block { border-top: 1px solid var(--border-default); padding-top: var(--space-xl); }
	.remote-block { margin-top: var(--space-2xl); }
	.section-heading { display: flex; align-items: baseline; justify-content: space-between; gap: var(--space-lg); margin-bottom: var(--space-lg); }
	.section-heading h3 { margin: 0; }
	.integration-grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: var(--space-md); }
	.integration-card { min-width: 0; padding: var(--space-lg); border: 1px solid var(--border-default); background: var(--bg-card); }
	.card-topline { display: flex; align-items: center; justify-content: space-between; gap: var(--space-md); }
	.state { display: inline-flex; align-items: center; gap: 5px; color: var(--accent-green); font: 700 8px/1 var(--font-mono); letter-spacing: .08em; }
	.state-marker { width: 5px; height: 5px; background: var(--accent-green); border: 1px solid var(--accent-green); }
	.integration-card p { color: var(--text-primary); font-size: 13px; line-height: 1.55; }
	.detail { display: block; margin-top: var(--space-md); }
	@media (max-width: 720px) {
		header, .section-heading { align-items: flex-start; flex-direction: column; gap: var(--space-sm); }
		.integration-grid { grid-template-columns: 1fr; }
	}
	@media (prefers-reduced-motion: reduce) { .integration-card { transition: none; } }
</style>
