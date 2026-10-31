> **Preview.** A breaking preview entry must say "breaking (preview)" and give each affected method's new `rev`. Check `hello.breakingSince`.

# c9p/1 changelog

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
