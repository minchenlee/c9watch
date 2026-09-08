<script lang="ts">
 import { invoke } from '@tauri-apps/api/core';
 import { canAnswer, refreshCodexInteractions, dismissDisconnected, type PendingInteraction } from '$lib/stores/codex-interactions';
 let { request, endpoint, connected, ambiguous = false }: { request: PendingInteraction; endpoint: string; connected: boolean; ambiguous?: boolean } = $props();
 let answers = $state<Record<string, string>>({});
 let other = $state<Record<string, boolean>>({});
 let sending = $state(false);
 let submitted = $state(false);
 let unknown = $state(false);
 let notice = $state('');
 const editable = $derived(connected && !ambiguous && request.answerable && !request.submitted && !sending && !submitted && !unknown);
 const complete = $derived(request.questions.length > 0 && request.questions.every(q => answers[q.id]?.trim()));
 async function submit() {
  if (!editable || !complete || !canAnswer(endpoint, request)) return;
  sending = true;
  notice = '';
  try {
   const receipt = await invoke<{ status: string; detail: string }>('answer_codex_question', {
    endpoint, token: request.token, threadId: request.threadId,
    answers: Object.fromEntries(request.questions.map(q => [q.id, { answers: [answers[q.id]] }]))
   });
   submitted = receipt.status === 'submitted';
   unknown = receipt.status === 'unknown';
   notice = receipt.detail;
  } catch {
   unknown = true;
   notice = 'Could not confirm delivery. Check the original Codex question; this answer will not be resent.';
  } finally { sending = false; void refreshCodexInteractions(); }
 }
</script>
<article class="request" aria-label={request.kind === 'question' ? 'Codex question' : 'Codex approval'}>
 <div class="request-heading"><span class="square"></span><span>{request.kind === 'question' ? 'WAITING FOR ANSWER' : 'WAITING FOR APPROVAL'}</span></div>
 {#if request.kind === 'question' && request.questions.length}
  {#each request.questions as question}
   <fieldset disabled={!editable}>
    <legend>{question.question}</legend>
    {#if question.isSecret}
     <p>This question contains a private answer. Respond in Codex.</p>
    {:else}
     {#each question.options as option}
      <button class="option" class:selected={!other[question.id] && answers[question.id] === option.label} aria-pressed={!other[question.id] && answers[question.id] === option.label} onclick={() => { other[question.id] = false; answers[question.id] = option.label; }}>
       <span class="marker"></span><span>{option.label}{#if option.description}<small>{option.description}</small>{/if}</span>
      </button>
     {/each}
     {#if question.options.length && question.isOther}
      <button class="option" class:selected={other[question.id]} aria-pressed={!!other[question.id]} onclick={() => { other[question.id] = true; answers[question.id] = ''; }}><span class="marker"></span>Other answer</button>
     {/if}
     {#if !question.options.length || other[question.id]}
      <textarea aria-label={`Answer: ${question.question}`} rows="2" maxlength="32768" placeholder="Your answer…" value={answers[question.id] ?? ''} oninput={e => answers[question.id] = e.currentTarget.value}></textarea>
     {/if}
    {/if}
   </fieldset>
  {/each}
 {:else}<p>{request.summary}</p>{/if}
 {#if !connected}
  <p role="status">Connection lost. This request may have been resolved in Codex.</p>
  <button onclick={() => dismissDisconnected(endpoint, request.token)}>DISMISS STALE CARD</button>
 {:else if ambiguous}<p role="status">Multiple connections have this session. Answer in Codex.</p>
 {:else if request.submitted || submitted}<p role="status">Answer submitted. Waiting for Codex to clear the request.</p>
 {:else if request.answerable}<button class="submit" onclick={submit} disabled={!editable || !complete}>{sending ? 'SUBMITTING…' : 'SUBMIT ANSWERS'}</button>
 {:else}<p>Complete this request in the original Codex window.</p>{/if}
 {#if notice}<p role="status">{notice}</p>{/if}
</article>
<style>
 .request { border: 1px solid var(--border-default); padding: 12px; background: var(--bg-card); }
 .request-heading { display: flex; gap: 8px; align-items: center; font: 10px var(--font-mono); letter-spacing: .06em; color: var(--text-secondary); }
 .square { width: 6px; height: 6px; background: var(--status-input); }
 fieldset { border: 0; padding: 0; margin: 12px 0; min-width: 0; }
 legend { font-size: 13px; line-height: 1.5; margin-bottom: 8px; overflow-wrap: anywhere; }
 button, textarea { border: 1px solid var(--border-default); border-radius: 0; color: var(--text-primary); background: transparent; font: 12px var(--font-mono); padding: 8px; }
 button { cursor: pointer; }
 button:disabled { opacity: .5; cursor: default; }
 .option { display: flex; align-items: baseline; gap: 8px; width: 100%; text-align: left; margin: 4px 0; overflow-wrap: anywhere; }
 .marker { display: inline-block; flex-shrink: 0; width: 8px; height: 8px; border: 1px solid var(--border-default); }
 .selected { border-color: var(--text-secondary); }
 .selected .marker { background: var(--text-primary); box-shadow: inset 0 0 0 2px var(--bg-card); }
 small { display: block; font: 12px var(--font-body, sans-serif); color: var(--text-secondary); margin-top: 4px; }
 textarea { box-sizing: border-box; width: 100%; resize: vertical; margin-top: 6px; }
 button:focus-visible, textarea:focus-visible { outline: 1px solid var(--text-primary); outline-offset: 2px; }
 p { font-size: 12px; line-height: 1.5; color: var(--text-secondary); overflow-wrap: anywhere; }
</style>
