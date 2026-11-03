<script lang="ts">
	import { tick } from 'svelte';
	import { flyIn } from '$lib/transitions';
	import {
		mail, mailThreads, ownMail, pausedAgents, mailFocus,
		sendMail, cancelMail, setMailPaused, readMailBody
	} from '$lib/stores/mail';
	import { sourceOf, stateLabel, routeLabel, isHeld, needsAttention, type MailSource } from '$lib/mail-store';
	import type { C9pMsgSummary } from '$lib/c9p-1';

	const ROUTE_HELP: Record<string, string> = {
		PEER: 'peer: sent over the Claude Code messaging socket. The model knows this is not the user.',
		PASTE: 'paste: typed into the session. The model reads this as user input.'
	};

	let threads = $derived($mailThreads);
	let selectedId = $derived($mailFocus && threads.some(t => t.agentId === $mailFocus) ? $mailFocus : threads[0]?.agentId ?? null);
	let thread = $derived(threads.find(t => t.agentId === selectedId) ?? null);
	let paused = $derived(selectedId ? $pausedAgents.has(selectedId) : false);

	let draft = $state('');
	let sending = $state(false);
	let busy = $state<Record<string, boolean>>({});
	let errors = $state<Record<string, string>>({});
	let bodies = $state<Record<string, string>>({});
	let listEl = $state<HTMLDivElement | null>(null);
	let flashId = $state<string | null>(null);

	function select(agentId: string) { mailFocus.set(agentId); }

	function time(ms: number): string {
		return new Date(ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
	}
	function chipClass(m: C9pMsgSummary): string {
		if (m.state === 'failed') return 'chip chip-red';
		if (needsAttention(m)) return 'chip chip-amber';
		return 'chip';
	}
	function sourceLabel(source: MailSource, m: C9pMsgSummary): string {
		if (source === 'you') return '[you]';
		if (source === 'terminal') return 'terminal';
		return `from ${m.from.name ?? 'agent'} (agent), not you`;
	}
	function previewOf(id: string | null): string | null {
		if (!id) return null;
		const m = $mail.messages.get(id);
		return m ? m.preview : null;
	}
	async function jump(id: string | null) {
		if (!id || !listEl) return;
		const el = listEl.querySelector<HTMLElement>(`[data-msg-id="${CSS.escape(id)}"]`);
		if (!el) return;
		el.scrollIntoView({ block: 'center', behavior: 'smooth' });
		flashId = id;
		setTimeout(() => { if (flashId === id) flashId = null; }, 1200);
	}
	async function run(key: string, action: () => Promise<string | null>) {
		busy = { ...busy, [key]: true };
		const { [key]: _old, ...rest } = errors;
		errors = rest;
		const error = await action();
		busy = { ...busy, [key]: false };
		if (error) errors = { ...errors, [key]: error };
	}
	async function showFull(m: C9pMsgSummary) {
		const r = await readMailBody(m.msgId);
		if ('body' in r) bodies = { ...bodies, [m.msgId]: r.body };
		else errors = { ...errors, [m.msgId]: r.error };
	}
	async function send() {
		const body = draft.trim();
		if (!body || !thread || sending) return;
		sending = true;
		const error = await sendMail(thread.agentId, body);
		sending = false;
		if (error) { errors = { ...errors, send: error }; return; }
		const { send: _s, ...rest } = errors;
		errors = rest;
		draft = '';
		await tick();
		listEl?.scrollTo({ top: listEl.scrollHeight });
	}
	function onComposerKey(e: KeyboardEvent) {
		if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) { e.preventDefault(); void send(); }
		if (e.key === 'Escape') (e.target as HTMLElement).blur();
	}

	// New thread selected → start at the newest mail.
	$effect(() => {
		void selectedId;
		tick().then(() => listEl?.scrollTo({ top: listEl.scrollHeight }));
	});
</script>

<div class="mail-tab">
	<div class="section-header" in:flyIn|global={{ index: 0, duration: 350, stride: 25 }}>
		<span class="section-title">Mail</span>
		<span class="section-count">{threads.length}</span>
	</div>

	{#if $mail.status === 'unavailable'}
		<div class="state-msg">Mail API not available in this build</div>
		<div class="empty-hint">{$mail.error ?? 'This window cannot call the mail methods.'}</div>
	{:else if $mail.status === 'loading'}
		<div class="state-msg"><span class="loading-spinner">◌</span> Loading mail…</div>
	{:else if $mail.status === 'error' && threads.length === 0}
		<div class="state-msg">Cannot read mail</div>
		<div class="empty-hint">{$mail.error} · retrying</div>
	{:else if threads.length === 0}
		<div class="state-msg">No mail yet.</div>
		<div class="empty-hint">Mail between engine agents and from you shows here.</div>
	{:else}
		{#if $mail.status === 'error'}
			<div class="stale-note">Mail is not live: {$mail.error} · retrying</div>
		{/if}
		<div class="two-panel">
			<aside class="agent-list" aria-label="Agents with mail">
				{#each threads as t, i (t.agentId)}
					<button
						class="agent-item"
						class:selected={t.agentId === selectedId}
						onclick={() => select(t.agentId)}
						in:flyIn|global={{ index: i + 1, duration: 350, stride: 25 }}
					>
						<span class="agent-item-main">
							<span class="agent-name">{t.name}</span>
							<span class="agent-latest">{t.latest.preview}</span>
						</span>
						<span class="agent-badges">
							{#if $pausedAgents.has(t.agentId)}<span class="chip">PAUSED</span>{/if}
							{#if t.attention > 0}<span class="chip chip-amber" title="Mail that needs a look">{t.attention}</span>{/if}
							{#if t.queued > 0}<span class="chip" title="Queued mail">{t.queued} Q</span>{/if}
						</span>
					</button>
				{/each}
			</aside>

			{#if thread}
				<section class="thread" aria-label="Mail with {thread.name}">
					<header class="thread-header">
						<span class="thread-name">{thread.name}</span>
						<span class="thread-id">{thread.agentId}</span>
						{#if paused}<span class="chip chip-amber" title="Agent mail to this agent waits. Your mail still goes.">PAUSED</span>{/if}
						<button
							class="action-btn"
							disabled={busy.pause}
							onclick={() => run('pause', () => setMailPaused(thread!.agentId, !paused))}
							title={paused ? 'Resume agent mail to this agent' : 'Hold agent mail to this agent. Your own mail still goes.'}
						>
							{#if paused}
								<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><polygon points="6 4 20 12 6 20 6 4" /></svg>
								Resume mail
							{:else}
								<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><line x1="9" y1="5" x2="9" y2="19" /><line x1="15" y1="5" x2="15" y2="19" /></svg>
								Pause mail
							{/if}
						</button>
					</header>
					{#if errors.pause}<div class="action-error">{errors.pause}</div>{/if}

					<div class="messages" bind:this={listEl}>
						{#each thread.messages as m (m.msgId)}
							{@const source = sourceOf(m, $ownMail)}
							{@const route = routeLabel(m.via)}
							{#if source === 'engine'}
								<div class="system-row" class:flash={flashId === m.msgId} data-msg-id={m.msgId}>
									<span class="system-tag">ENGINE</span>
									<span class="system-text">{m.preview}</span>
									{#if m.noticeId}
										<button class="link-btn" onclick={() => jump(m.noticeId)}>about {previewOf(m.noticeId)?.slice(0, 40) ?? m.noticeId}</button>
									{/if}
									<span class="msg-time">{time(m.createdAt)}</span>
								</div>
							{:else}
								<article
									class="msg"
									class:human={source === 'you' || source === 'terminal'}
									class:flash={flashId === m.msgId}
									data-msg-id={m.msgId}
								>
									<div class="msg-head">
										<span class="msg-source" class:you={source === 'you'}>{sourceLabel(source, m)}</span>
										<span class="msg-arrow">→ {m.to.name ?? m.to.id}</span>
										{#if m.kind !== 'request'}<span class="tag">{m.kind.toUpperCase()}</span>{/if}
										<span class="msg-time">{time(m.createdAt)}</span>
									</div>
									{#if m.replyTo}
										<button class="link-btn reply-link" onclick={() => jump(m.replyTo)}>
											↩ reply to {previewOf(m.replyTo)?.slice(0, 60) ?? m.replyTo}
										</button>
									{/if}
									<div class="msg-body">{bodies[m.msgId] ?? m.preview}</div>
									{#if !bodies[m.msgId] && [...m.preview].length >= 200}
										<button class="link-btn" onclick={() => showFull(m)}>Show full mail</button>
									{/if}
									<div class="msg-foot">
										<span class={chipClass(m)} title={m.detail ?? m.state}>{stateLabel(m)}</span>
										{#if isHeld(m)}
											<span class="held-note">waiting for approval in {m.to.name ?? m.to.id}'s Claude Code</span>
										{:else if m.detail && (m.state === 'failed' || m.state === 'uncertain' || m.state === 'queued')}
											<span class="detail">{m.detail}</span>
										{/if}
										{#if route}<span class="tag" title={ROUTE_HELP[route] ?? route}>{route}</span>{/if}
										{#if m.state === 'queued'}
											<button class="action-btn small" disabled={busy[m.msgId]} onclick={() => run(m.msgId, () => cancelMail(m.msgId))} title="Cancel this queued mail">
												<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><line x1="18" y1="6" x2="6" y2="18" /><line x1="6" y1="6" x2="18" y2="18" /></svg>
												Cancel
											</button>
										{/if}
									</div>
									{#if errors[m.msgId]}<div class="action-error">{errors[m.msgId]}</div>{/if}
								</article>
							{/if}
						{/each}
					</div>

					<form class="composer" onsubmit={(e) => { e.preventDefault(); void send(); }}>
						<label class="composer-label" for="mail-draft">[you] → {thread.name}</label>
						<div class="composer-row">
							<textarea
								id="mail-draft"
								rows="2"
								placeholder="Write to {thread.name}. The engine pastes it into the session. ⌘↵ sends."
								bind:value={draft}
								onkeydown={onComposerKey}
								disabled={sending}
							></textarea>
							<button class="action-btn send-btn" type="submit" disabled={sending || !draft.trim()} title="Send (⌘↵)">
								<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><line x1="22" y1="2" x2="11" y2="13" /><polygon points="22 2 15 22 11 13 2 9 22 2" /></svg>
								{sending ? 'Sending' : 'Send'}
							</button>
						</div>
						{#if errors.send}<div class="action-error">{errors.send}</div>{/if}
					</form>
				</section>
			{/if}
		</div>
	{/if}
</div>

<style>
	.mail-tab { display: flex; flex-direction: column; height: 100%; overflow: hidden; }

	/* ── Section header (matches HISTORY / COST / MEMORY) ────────── */
	.section-header {
		display: flex; align-items: center; gap: var(--space-md);
		padding-bottom: var(--space-md); border-bottom: 1px solid var(--text-primary);
		margin-bottom: var(--space-md); flex-shrink: 0;
	}
	.section-title {
		font-family: var(--font-pixel); font-size: 22px; font-weight: 600; color: var(--text-primary);
		text-transform: uppercase; letter-spacing: 0.1em; line-height: 1;
	}
	.section-count { font-family: var(--font-pixel); font-size: 18px; font-weight: 500; line-height: 1; color: var(--text-secondary); }

	/* ── Loading / empty states ──────────────────────────────────── */
	.state-msg {
		font-family: var(--font-mono); font-size: 13px; color: var(--text-muted); text-transform: uppercase;
		letter-spacing: 0.05em; padding: var(--space-xl) 0; text-align: center;
	}
	.state-msg .loading-spinner { animation: spin 1s linear infinite; display: inline-block; margin-right: var(--space-xs); }
	@keyframes spin { from { transform: rotate(0deg); } to { transform: rotate(360deg); } }
	.empty-hint { font-family: var(--font-mono); font-size: 11px; color: var(--text-muted); margin-top: var(--space-sm); text-align: center; }
	.stale-note {
		font-family: var(--font-mono); font-size: 11px; color: var(--accent-amber);
		padding: var(--space-xs) 0 var(--space-sm); flex-shrink: 0;
	}

	/* ── Two-panel layout ────────────────────────────────────────── */
	.two-panel { display: flex; flex: 1; overflow: hidden; min-height: 0; }
	.agent-list { width: 220px; min-width: 160px; border-right: 1px solid var(--border-default); overflow-y: auto; flex-shrink: 0; }
	.agent-item {
		display: flex; align-items: center; justify-content: space-between; gap: var(--space-sm);
		width: 100%; padding: var(--space-sm) var(--space-md); background: none; border: none;
		border-bottom: 1px solid var(--border-default); color: var(--text-muted); cursor: pointer;
		text-align: left; font-family: var(--font-mono); font-size: 12px; transition: all 0.15s ease;
	}
	.agent-item:hover { background: rgba(255, 255, 255, 0.03); color: var(--text-primary); }
	.agent-item.selected { background: rgba(255, 255, 255, 0.08); color: var(--text-primary); border-left: 2px solid var(--text-primary); }
	.agent-item-main { display: flex; flex-direction: column; gap: 2px; min-width: 0; }
	.agent-name { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
	.agent-latest { font-family: var(--font-sans); font-size: 11px; color: var(--text-muted); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
	.agent-badges { display: flex; gap: var(--space-xs); flex-shrink: 0; }

	/* ── Thread ──────────────────────────────────────────────────── */
	.thread { flex: 1; display: flex; flex-direction: column; min-width: 0; }
	.thread-header {
		display: flex; align-items: center; gap: var(--space-sm); padding: 0 var(--space-lg) var(--space-sm);
		border-bottom: 1px solid var(--border-default); flex-shrink: 0;
	}
	.thread-name { font-family: var(--font-mono); font-size: 13px; color: var(--text-primary); }
	.thread-id { font-family: var(--font-mono); font-size: 11px; color: var(--text-muted); margin-right: auto; }

	.messages { flex: 1; overflow-y: auto; padding: var(--space-md) var(--space-lg); display: flex; flex-direction: column; gap: var(--space-md); }
	.msg {
		max-width: 78%; align-self: flex-start; background: var(--bg-card); border: 1px solid var(--border-default);
		border-left: 2px solid var(--accent-purple); padding: var(--space-sm) var(--space-md);
		display: flex; flex-direction: column; gap: var(--space-xs); transition: box-shadow 0.3s ease;
	}
	.msg.human { align-self: flex-end; border-left: 1px solid var(--border-default); border-right: 2px solid var(--text-primary); }
	.msg.flash, .system-row.flash { box-shadow: 0 0 0 1px var(--border-focus); }
	.msg-head { display: flex; align-items: baseline; gap: var(--space-sm); font-family: var(--font-mono); font-size: 11px; color: var(--text-muted); flex-wrap: wrap; }
	.msg-source { color: var(--text-secondary); }
	.msg-source.you { color: var(--text-primary); }
	.msg-time { margin-left: auto; color: var(--text-muted); font-family: var(--font-mono); font-size: 10px; }
	.msg-body { font-family: var(--font-sans); font-size: 13px; line-height: 1.5; color: var(--text-description); white-space: pre-wrap; overflow-wrap: anywhere; }
	.msg-foot { display: flex; align-items: center; gap: var(--space-sm); flex-wrap: wrap; }
	.detail, .held-note { font-family: var(--font-mono); font-size: 10px; color: var(--text-muted); }
	.held-note { color: var(--accent-amber); }

	.system-row {
		align-self: center; display: flex; align-items: baseline; gap: var(--space-sm); max-width: 90%;
		font-family: var(--font-mono); font-size: 11px; color: var(--text-muted);
		border-top: 1px dashed var(--border-muted); border-bottom: 1px dashed var(--border-muted); padding: var(--space-xs) var(--space-sm);
	}
	.system-tag { color: var(--text-secondary); letter-spacing: 0.05em; }
	.system-text { overflow-wrap: anywhere; }

	/* ── Chips and tags ──────────────────────────────────────────── */
	.chip, .tag {
		font-family: var(--font-mono); font-size: 10px; text-transform: uppercase; letter-spacing: 0.05em;
		padding: 1px var(--space-xs); border: 1px solid var(--border-default); color: var(--text-muted); white-space: nowrap;
	}
	.tag { border-style: dashed; cursor: help; }
	.chip-amber { color: var(--accent-amber); border-color: var(--accent-amber); }
	.chip-red { color: var(--accent-red); border-color: var(--accent-red); }

	.link-btn {
		background: none; border: none; padding: 0; cursor: pointer; text-align: left;
		font-family: var(--font-mono); font-size: 11px; color: var(--text-secondary);
		overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 100%;
	}
	.link-btn:hover { color: var(--text-primary); text-decoration: underline; }

	.action-btn {
		display: inline-flex; align-items: center; gap: var(--space-xs); background: none;
		border: 1px solid var(--border-default); color: var(--text-secondary); cursor: pointer;
		font-family: var(--font-mono); font-size: 11px; text-transform: uppercase; letter-spacing: 0.05em;
		padding: var(--space-xs) var(--space-sm); transition: all 0.15s ease;
	}
	.action-btn:hover:not(:disabled) { color: var(--text-primary); border-color: var(--text-primary); }
	.action-btn:disabled { opacity: 0.4; cursor: default; }
	.action-btn.small { padding: 0 var(--space-xs); font-size: 10px; }
	.action-error { font-family: var(--font-mono); font-size: 11px; color: var(--accent-red); padding: var(--space-xs) var(--space-lg); }
	.msg .action-error { padding: 0; }

	/* ── Composer ────────────────────────────────────────────────── */
	.composer { border-top: 1px solid var(--border-default); padding: var(--space-sm) var(--space-lg) 0; flex-shrink: 0; }
	.composer-label { display: block; font-family: var(--font-mono); font-size: 11px; color: var(--text-primary); margin-bottom: var(--space-xs); }
	.composer-row { display: flex; gap: var(--space-sm); align-items: stretch; }
	textarea {
		flex: 1; resize: vertical; min-height: 44px; max-height: 200px; background: var(--bg-elevated);
		border: 1px solid var(--border-default); color: var(--text-primary); font-family: var(--font-sans);
		font-size: 13px; padding: var(--space-sm);
	}
	textarea:focus { outline: none; border-color: var(--border-focus); }
	.send-btn { align-self: stretch; }

	@media (max-width: 640px) {
		.agent-list { width: 140px; min-width: 120px; }
		.msg { max-width: 100%; }
	}
</style>
