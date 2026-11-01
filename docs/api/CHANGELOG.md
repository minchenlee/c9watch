> **Preview.** A breaking preview entry must say "breaking (preview)" and give each affected method's new `rev`. Check `hello.breakingSince`.

# c9p/1 changelog

## Leaving preview

c9p 1 leaves preview when both conditions are true:

1. Gateway public-key pairing ships.
2. Two weeks pass after that with no breaking change.

After that, major 1 accepts additive changes only. A breaking change needs major 2.

When preview ends, the GUI's legacy in-process fallback is removed. The GUI then reads only through core.

## 1.11 — upcoming

- Responses on one connection may arrive out of order. Clients match them by `id`. Core may handle requests on one connection concurrently.
- `agent.create`, `agent.retire` and `session.stop` accept `idem`, as `msg.send` does today. A repeated `idem` replays the original result.
- Annotation namespaces use the lowercase form of the owner ID, so every valid agent and client ID gets a namespace (the 43-byte limit no longer rejects them). New gateway client IDs are `cli_` plus a lowercase ULID.
- Add detector health. `snapshot.get` and `session.list` add optional `observedAt` (the last written detection pass) and `detectorHealth` (`DetectorHealth`: `state` ok/degraded/failed/starting, `reason`, `failedProviders`, `lastOkAt`, `lastAttemptAt`, `stale`, `staleAfterMs`). New preview topic `detect.health` carries `DetectorHealth` when the state, the failed providers or staleness change, and as a heartbeat every `staleAfterMs / 2` while passes are written. `observedAt` equals `detectorHealth.lastOkAt`. A failed detection no longer looks like a quiet machine. Additive; no method `rev` changes.
- Hook and CC-socket state changes reach the in-memory merge only after their transaction commits. A failed write leaves the merge unchanged, so a retried event writes the same turn end.
- Docs: `refreshing` and `retiring` are reserved states that core does not emit yet. `seq` can have gaps after view compaction; a gap is not a lost event.
- Core writes `orphaned` (`stateReason` `kill_unconfirmed`) when no kill can confirm that an agent's process ended: after a retire, after an agent's own exit, after a failed create or resume, and in boot reconcile. Before SIGKILL, core confirms the process by pid and start time. An `orphaned` agent keeps `goneAt` `null`, counts as live for the cwd rules of `agent.create`, and is not resumable. `agent.retire` kills it again, and the row becomes `retired` only when the kill is confirmed. Before this change, boot reconcile marked such a row `interrupted` and resumed it.
- Add preview `agent.resume {id}` (local client only). It resumes a `paused` or `interrupted` engine agent and answers when the agent is `ready`.
- A failed resume no longer ends the agent as `failed`. The row goes back to `paused` or `interrupted` with `stateReason` `resume_failed`, keeps its mail queue, and does not emit `agent.failed`. Boot behavior change: a boot skips these rows; `agent.resume` retries them. `failed` with `resume_failed` now means only that there is no session to resume.

## 1.10 — public error cleanup

- **breaking (preview):** rename the unsupported-operation error to `unsupported` in the engine, CLI and generated contracts. `agent.create` is now revision 2; clients built against minors 3–9 see it in `hello.breakingSince`. Internal `agent.handoff` also advances to revision 2.
- Correct the `unknown_method` hint to point to `hello.result.methods`, remove the internal reference from the `not_controlled` hint, and remove design codes from schema descriptions. A contract test prevents those codes from returning.
- Make hook-class discovery agent-only (`hook.*`, `task.done`, `agent.handoff`). Local callers still receive `peer_not_authorized` and exactly one refusal audit row.
- Skip the annotation sweep's writer transaction when no TTL, target grace, empty source or stale rate budget needs cleanup. Expiry still commits before observation, even if observation fails.

## 1.9 — L1 annotations

- Add preview `annotation.set`, `annotation.delete`, `annotation.patch` and `annotation.list`, capability `annotations`, and typed `annotation.changed`/`annotation.removed` topics.
- Enforce per-principal namespaces, typed values, patch sequence watermarks, per-key TTL, count/rate limits, hash-only write/refusal audit and schema-driven value redaction.
- Add `annotate set|delete|list`, annotation exports, and the opt-in data-layer store. Classic rendering is unchanged; gateway handshake remains disabled.

## 1.8 — authorization and gateway foundations

- Add central `x-authz`, principal-filtered discovery, `auth.can` and `auth.rules`; gateway transport remains disabled.

## 1.7 — local mailbox APIs

- Add `msg.list`, `msg.read`, human-only `msg.cancel`, `mail.pause` and `mail.resume`, ownership-filtered mail events and opt-in CC socket delivery.

## 1.6 — public preview tooling

- Add offline `c9watch api schema [--public]`, also in CLI-only builds.
- List known state, status, role, mode, control and source strings in the schema, with open enum markers. Generated types accept new strings.
- Add internal visibility markers to schema entries and selected `AgentRow` fields. Public export removes them and unused definitions. Wire fields are unchanged.
- Add this documentation, a private TypeScript `0.x` SDK, two examples and `c9watch export`.
- The 1.6 baseline includes typed topics, discovery, method revisions, limits, `events.unsubscribe`, `ping`, and the GUI data queries listed in the protocol page.
- Gateway and annotation support remain separate milestones. The legacy error-name cleanup also remains separate. No method shape or method revision changes in this tooling unit.

## 1.5

The baseline's discovery records session views, away counts, recheck, stop and rename as introduced in minor 5. There is no reconstructed release history beyond those method facts.

## 1.4

history not recorded

## 1.3

history not recorded

## 1.2

history not recorded

## 1.1

history not recorded
