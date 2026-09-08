<script lang="ts">
 import { invoke } from '@tauri-apps/api/core';
 import { refreshCodexInteractions, type TurnProgress } from '$lib/stores/codex-interactions';
 let { progress, endpoint, sessionId, connected, ambiguous }: { progress: TurnProgress; endpoint: string; sessionId: string; connected: boolean; ambiguous: boolean } = $props();
 let sending = $state(false), submitted = $state(false), unknown = $state(false);
 let notice = $state('');
 function statusLabel(status: string) { return ({ inProgress: 'Running', completed: 'Completed', interrupted: 'Stopped', failed: 'Failed', pending: 'Pending', in_progress: 'In progress' } as Record<string, string>)[status] ?? status; }
 const active = $derived(progress.status === 'inProgress');
 async function stop() {
  if (!connected || ambiguous || !active || progress.stopping || sending || submitted || unknown) return;
  sending = true;
  try {
   const receipt = await invoke<{ status: string; detail: string }>('interrupt_codex_turn', { endpoint, threadId: sessionId, turnId: progress.turnId });
   submitted = receipt.status === 'submitted'; unknown = receipt.status === 'unknown'; notice = receipt.detail;
  } catch { unknown = true; notice = 'Stop delivery is unknown. Check the current turn in Codex.'; }
  finally { sending = false; void refreshCodexInteractions(); }
 }
</script>
<section class="progress" aria-label="Codex turn progress">
 <div class="heading"><span>TURN · {statusLabel(progress.status)}</span>{#if active}<button onclick={stop} disabled={!connected || ambiguous || progress.stopping || sending || submitted || unknown}>{progress.stopping || sending || submitted ? 'STOP REQUESTED…' : 'STOP CURRENT TURN'}</button>{/if}</div>
 {#if !connected}<p>Connection lost. Progress may be outdated.</p>{/if}
 {#if ambiguous}<p>Multiple connections have this session. Stop it in Codex.</p>{/if}
 {#if progress.plan?.length}
  <details open><summary>Plan · {progress.plan.filter(s => s.status === 'completed').length}/{progress.plan.length}</summary>
   {#if progress.explanation}<p>{progress.explanation}</p>{/if}
   <ol>{#each progress.plan as step}<li><span class="step-status">{statusLabel(step.status)}</span> {step.step}</li>{/each}</ol>
  </details>
 {/if}
 {#if progress.diff}
  <details><summary>Latest turn changes{progress.diffTruncated ? ' · preview' : ''}</summary><pre>{progress.diff}</pre>{#if progress.diffTruncated}<p>Preview truncated. Review the full diff in Codex. This summary is not an approval request.</p>{/if}</details>
 {/if}
 {#if notice}<p role="status">{notice}</p>{/if}
</section>
<style>
 .progress { border: 1px solid var(--border-default); padding: 10px 12px; background: var(--bg-card); min-width: 0; }
 .heading { display: flex; flex-wrap: wrap; gap: 8px; align-items: center; justify-content: space-between; font: 10px var(--font-mono); color: var(--text-secondary); }
 button { border: 1px solid var(--border-default); border-radius: 0; color: var(--text-primary); background: transparent; font: 10px var(--font-mono); padding: 7px; cursor: pointer; }
 button:disabled { opacity: .45; cursor: default; }
 p, summary, li { font-size: 12px; line-height: 1.5; overflow-wrap: anywhere; }
 p, .step-status { color: var(--text-secondary); }
 .step-status { font: 10px var(--font-mono); }
 details { margin-top: 8px; } summary { cursor: pointer; }
 ol { padding-left: 20px; margin: 8px 0; }
 pre { font: 11px/1.5 var(--font-mono); white-space: pre-wrap; overflow-wrap: anywhere; max-height: 240px; overflow: auto; }
 button:focus-visible { outline: 1px solid var(--text-primary); outline-offset: 2px; }
</style>
