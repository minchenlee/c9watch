<script lang="ts">
 import type { TurnProgress } from '$lib/stores/codex-interactions';
 let { progress }: { progress: TurnProgress } = $props();
 function statusLabel(status: string) { return ({ inProgress: 'Running', completed: 'Completed', interrupted: 'Stopped', failed: 'Failed', pending: 'Pending', in_progress: 'In progress' } as Record<string, string>)[status] ?? status; }
</script>
<section class="progress" aria-label="Codex turn progress">
 {#if progress.plan?.length}
  <details open><summary>Plan · {progress.plan.filter(s => s.status === 'completed').length}/{progress.plan.length}</summary>
   {#if progress.explanation}<p>{progress.explanation}</p>{/if}
   <ol>{#each progress.plan as step}<li><span class="step-status">{statusLabel(step.status)}</span> {step.step}</li>{/each}</ol>
  </details>
 {/if}
 {#if progress.diff}
  <details><summary>Latest turn changes{progress.diffTruncated ? ' · preview' : ''}</summary><pre>{progress.diff}</pre>{#if progress.diffTruncated}<p>Preview truncated. Review the full diff in Codex. This summary is not an approval request.</p>{/if}</details>
 {/if}
</section>
<style>
 .progress { border: 1px solid var(--border-default); padding: 10px 12px; background: var(--bg-card); min-width: 0; }
 p, summary, li { font-size: 12px; line-height: 1.5; overflow-wrap: anywhere; }
 p, .step-status { color: var(--text-secondary); }
 .step-status { font: 10px var(--font-mono); }
 details { margin-top: 8px; } summary { cursor: pointer; }
 ol { padding-left: 20px; margin: 8px 0; }
 pre { font: 11px/1.5 var(--font-mono); white-space: pre-wrap; overflow-wrap: anywhere; max-height: 240px; overflow: auto; }
</style>
