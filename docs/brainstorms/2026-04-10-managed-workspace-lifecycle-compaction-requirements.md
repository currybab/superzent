---
date: 2026-04-10
topic: managed-workspace-lifecycle-compaction
---

# Managed Workspace Lifecycle Compaction

## Problem Frame

The recently added managed workspace lifecycle feature preserves important user-facing behavior, but the internal implementation has become larger and more coupled than it needs to be. The compaction follow-up should still reduce that carrying cost, but the narrowed save semantics introduced a user-facing gap: teams can no longer promote a one-off `setup` script into the repo default from the create flow even though `.superzent/config.json` still treats `setup` as a repo-level default concept. The goal of this revision is to keep the simpler persistence model while restoring explicit repo-default `setup` saving in a way that is easier to understand and less error-prone than a shared lifecycle save toggle.

## Requirements

**Source Of Truth**

- R1. Repo-level lifecycle defaults must come from repo-root `.superzent/config.json` and remain the default source of truth for `base_branch`, `setup`, and `teardown`.
- R2. A workspace-local override is allowed only for `teardown`, and only when the user provided a non-saved teardown value during workspace creation.
- R3. A workspace-local teardown override must persist across app restarts so later delete behavior matches what the user chose at create time.

**Create Contract**

- R4. `setup` remains a create-time-only action and must not become a persisted per-workspace behavior.
- R5. The create modal must continue to allow both `setup` and `teardown` input.
- R5a. The create modal must allow repo-default `setup` and repo-default `teardown` to be saved independently during workspace creation.
- R5b. Each save control must live with its corresponding script field rather than behind one combined lifecycle save toggle.
- R5c. Saving one script as a repo default must not implicitly save, clear, or otherwise change the other script's repo default.
- R5d. If the user clears a script field and explicitly saves that field as the repo default, the existing repo default for that field must be removed from `.superzent/config.json`.
- R5e. Repo-default `setup` or `teardown` changes requested during create must be persisted only after workspace creation succeeds; failed creates must not leave partial repo-default changes behind.
- R5f. Saved repo-default `setup` and `teardown` values must prefill their matching create-modal fields on later workspace creations.
- R6. The create flow should not depend on re-entering the current workspace state during modal construction or other similarly fragile UI initialization paths.

**Delete Contract**

- R7. The delete confirmation must always show the actual final teardown script that will run, or clearly indicate that no teardown script will run.
- R8. When the teardown script is long or multiline, the confirmation should present it in a scrollable code block rather than truncating or hiding it.
- R9. If repo config is malformed or unreadable, normal delete should be blocked, but `Delete Anyway` must remain available as an explicit recovery path.
- R9a. When unreadable config blocks normal delete, the recovery flow must explicitly state that `Delete Anyway` skips teardown rather than retrying an unknown script.

**Compaction Goals**

- R10. Internal simplification must preserve the current managed local workspace feature set, including repo-default `setup` authoring from the create flow, rather than removing behavior for the sake of smaller code.
- R11. Repo defaults should remain in repo-root `.superzent/config.json` and should not be duplicated into unrelated workspace persistence or sync paths unless that duplication is necessary to preserve the explicit workspace-local teardown override contract.
- R12. Delete flow state should become easier to reason about by reducing split ownership across global state, result plumbing, and post-delete cleanup decisions.

## Success Criteria

- A reader can identify one default source of truth for lifecycle behavior and one narrow exception for persisted teardown overrides.
- A user can save repo-default `setup` and repo-default `teardown` independently from the create modal without guessing what another field will do.
- A user can explicitly remove an existing repo-default `setup` or `teardown` from the create modal by saving an empty value for that field.
- Failed creates do not mutate repo lifecycle defaults.
- The implementation has fewer cross-layer data propagation paths and fewer hidden state transitions than the current version.
- The refined design gives planning a clear target for simplification without widening scope beyond the clarified field-level save behavior.

## Scope Boundaries

- Do not remove support for repo-root `.superzent/config.json`.
- Do not remove `setup` or `teardown` inputs from the create modal.
- Do not persist `setup` as workspace-local behavior or re-run it when an existing workspace is opened again.
- Do not add a post-create UI for editing workspace-local teardown overrides.
- Do not change remote workspace behavior in this pass.
- Do not broaden this effort into agent-native parity work or a generalized job/logging system.

## Key Decisions

- Repo config remains the default contract: it is the baseline lifecycle source of truth rather than something inferred from workspace metadata.
- `setup` is intentionally one-shot per workspace: it applies during creation only and should not linger as workspace-local behavior, but it should still be saveable as a repo default for future creations.
- `teardown` is the only allowed persisted workspace-local override because users may reasonably expect delete-time cleanup to honor what they chose at create time.
- Save intent must be field-specific and colocated with each script input; a shared lifecycle save toggle is simpler visually but too ambiguous operationally.
- Explicitly saving an empty script means removing that repo default rather than leaving stale config behind.
- Repo-default writes should remain transactional with successful workspace creation.
- Delete confirmation should always show the final script that will run, but it does not need to explain whether it came from repo config or a workspace-local override.
- If config is broken, delete should stay conservative by default while still offering an explicit `Delete Anyway` escape hatch.

## Alternatives Considered

- A single save toggle for both lifecycle fields was rejected. It is more compact, but it makes it too easy to accidentally persist a temporary `setup` edit while trying to save only `teardown`, and it obscures which field will actually be changed in `.superzent/config.json`.

## Dependencies / Assumptions

- The current managed workspace lifecycle feature and docs remain the behavior baseline for this refactor except where this document explicitly restores repo-default `setup` saving.
- Repo-root `.superzent/config.json` remains the only default lifecycle config source and already supports both `setup` and `teardown`.
- The simplification effort is allowed to change internal ownership boundaries as long as the user contract above stays intact.

## Outstanding Questions

### Deferred to Planning

- [Affects R11][Technical] What is the smallest persistence shape that keeps repo defaults only in `.superzent/config.json` while still preserving workspace-local teardown overrides where required?
- [Affects R5e][Technical] What is the simplest transactional create flow that can stage independent `setup` and `teardown` repo-default writes in memory and apply them only after create succeeds?
- [Affects R12][Technical] What is the simplest delete-flow structure that keeps explicit recovery behavior while reducing state split across UI helpers?
- [Affects R6][Technical] Which create-time values should always be precomputed before modal construction so field-level save state and default-prefill logic avoid re-entrant UI state access?

## Next Steps

-> /ce:plan for structured implementation planning
