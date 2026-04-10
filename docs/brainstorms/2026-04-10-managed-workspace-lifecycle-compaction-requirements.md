---
date: 2026-04-10
topic: managed-workspace-lifecycle-compaction
---

# Managed Workspace Lifecycle Compaction

## Problem Frame

The recently added managed workspace lifecycle feature preserves important user-facing behavior, but the internal implementation has become larger and more coupled than it needs to be. The goal of this follow-up is to keep the current user contract intact while making the code more compact, more accurate about sources of truth, and easier to evolve without touching multiple unrelated layers for one behavior change.

## Requirements

**Source Of Truth**

- R1. Repo-level lifecycle defaults must come from repo-root `.superzent/config.json` and remain the default source of truth for `base_branch`, `setup`, and `teardown`.
- R2. A workspace-local override is allowed only for `teardown`, and only when the user provided a non-saved teardown value during workspace creation.
- R3. A workspace-local teardown override must persist across app restarts so later delete behavior matches what the user chose at create time.

**Create Contract**

- R4. `setup` remains a create-time-only action and must not become a persisted per-workspace behavior.
- R5. The create modal must continue to allow both `setup` and `teardown` input.
- R5a. The current save-toggle semantics must be made explicit; if a single save control remains, it must apply only to repo-default teardown persistence rather than implicitly persisting `setup`.
- R6. The create flow should not depend on re-entering the current workspace state during modal construction or other similarly fragile UI initialization paths.

**Delete Contract**

- R7. The delete confirmation must always show the actual final teardown script that will run, or clearly indicate that no teardown script will run.
- R8. When the teardown script is long or multiline, the confirmation should present it in a scrollable code block rather than truncating or hiding it.
- R9. If repo config is malformed or unreadable, normal delete should be blocked, but `Delete Anyway` must remain available as an explicit recovery path.
- R9a. When unreadable config blocks normal delete, the recovery flow must explicitly state that `Delete Anyway` skips teardown rather than retrying an unknown script.

**Compaction Goals**

- R10. Internal simplification must preserve the current managed local workspace feature set rather than removing behavior for the sake of smaller code.
- R11. Repo defaults should not be duplicated into unrelated workspace persistence or sync paths unless that duplication is necessary to preserve the explicit workspace-local teardown override contract.
- R12. Delete flow state should become easier to reason about by reducing split ownership across global state, result plumbing, and post-delete cleanup decisions.

## Success Criteria

- A reader can identify one default source of truth for lifecycle behavior and one narrow exception for persisted teardown overrides.
- The create and delete user experience remains functionally equivalent for current managed local workspace behavior.
- The implementation has fewer cross-layer data propagation paths and fewer hidden state transitions than the current version.
- The refined design gives planning a clear target for simplification without inventing new user-facing behavior.

## Scope Boundaries

- Do not remove support for repo-root `.superzent/config.json`.
- Do not remove `setup` or `teardown` inputs from the create modal.
- Do not add a post-create UI for editing workspace-local teardown overrides.
- Do not change remote workspace behavior in this pass.
- Do not broaden this effort into agent-native parity work or a generalized job/logging system.

## Key Decisions

- Repo config remains the default contract: it is the baseline lifecycle source of truth rather than something inferred from workspace metadata.
- `setup` is intentionally one-shot: it applies during creation only and should not linger as workspace-local behavior.
- `teardown` is the only allowed persisted workspace-local override because users may reasonably expect delete-time cleanup to honor what they chose at create time.
- Delete confirmation should always show the final script that will run, but it does not need to explain whether it came from repo config or a workspace-local override.
- If config is broken, delete should stay conservative by default while still offering an explicit `Delete Anyway` escape hatch.
- If the create modal keeps one save checkbox, that control should persist repo-default teardown only; `setup` remains one-shot even when the checkbox is selected.

## Dependencies / Assumptions

- The current managed workspace lifecycle feature and docs remain the behavior baseline for this refactor.
- The simplification effort is allowed to change internal ownership boundaries as long as the user contract above stays intact.

## Outstanding Questions

### Deferred to Planning

- [Affects R11][Technical] What is the smallest persistence shape that preserves workspace-local teardown overrides without copying repo defaults through every workspace sync path?
- [Affects R12][Technical] What is the simplest delete-flow structure that keeps explicit recovery behavior while reducing state split across UI helpers?
- [Affects R6][Technical] Which create-time values should always be precomputed before modal construction to avoid re-entrant UI state access?

## Next Steps

-> /ce:plan for structured implementation planning
