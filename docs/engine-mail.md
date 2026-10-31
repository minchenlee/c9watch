# Engine mail (C1b-M2)

`msg.*` operates on messages; `mail.*` controls a mailbox. These are c9p
methods and CLI operations. M2 supplies data and API only; it adds no GUI.

`C9_MAIL_SOCKET=on` opts in to Claude Code peer delivery. The default is off
until the live spike, live acceptance and owner approval. Human messages
always use the input box. Agent and engine system messages use the peer
socket when a matching, current CC pid.json advertises peerProtocol 1.
A complete matching pid.json without that protocol can use paste. Missing,
partial or mismatched identity, unsafe paths, wrong peers and transient I/O
failures wait with backoff; they never upgrade peer mail to user authority.

```text
queued -> delivering -> delivered
                    -> failed | rejected | expired | uncertain
queued -> cancelled
uncertain -> delivered (late_evidence only, within ten minutes)
```

`delivered` means the model context received the message, established by
`origin.msg_id` in a peer JSONL entry or a signed fence in a prompt hook.
It does not mean the model obeyed. Socket write success and CC's
`delivered` receipt mean enqueue/acceptance only. No evidence after 30
seconds means `uncertain`. An enqueued message waits while the target
works; after its turn ends there is a 15-second evidence grace period.
`held` is a detail of `delivering`, blocks subsequent mail and waits up to
15 minutes. The uncertain state never automatically retries. Evidence may
arrive late and cancel a still-queued system notice.

Only one message per target may be in flight. Backoff and pause are stored
in SQL. Rate-limit/queue-full receipts wait 10/30/60 seconds; duplicate
receipts are uncertain and are never resent. Pre-write failures wait
2/10/30/60 seconds, failing after five attempts. Agent termination or
engine restart leaves socket mail uncertain. Drain blocks new injection;
in-flight socket mail counts as a turn and is interrupted if cut off.

Agents reply explicitly with `c9watch send <addr> --reply-to <msg> "text"`.
The hop limit is eight. SendMessage replies into the engine socket are
audited with `cc.inbound reason=use_cli` and the sender receives at most
one corrective system mail per ten minutes. Such replies are not imported
into the ledger. Foreign/detected CC sessions cannot submit receipts.

System mail reports uncertain, failed, rejected, expired or cancelled
agent mail. It never replies to itself or to human mail, merges pending
notices and delays uncertain notices for 60 seconds. It tells the sender
that mail may have arrived, to ask before resending, and not to reply.

Read methods never set the reserved `read` state (which means model read).
Agent principals see only mail they sent or received. Human local callers
may read all mail and alone may cancel queued mail or pause/resume a
mailbox. Pause affects agent and system mail, while human mail continues.
Controls are audited; read methods are not.

`msg.state` retains its existing `from` principal string and `to` agent ID
for old clients. Optional `sender`/`recipient` carry rich addresses on
queued events, alongside `threadId`, `replyTo`, `kind`, `preview` and `via`.
`msg.list`/`msg.read` use the rich `from`/`to` summary shape. This additive
event form keeps existing `send --wait` clients compatible.

The internal CC wire format remains a live-spike dependency: prompt-hook
shape, return receipts, auto-mode hold behavior, lookalike escaping,
subagent transcript placement and restart/re-enqueue behavior need live
confirmation. Synthetic fixtures are labeled; they are not live evidence.
