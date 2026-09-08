<script lang="ts">
 import { invoke } from '@tauri-apps/api/core';
 import { openUrl } from '@tauri-apps/plugin-opener';
 import { canDecide, refreshCodexInteractions, dismissDisconnected, type PendingInteraction } from '$lib/stores/codex-interactions';
 import { formComplete, safeExternalUrl } from '$lib/codex-forms';
 import CodexMcpForm from './CodexMcpForm.svelte';
 let { request, endpoint, connected, ambiguous = false }: { request: PendingInteraction; endpoint: string; connected: boolean; ambiguous?: boolean } = $props();
 const d = $derived(request.details ?? {});
 let sending = $state(false), submitted = $state(false), unknown = $state(false);
 let reviewed = $state(false), completed = $state(false);
 let selected = $state<string[]>([]);
 let values = $state<Record<string, unknown>>({});
 let notice = $state('');
 const enabled = $derived(connected && !ambiguous && !request.submitted && !sending && !submitted && !unknown);
 const hasFormFields = $derived(Object.keys(d.requestedSchema?.properties ?? {}).length > 0);
 const title = $derived(request.kind === 'form' ? d.mode === 'url' ? 'MCP EXTERNAL STEP' : hasFormFields ? 'MCP FORM' : 'MCP APPROVAL' : request.kind === 'permission' ? 'PERMISSION APPROVAL' : request.kind === 'file' ? 'FILE CHANGE APPROVAL' : d.networkApprovalContext ? 'NETWORK APPROVAL' : d.kind === 'stdin' ? 'COMMAND INPUT APPROVAL' : 'COMMAND APPROVAL');
 function ready(action: string) {
  if (action === 'accept' && request.kind === 'file') return reviewed;
  if (action === 'grant') return selected.length > 0;
  if (action === 'accept' && request.kind === 'form') return d.mode === 'url' ? completed : formComplete(d.requestedSchema, values);
  return true;
 }
 function label(action: string) {
  if (action === 'accept') return request.kind === 'form' ? d.mode === 'url' ? 'CONFIRM COMPLETED' : hasFormFields ? 'SUBMIT ANSWERS' : 'APPROVE' : 'APPROVE ONCE';
  if (action === 'grant') return 'APPROVE SELECTED · THIS TURN';
  if (action === 'deny') return 'REJECT';
  return action === 'cancel' ? 'CANCEL' : 'REJECT';
 }
 async function decide(action: string) {
  if (!enabled || !ready(action)) return;
  if (!canDecide(endpoint, request, action)) {
   notice = 'This request changed or its connection is unavailable. Refreshing the current request; no response was sent.';
   void refreshCodexInteractions();
   return;
  }
  sending = true; notice = '';
  try {
   const receipt = await invoke<{ status: string; detail: string }>('decide_codex_interaction', {
    endpoint, token: request.token, threadId: request.threadId, action,
    input: { reviewed, selected, completed, ...(request.kind === 'form' && action === 'accept' && d.mode !== 'url' ? { content: values } : {}) }
   });
   submitted = receipt.status === 'submitted'; unknown = receipt.status === 'unknown'; notice = receipt.detail;
  } catch { unknown = true; notice = 'Delivery is unknown. Check Codex; this response will not be resent.'; }
  finally { sending = false; void refreshCodexInteractions(); }
 }
 async function openExternal() {
  if (!enabled || !d.url || !safeExternalUrl(d.url)) return;
  try { await openUrl(d.url); notice = 'Complete the external step, then return here. Opening the link does not approve this request.'; }
  catch { notice = 'Could not open the link. Continue in the original Codex window.'; }
 }
</script>
<article class="request" aria-label={title}>
 <div class="heading"><span class="square"></span>{title}</div>
 {#if d.serverName}<p>Server: <strong>{d.serverName}</strong></p>{/if}
 <p>{d.message ?? d.reason ?? request.summary}</p>
 {#if d.cwd}<p class="path">Working directory: {d.cwd}</p>{/if}
 {#if d.environmentId}<p>Environment: {d.environmentId}</p>{/if}
 {#if d.command}<pre aria-label="Command">{d.command}</pre>{/if}
 {#if d.networkApprovalContext}<p>Destination: <strong>{d.networkApprovalContext.protocol}://{d.networkApprovalContext.host}{d.networkApprovalContext.port ? `:${d.networkApprovalContext.port}` : ''}</strong></p>{/if}
 {#if d.additionalPermissions}<details open><summary>Additional permissions</summary><pre>{JSON.stringify(d.additionalPermissions, null, 2)}</pre></details>{/if}
 {#if request.kind === 'file'}
  {#if d.grantRoot}<p>Requested session-wide write scope: {d.grantRoot}. Review this scope in Codex.</p>{/if}
  {#each d.changes ?? [] as change}
   <details open><summary>{change.kind.type.toUpperCase()} · {change.path}{change.kind.move_path ? ` → ${change.kind.move_path}` : ''}</summary><pre aria-label={`Diff: ${change.path}`}>{change.diff || 'No text diff supplied'}</pre></details>
  {/each}
  {#if request.actions?.includes('accept')}<button class="choice" class:selected={reviewed} aria-pressed={reviewed} disabled={!enabled} onclick={() => reviewed = !reviewed}><span class="marker"></span>I reviewed the complete changes and paths</button>
  {:else}<p>Approval here requires a complete diff and a one-time scope. Review the proposal in Codex.</p>{/if}
 {:else if request.kind === 'permission'}
  <p>Choose permissions to approve for <strong>this turn only</strong>.</p>
  {#each d.choices ?? [] as choice}<button class="choice" class:selected={selected.includes(choice.id)} aria-pressed={selected.includes(choice.id)} disabled={!enabled} onclick={() => selected = selected.includes(choice.id) ? selected.filter(id => id !== choice.id) : [...selected, choice.id]}><span class="marker"></span>{choice.label}</button>{/each}
  <details><summary>Full requested scope (deny restrictions are preserved)</summary><pre>{JSON.stringify(d.permissions, null, 2)}</pre></details>
 {:else if request.kind === 'form'}
  {#if d.mode === 'url'}
   <p class="path">{d.url}</p>
   <button onclick={openExternal} disabled={!enabled || !d.safeUrl}>OPEN EXTERNAL STEP</button>
   <button class="choice" class:selected={completed} aria-pressed={completed} disabled={!enabled || !d.safeUrl} onclick={() => completed = !completed}><span class="marker"></span>I completed the external step</button>
  {:else if d.supportedForm && d.requestedSchema}
   <CodexMcpForm schema={d.requestedSchema} bind:values disabled={!enabled} />
  {:else}<p>Review this request in the original Codex client.</p>{/if}
 {/if}
 {#if !connected}<p role="status">Connection lost. Review the current request in Codex.</p><button onclick={() => dismissDisconnected(endpoint, request.token)}>DISMISS STALE CARD</button>
 {:else if ambiguous}<p role="status">Multiple connections have this session. Continue in Codex.</p>
 {:else if request.submitted || submitted}<p role="status">Response submitted. Waiting for Codex to clear the request.</p>
 {:else}<div class="actions">{#each request.actions ?? [] as action}<button onclick={() => decide(action)} disabled={!enabled || !ready(action)}>{sending ? 'SUBMITTING…' : label(action)}</button>{/each}</div>{/if}
 {#if request.kind === 'command' || request.kind === 'file'}<p class="hint">Reject skips this action. Cancel interrupts the turn.</p>{/if}
 {#if request.kind === 'form'}<p class="hint">Reject declines this request. Cancel dismisses it. Neither approves access.</p>{/if}
 {#if notice}<p role="status">{notice}</p>{/if}
</article>
<style>
 .request { border: 1px solid var(--border-default); padding: 12px; background: var(--bg-card); min-width: 0; }
 .heading { display: flex; gap: 8px; align-items: center; font: 10px var(--font-mono); letter-spacing: .06em; color: var(--text-secondary); }
 .square { width: 6px; height: 6px; background: var(--status-input); }
 p, summary { font-size: 12px; line-height: 1.5; color: var(--text-secondary); overflow-wrap: anywhere; }
 strong { color: var(--text-primary); }
 pre { font: 11px/1.5 var(--font-mono); white-space: pre-wrap; overflow-wrap: anywhere; max-height: 240px; overflow: auto; border: 1px solid var(--border-default); padding: 8px; }
 details { margin: 8px 0; } summary { cursor: pointer; }
 button { border: 1px solid var(--border-default); border-radius: 0; color: var(--text-primary); background: transparent; font: 11px var(--font-mono); padding: 8px; cursor: pointer; }
 button:disabled { opacity: .45; cursor: default; }
 .choice { width: 100%; display: flex; align-items: center; gap: 8px; text-align: left; margin: 6px 0; overflow-wrap: anywhere; }
 .marker { width: 8px; height: 8px; border: 1px solid var(--border-default); flex-shrink: 0; }
 .selected { border-color: var(--text-secondary); }
 .selected .marker { background: var(--text-primary); box-shadow: inset 0 0 0 2px var(--bg-card); }
 .actions { display: flex; gap: 6px; flex-wrap: wrap; margin-top: 12px; }
 .hint { font-size: 10px; } .path { font-family: var(--font-mono); }
 button:focus-visible { outline: 1px solid var(--text-primary); outline-offset: 2px; }
</style>
