import { writable, get } from 'svelte/store';
import type { FormSchema } from '$lib/codex-forms';
import { invoke } from '@tauri-apps/api/core';
import { SessionStatus, type Session } from '$lib/types';

export interface Question {
 id: string; header: string; question: string;
 options: { label: string; description: string }[];
 isOther: boolean; isSecret: boolean;
}
export interface InteractionDetails {
 command?: string; cwd?: string; reason?: string; kind?: string; grantRoot?: string;
 environmentId?: string; additionalPermissions?: unknown; networkApprovalContext?: { host?: string; protocol?: string; port?: number };
 permissions?: unknown; choices?: { id: string; label: string }[];
 changes?: { path: string; diff: string; kind: { type: string; move_path?: string | null } }[];
 serverName?: string; mode?: string; message?: string; url?: string;
 requestedSchema?: FormSchema; supportedForm?: boolean; safeUrl?: boolean;
}
export interface TurnProgress {
 turnId: string; status: string; stopping: boolean;
 plan: { step: string; status: string }[] | null; explanation: string;
 diff: string; diffTruncated: boolean;
}
export interface PendingInteraction {
 token: string; threadId: string; turnId: string;
 kind: string; summary: string; questions: Question[];
 submitted: boolean; answerable: boolean; details?: InteractionDetails; actions?: string[];
}
export interface InteractionSnapshot {
 endpoint: string; connected: boolean; pending: PendingInteraction[];
 statuses: Record<string, string>; overflow: boolean; turns?: Record<string, TurnProgress>;
}
export const codexInteractions = writable<InteractionSnapshot[]>([]);

// Preserve disconnected cards only until the user dismisses them, with a hard bound.
export function mergeSnapshots(previous: InteractionSnapshot[], fresh: InteractionSnapshot[]) {
 const result = fresh.map(s => s.connected ? s : {
  ...s, pending: previous.find(p => p.endpoint === s.endpoint)?.pending ?? [], statuses: {}, turns: previous.find(p => p.endpoint === s.endpoint)?.turns ?? {}
 });
 for (const old of previous) {
  if ((old.pending.length || Object.keys(old.turns ?? {}).length) && !result.some(s => s.endpoint === old.endpoint)) {
   result.push({ ...old, connected: false, statuses: {} });
  }
 }
 return result.slice(0, 16);
}
export function projectCodexSession(session: Session, snapshots: InteractionSnapshot[]): Session {
 if (session.provider !== 'codex') return session;
 const candidates = snapshots.filter(s => s.pending.some(p => p.threadId === session.id) || session.id in s.statuses);
 const pending = candidates.flatMap(s => s.pending.filter(p => p.threadId === session.id));
 if (pending.length) {
  const disconnected = candidates.some(s => !s.connected);
  const ambiguous = candidates.filter(s => s.connected).length > 1;
  const reason = disconnected ? 'Connection lost · check Codex' : ambiguous ? 'Multiple connections · check Codex'
   : pending.some(p => ['command', 'file', 'permission'].includes(p.kind)) ? `Waiting for approval · ${pending.length}` : pending.some(p => p.kind === 'form') ? `Waiting for form · ${pending.length}` : `Waiting for answer · ${pending.length}`;
  return { ...session, status: SessionStatus.WaitingForInput, pendingToolName: reason };
 }
 if (candidates.length !== 1 || !candidates[0].connected) return session;
 const status = candidates[0].statuses[session.id];
 if (status === 'waiting') return { ...session, status: SessionStatus.WaitingForInput, pendingToolName: 'Waiting · open Codex for details' };
 if (status === 'active') return { ...session, status: SessionStatus.Working };
 if (status === 'idle') return { ...session, status: SessionStatus.WaitingForInput };
 return session;
}
let running = false;
let refreshing: Promise<void> | undefined;
export function refreshCodexInteractions(): Promise<void> {
 if (refreshing) return refreshing;
 refreshing = (async () => {
  try {
   const fresh = await invoke<InteractionSnapshot[]>('codex_interaction_snapshots');
   codexInteractions.update(previous => mergeSnapshots(previous, fresh));
  } catch {
   codexInteractions.update(previous => mergeSnapshots(previous, []));
  }
 })().finally(() => { refreshing = undefined; });
 return refreshing;
}
export function startCodexInteractions() {
 if (running) return;
 running = true;
 async function poll() {
  await refreshCodexInteractions();
  if (running) setTimeout(poll, 2000);
 }
 void poll();
}
export function dismissDisconnected(endpoint: string, token: string) {
 codexInteractions.update(all => all.map(s => s.endpoint === endpoint && !s.connected
  ? { ...s, pending: s.pending.filter(p => p.token !== token) } : s));
}
export function canAnswer(endpoint: string, request: PendingInteraction) {
 const all = get(codexInteractions);
 const matching = all.filter(s => s.connected && (s.pending.some(p => p.threadId === request.threadId) || request.threadId in s.statuses));
 return matching.length === 1 && matching[0].endpoint === endpoint && matching[0].pending.some(p => p.token === request.token && p.answerable && !p.submitted);
}

export function canDecide(endpoint: string, request: PendingInteraction, action: string) {
 const matching = get(codexInteractions).filter(s => s.connected && (s.pending.some(p => p.threadId === request.threadId) || request.threadId in s.statuses));
 return matching.length === 1 && matching[0].endpoint === endpoint && matching[0].pending.some(p => p.token === request.token && !p.submitted && p.actions?.includes(action));
}
