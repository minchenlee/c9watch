<script lang="ts">
	import { invoke } from '@tauri-apps/api/core';
	let launching = $state(false);
	let notice = $state('');
	async function launch() {
		if (launching) return;
		launching = true;
		try { notice = await invoke<string>('launch_codex_desktop_bridge'); }
		catch (error) { notice = String(error); }
		finally { launching = false; }
	}
</script>

<section class="desktop-support" aria-label="Codex Desktop messaging support">
	<h3><span class="square"></span>CODEX DESKTOP MESSAGING <span class="preview">PREVIEW</span></h3>
	<p>Finish your work and quit Codex / ChatGPT, then launch it here. Open your conversation in Codex before sending a message from c9watch.</p>
	<p>Approvals stay in Codex. To turn support off, quit Codex and open it normally. Your Codex settings are not changed.</p>
	<button onclick={launch} disabled={launching}>{launching ? 'LAUNCHING…' : 'LAUNCH WITH SUPPORT'}</button>
	{#if notice}<p role="status">{notice}</p>{/if}
</section>

<style>
	.desktop-support { border: 1px solid var(--border-default); background: var(--bg-card); padding: var(--space-lg); margin-top: var(--space-md); }
	h3 { display: flex; align-items: center; flex-wrap: wrap; gap: 8px; margin: 0 0 12px; font: 11px var(--font-mono); letter-spacing: .05em; color: var(--text-primary); }
	.square { width: 6px; height: 6px; background: var(--text-secondary); }
	.preview { color: var(--text-secondary); border: 1px solid var(--border-muted); padding: 2px 4px; }
	p { color: var(--text-secondary); font-size: 12px; line-height: 1.6; margin: 8px 0; }
	button { border: 1px solid var(--border-default); border-radius: 0; background: transparent; padding: 9px 12px; color: var(--text-primary); font: 11px var(--font-mono); cursor: pointer; margin-top: 4px; }
	button:hover:not(:disabled) { border-color: var(--border-focus); }
	button:disabled { opacity: .4; cursor: default; }
</style>
