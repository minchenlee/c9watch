<script lang="ts">
 import { codexInteractions, hasCodexThread } from '$lib/stores/codex-interactions';
 import CodexQuestionCard from './CodexQuestionCard.svelte';
 import CodexApprovalCard from './CodexApprovalCard.svelte';
 import CodexTurnProgress from './CodexTurnProgress.svelte';
 let { sessionId }: { sessionId: string } = $props();
 const relevant = $derived($codexInteractions.filter(s => hasCodexThread(s, sessionId)));
 const cards = $derived(relevant.flatMap(s => s.pending.filter(p => p.threadId === sessionId).map(request => ({ endpoint: s.endpoint, connected: s.connected, request }))));
 const progress = $derived(relevant.filter(s => s.turns?.[sessionId]?.plan?.length || s.turns?.[sessionId]?.diff));
 const ambiguous = $derived(relevant.filter(s => s.connected).length > 1);
 const waitingWithoutDetails = $derived(!cards.length && relevant.some(s => s.connected && s.statuses[sessionId] === 'waiting'));
</script>
{#if cards.length || waitingWithoutDetails || progress.length}
 <section class="pending" aria-label="Pending Codex requests">
  {#if cards.length || waitingWithoutDetails}<div class="heading">WAITING · {cards.length || 'CODEX'}</div>{/if}
  {#each cards as card (`${card.endpoint}:${card.request.token}`)}
   {#if card.request.kind === 'question'}<CodexQuestionCard {...card} {ambiguous} />{:else}<CodexApprovalCard {...card} {ambiguous} />{/if}
  {/each}
  {#each progress as snapshot (`${snapshot.endpoint}:${snapshot.turns![sessionId].turnId}`)}
   <CodexTurnProgress progress={snapshot.turns![sessionId]} />
  {/each}
  {#if waitingWithoutDetails}<p>Codex is waiting for you. Open the original conversation for details.</p>{/if}
 </section>
{/if}
<style>
 .pending { flex-shrink: 0; max-height: 40vh; overflow-y: auto; border-top: 1px solid var(--border-default); padding: 8px 16px; display: grid; gap: 8px; }
 .heading { font: 10px var(--font-mono); color: var(--text-secondary); letter-spacing: .08em; }
 p { font-size: 12px; color: var(--text-secondary); }
</style>
