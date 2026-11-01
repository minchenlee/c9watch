> **Preview.** Topic shapes may change in a preview minor. Read the public schema and discovery before subscribing.

# Event topics

Every event has `v`, `type:"event"`, `sub`, `cursor`, `topic`, `ts` (Unix milliseconds), `data`, and optional `subject` and `origin`. `subject` is an engine identity or a machine-qualified session key. Keep the value core supplies; do not derive a provider path from it.

This table covers every public topic in c9p 1.6. Type names refer to `$defs` in [the schema](c9p-1.json). Use `api schema --public` to omit private fields from those types.

| Topic | Subject | Data type and meaning | Planned gateway scope |
| --- | --- | --- | --- |
| `session.view` | machine/session key | `SessionView`: newest session state | `session:read` |
| `session.gone` | machine/session key | `SessionGoneEvent`: removed session id and key | `session:read` |
| `diag.detection` | machine | `DetectionDiagnostics`: source diagnostics | `session:read` |
| `detect.health` (1.11) | machine | `DetectorHealth`: detection is ok, degraded, failed or stale; see [c9p-1.md](c9p-1.md#detector-health-111) | `session:read` |
| `notification.needs_attention` | machine/agent id | `NotificationEvent`: attention transition | `session:read` |
| `notification.waiting_for_input` | machine/agent id | `NotificationEvent`: input transition | `session:read` |
| `agent.upsert` | machine/agent id | `AgentRow`: newest engine agent state | `agent:read` |
| `agent.attention` | machine/agent id | `AgentAttentionEvent`: agent needs attention | `agent:read` |
| `agent.turn_end` | machine/agent id | `AgentTurnEndEvent`: turn ended; not proof the task is complete | `agent:read` |
| `agent.gone` | machine/agent id | `AgentGoneEvent`: agent is gone | `agent:read` |
| `agent.failed` | machine/agent id | `AgentFailedEvent`: agent failure | `agent:read` |
| `msg.state` | machine/recipient agent id | `MsgStateEvent`: mail delivery state | Not available to gateway clients in preview |

Socket topic discovery in 1.6 lists all topics. M-b derives the allowed set for each principal and gateway token from `x-authz`. Agent principals will receive the permitted session and agent topics. Conversation and mail access remain restricted. Gateway fields are also redacted by ceiling. The table's scopes describe that M-b contract; they are not present as grants in today's socket API.

`legacy.inbox` is private migration data. It is absent from public schema, SDK types and event exports. Do not subscribe to it in a public client.

`session.view` replay is compacted to the latest view of each live session. Intermediate views and ended sessions' old views may be absent. It is enough to rebuild current state with `session.gone`; it is not a full historical status timeline.

For OS notifications, subscribe to `notification.*` without `since`. Drop events older than the time you subscribed. Replaying old notifications on app start can create a stale burst.

Future annotation topics are described in [extending.md](extending.md). They are not part of 1.6.

## Annotation topics (1.9)

Both topics use `annotation:read` for clients and are visible to local and agent principals. `annotation.changed` carries `{target,source,values}`: replace that source's complete map, including on expiry or explicit clear. Sequence-suppressed patches emit nothing. `annotation.removed` carries `{target,key,reason:"target_gone"}` after the 30-day disappearance grace. Subject is the target session key. String values are omitted below the conversation ceiling and marked `redacted:true`; numeric values require metadata. See [L1 annotations](extending.md#l1-annotations-preview-c9p-19).
