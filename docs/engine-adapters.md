# Engine P1 adapter inventory

The engine recognizes seven adapter kinds. Recognition exposes metadata and
runtime argv helpers; it does not enable a spawn profile. `agent.create` and boot
resume still run only Claude Code. Other recognized kinds fail before a process
is launched. Passive session detection is unchanged.

| Kind | Existing provider key | Executables | Hook trust | Resume |
| --- | --- | --- | --- | --- |
| `claude` | `claudeCode` | `claude` | `turn_end` | `claude --resume <id>` |
| `codex` | `codex` | `codex` | `turn_end` | `codex resume <id>` |
| `pi` | `pi` | `pi` | `authoritative` | `pi --session <path>` |
| `omp` | `pi` | `omp` | `authoritative` | `omp --resume <id-or-path>` |
| `opencode` | `opencode` | `opencode`, `opencode2` | `authoritative` | `opencode --session <id>` |
| `kilo` | `opencode` | `kilo` | `authoritative` | `kilo --session <id>` |
| `cursor` | `cursor` | `cursor-agent`, `agent` | `identity_only` | `cursor-agent --resume <id>` |

All are full-tier descriptors with bracketed-paste prompt delivery and
`merge_only` path guards. These declarations are not live acceptance evidence.
OMP also accepts Pi hook kinds; Kilo also accepts OpenCode hook kinds, following
the design's shared extension/plugin plan.

Provider keys select session readers. They cannot select between Pi and OMP or
between OpenCode and Kilo. `descriptor_for_provider` therefore returns no result
for these ambiguous keys. A future spawn implementation must persist the actual
adapter kind separately and retain Pi's transcript-path session reference.

## Permission flags

The new flags below were read from locally installed CLI help on 2026-11-01.
An empty posture has no verified override; it must not be treated as a supported
permission mode when adding a spawn profile.

| Kind | Manual | Auto | Yolo |
| --- | --- | --- | --- |
| Codex | `--ask-for-approval on-request --sandbox workspace-write` | `--approve-for-me` | `--dangerously-bypass-approvals-and-sandbox` |
| Pi | empty | empty | empty |
| OMP | `--approval-mode always-ask` | empty | `--approval-mode yolo` |
| OpenCode | empty | `--auto` | empty |
| Kilo | empty | empty | empty |
| Cursor | empty | `--auto-review` | `--yolo` |

Pi's `--approve` concerns project trust, not tool permissions. OMP lists a
`write` approval mode without defining it as the auto posture. Kilo was not
installed locally. No flags were inferred for those missing postures.

## Models

`Descriptor::model_argv` accepts an optional model name and a trusted catalogue
of kind-specific model-to-argv mappings. An absent model emits no flags. A model
not found for that kind returns an error whose code is `unknown_model`.

No production model catalogue is supplied yet. The existing free-create API has
no brief/model field, and managed creation is outside this change. Catalogue
discovery, CLI version selection, and mapping this error to a future managed
create response remain integration work. The installed OpenCode v2 root help
does not expose the same model flags as OpenCode v1, so the helper does not
invent an unversioned OpenCode mapping.

## Read-only integration status

`c9watch integration status` lists all seven descriptors. `integration show
<kind>` keeps Claude's existing settings-block output and returns an inventory
entry for the other known kinds. Unknown kinds still fail.

Claude is `current` when its engine-owned agent helper is executable, otherwise
`missing`, as before. This checkout has no engine Codex hook installer, Pi/OMP
extension installer, OpenCode/Kilo plugin installer, or Cursor hook installer.
Those six kinds are reported as `missing`, with
`reason: "engine_integration_not_implemented"`, `delivery: "unavailable"`, and
an empty `integrationFiles` list, even when Claude's helper exists.

The status enum retains `outdated` and `foreign_conflict`, but those states
cannot be determined for unshipped integrations without their versioned content
and ownership markers. The command does not invent paths or inspect arbitrary
vendor files, start the engine, or write user configuration. Completing those
installers and status comparisons is required before enabling Codex free spawn.
