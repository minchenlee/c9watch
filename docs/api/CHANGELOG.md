> **Preview.** A breaking preview entry must say "breaking (preview)" and give each affected method's new `rev`. Check `hello.breakingSince`.

# c9p/1 changelog

## Leaving preview

c9p 1 leaves preview when both conditions are true:

1. Gateway public-key pairing ships.
2. Two weeks pass after that with no breaking change.

After that, major 1 accepts additive changes only. A breaking change needs major 2.

When preview ends, the GUI's legacy in-process fallback is removed. The GUI then reads only through core.

## 1.15 — upcoming

- Put the engine home's `ext-data/` directory under every agent's write floor. An extension's core half runs as `plugin:<id>` and keeps its state in `<engine home>/ext-data/<id>/`, so an agent must not write there. Core's own entries in a spawn's `permissions.deny` add `Edit(//<engine home>/ext-data/**)`, and `sandbox.filesystem.denyWrite` adds `<engine home>/ext-data`, next to `bin/`, `spawn/` and `extensions/`. A running agent gets them at its next spawn or resume. Core does not create the directory, and nothing reads or writes it yet. See [c9p-1.md](c9p-1.md#spawn-overlay-114). No method `rev` changes.
- `AgentRow` adds `mailPaused` (boolean, always sent): `true` while agent-to-agent mail to the agent is paused by `mail.pause`, `false` after `mail.resume` or when it was never paused. `agent.list`, `session.list`, `snapshot.get` and the `agent.create` result carry it. It is not the `paused` lifecycle `state`, which means a drained agent. A client of an older engine gets no field and does not know the pause state. See [c9p-1.md](c9p-1.md#mail-pause-on-the-agent-row-115). Additive; no method `rev` changes.

## 1.14 — upcoming

- Put the engine home's `extensions/` directory under every agent's write floor (D31). Core's own entries in a spawn's `permissions.deny` add `Edit(//<engine home>/extensions/**)`, and `sandbox.filesystem.denyWrite` adds `<engine home>/extensions`, next to `bin/` and `spawn/`. A running agent gets them at its next spawn or resume. Core does not create the directory. See [c9p-1.md](c9p-1.md#spawn-overlay-114). No method `rev` changes.
- Add the spawn overlay (E1). `agent.create` accepts optional `overlay` (`AgentOverlay`: `model`, `permissionsDeny`, `sandboxDenyWrite`, `standingOrders`; unknown fields are `bad_request`). Each field only adds to core's own spawn: `--model`, rules after core's in `permissions.deny`, paths after core's in `sandbox.filesystem.denyWrite`, and caller text after core's standing orders, fenced and labelled. Limits: model 1 to 64 of `[A-Za-z0-9._[]-]`, not starting with `-`; at most 64 deny rules of 1 to 512 bytes, with no `!` and, for a path tool (`Read`, `Edit`, `Write`, `MultiEdit`, `NotebookEdit`, `NotebookRead`, `Glob`, `Grep`, `LS`), a `//<abs>` or `~/` specifier (else `not_allowed`); at most 64 absolute deny-write paths (realpath when they exist; no quote, parenthesis, backslash, glob or control character); orders at most 8 KiB. An agent may pass all but `model`; its child always gets the parent's `model`, `permissionsDeny` and `sandboxDenyWrite`. `AgentRow` adds optional `overlay`; a resume applies the same one. The `agent.create` audit row records the overlay, or its sha256 and field counts when it is longer than 1024 bytes. Without `overlay` a create is unchanged. See [c9p-1.md](c9p-1.md#spawn-overlay-114). Additive; no method `rev` changes.

## 1.13 — upcoming

- Add agent creates agent (E5, D16). An engine agent may call `agent.create` and `agent.retire` for its own children, only from core's `c9watch-agent` binary (`child create`, `child retire`) as its current generation; any other process inside it is `peer_not_authorized`. A child must bring a `guard` at least as strict as its parent's, in a cwd inside the parent's write area (not the parent's cwd, not in `.claude` or `.git`); an agent may pass only `kind`, `mode`, `cwd`, `name`, `guard` and `require`. `AgentRow.parentId` names the parent. Limits: depth 3, 8 live children per parent, 16 live agents per tree. An agent retires only its descendants. A child loads no setting source and no `.mcp.json` (`--setting-sources ""`, `--strict-mcp-config`), only core's `--settings`. Every spawn starts in the cwd it was checked in, opened without following a symlink. When a parent is retired or failed, core retires its tree (audit reason `parent-gone`); a paused or interrupted parent keeps it. See [c9p-1.md](c9p-1.md#agent-creates-agent-113). Additive; no method `rev` changes.
- Add durable provenance (D8, D34). Core remembers, by pid and start time, each process it saw descend from an agent, an observed session or a teammate, and keeps it after the process is reparented under launchd or init. Such a process is stamped from its origin instead of `local-unverified`; a gone agent's is `observed:gone-agent:<id>` and one the full table had to keep out is `observed:unknown-origin:<pid>`, both read-only. A process under no agent, session or teammate stays `local-unverified`. Terminal writes refuse an ancestry with a process of another origin. See [c9p-1.md](c9p-1.md#principals-and-access). Additive; no method `rev` changes.
- Add agent-team teammates. `session.views` and `session.view` list live Claude Code teammates, which `claude agents --json` omits. `SessionView` adds optional `teammate` (`TeammateLink`: `agentName`, `teamName`, `leadSessionId`, optional `agentType`). A teammate's transcript is attached only when its first entry names the same agent and team and starts after the process; otherwise the teammate is not listed. Teammates have `canOpen`, `canStop` and `canRename` `false`. A process inside a teammate is `observed:<sessionKey>` (D8); inside a teammate core has not listed, it is `observed:unlisted-teammate:<pid>` and may call only read methods; an ancestor whose arguments core cannot read fails closed the same way, and every terminal write walks the whole ancestry, past a nearer listed session. `agentName` and `teamName` are conversation sensitivity. See [c9p-1.md](c9p-1.md#agent-team-teammates-113). Additive; no method `rev` changes.

## 1.12 — upcoming

- Add an agent write fence (D30). `agent.create` accepts optional `guard {denyWriteUnder, allowWrite}` and `require` (capability names; a missing one is `unsupported`). `AgentRow` adds optional `guard`, `ccVersion` and `capabilities`. A fenced agent gets a blocking Claude Code PreToolUse hook for `Write|Edit|MultiEdit|NotebookEdit`, and core answers it through the new internal `hook.pre_tool_use` (result `{decision, reason}`). Each deny is audited. With a fence on both sides, agents may run in sibling cwds. See [c9p-1.md](c9p-1.md#write-fence-and-required-capabilities-112). Additive; no method `rev` changes.

## 1.11 — upcoming

- Breaking (preview), gateway handshake only; no c9p method or `rev` changes. `/c9p` authentication is Ed25519 public-key pairing. `pair` sends `{code,publicKey,clientNonce,sig}` and receives `{paired:{clientId,serverKey,machineId,sig}}`; `auth` sends `{clientId,clientNonce,sig}`. The bearer token and the HMAC reconnect proof are removed. Nonces, keys and signatures are lowercase hex of exact length; anything else fails closed. See [gateway.md](gateway.md#encoding). SDK `Credentials` replaces `token` with `privateKey` (a `CryptoKey`).
- Responses on one connection may arrive out of order. Clients match them by `id`. Core handles requests on one connection concurrently, at most 32 in flight per connection.
- `agent.create`, `agent.retire` and `session.stop` accept `idem`, as `msg.send` does. A repeated `idem` with the same params replays the original result; other params are `duplicate`. The key lives in core's memory for 24 hours.
- Annotation namespaces use the lowercase form of the owner ID, so every valid agent and client ID gets a namespace (the 43-byte limit no longer rejects them). New gateway client IDs are `cli_` plus a lowercase ULID.
- Add detector health. `snapshot.get` and `session.list` add optional `observedAt` (the last written detection pass) and `detectorHealth` (`DetectorHealth`: `state` ok/degraded/failed/starting, `reason`, `failedProviders`, `lastOkAt`, `lastAttemptAt`, `stale`, `staleAfterMs`). New preview topic `detect.health` carries `DetectorHealth` when the state, the failed providers or staleness change, and as a heartbeat every `staleAfterMs / 2` while passes are written. `observedAt` equals `detectorHealth.lastOkAt`. A failed detection no longer looks like a quiet machine. Additive; no method `rev` changes.
- Hook and CC-socket state changes reach the in-memory merge only after their transaction commits. A failed write leaves the merge unchanged, so a retried event writes the same turn end.
- Docs: `refreshing` and `retiring` are reserved states that core does not emit yet. `seq` can have gaps after view compaction; a gap is not a lost event.
- Core writes `orphaned` (`stateReason` `kill_unconfirmed`) when no kill can confirm that an agent's process ended: after a retire, after an agent's own exit, after a failed create or resume, and in boot reconcile. Before SIGKILL, core confirms the process by pid and start time. An `orphaned` agent keeps `goneAt` `null`, counts as live for the cwd rules of `agent.create`, and is not resumable. `agent.retire` kills it again, together with the processes that the unconfirmed kill left (up to 64), and the row becomes `retired` only when every one is confirmed gone. If the kill left more than 64, the row stays `orphaned` with `stateReason` `survivors_overflow`. Add `agent.retire {force: true}` (local client only, as `agent.retire` is): it kills as usual, then writes the tombstone and a `forced` audit row. Before this change, boot reconcile marked such a row `interrupted` and resumed it.
- Add preview `agent.resume {id}` (local client only). It resumes a `paused` or `interrupted` engine agent and answers when the agent is `ready`.
- A failed resume no longer ends the agent as `failed`. The row goes back to `paused` or `interrupted` with `stateReason` `resume_failed`, keeps its mail queue, and does not emit `agent.failed`. Boot behavior change: a boot skips these rows; `agent.resume` retries them. `failed` with `resume_failed` now means only that there is no session to resume.
- `hello` never waits for session detection. Before detection's first successful pass, the principal it reports is provisional and may narrow to `observed:<sessionKey>`. The first request that needs the caller's identity waits up to 3 s for detection, then runs under the new stamp or gets `busy` with its own `id`. If the process cannot be verified then, such requests get `peer_not_authorized`.
- A local subscriber whose mailbox check fails no longer receives `legacy.inbox` or `msg.state`; a subscribe that names one of them fails with the check's error.
- The Rust client skips a late response to a request that timed out, instead of failing the next call with an id mismatch.
- `events.subscribe` and `events.unsubscribe` on one connection run in the order sent, also when pipelined. A slow one does not hold up other requests.
- New principal `observed:<sessionKey>` for a process inside an observed session. It cannot create, retire or resume agents, stops only its own session, and sends mail that names its session. Before, such a process was `local-unverified`.
- The authorization table now declares every admission check, including the Unix-peer gate and the mail sender check. `x-authz.admission` lists more checks; `x-authz.principals` may list `observed`.

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
