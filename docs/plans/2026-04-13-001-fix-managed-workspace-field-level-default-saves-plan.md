---
title: fix: Restore field-level managed workspace default saves
type: fix
status: active
date: 2026-04-13
origin: docs/brainstorms/2026-04-10-managed-workspace-lifecycle-compaction-requirements.md
---

# fix: Restore field-level managed workspace default saves

## Overview

Restore repo-default `setup` saving in the managed workspace create flow without undoing the lifecycle compaction work. The follow-up should keep repo-root `.superzent/config.json` as the only lifecycle default source, keep workspace-local persistence teardown-only, and replace the current shared save behavior with field-level save controls attached to the `setup` and `teardown` editors.

## Problem Frame

The 2026-04-10 lifecycle compaction pass simplified persistence by narrowing save behavior to teardown defaults and teardown overrides. That reduced ambiguity, but it also removed a useful authoring path: promoting a one-off `setup` script into the repo default from the create modal. The current code now hardcodes a teardown-only save contract across modal copy, request plumbing, helper tests, and config staging in `crates/superzent_ui/src/lib.rs` and `crates/superzent_git/src/lib.rs`.

This follow-up is a behavior fix inside the new compaction contract, not a rollback of it. Repo defaults should still live only in `.superzent/config.json`, `setup` should still remain one-shot per created workspace, and workspace-local persistence should still be teardown-only. The missing behavior is field-level repo-default authoring from the create flow, plus an unambiguous way to clear an existing repo default intentionally.

## Requirements Trace

- R1. Keep repo-root `.superzent/config.json` as the default source of truth for `base_branch`, `setup`, and `teardown`.
- R2-R4. Preserve the narrowed runtime contract: workspace-local persistence stays teardown-only, teardown overrides still survive restart, and `setup` stays create-time-only behavior.
- R5-R5f. Add independent repo-default save controls for `setup` and `teardown`, colocate each control with its field, make empty-plus-save clear that field's repo default, keep writes transactional with create success, and prefill later create modals from saved defaults.
- R6. Keep modal bootstrap data precomputed before view construction.
- R7-R9a. Preserve the delete contract from the compaction work; this follow-up must not reopen delete resolution or recovery behavior.
- R10-R12. Preserve the compaction outcome by restoring `setup` default authoring without reintroducing multi-source lifecycle state.

## Scope Boundaries

- Do not add workspace-local persistence for `setup`.
- Do not re-open delete-flow behavior, delete copy, or unreadable-config recovery beyond preserving the existing contract.
- Do not change SSH or other remote workspace create behavior.
- Do not add a new "disable repo default for this create only" mode; this follow-up only changes repo-default authoring and clearing when the matching save control is explicitly selected.
- Do not introduce a second lifecycle config file or any precedence layer beyond repo-root `.superzent/config.json`.

## Context & Research

### Relevant Code and Patterns

- `crates/superzent_ui/src/lib.rs`
  - `NewWorkspaceModalBootstrap` already precomputes base-branch and lifecycle default text before modal construction.
  - `NewWorkspaceModal` currently owns two editors but only one save flag, `save_teardown_script_as_repo_default`.
  - `new_workspace_create_options` and `spawn_new_workspace_request` duplicate the teardown-only save contract across the normal create path and the dirty-workspace retry path.
  - Existing unit tests near the bottom of the file already cover bootstrap defaults and create-option helper semantics.
- `crates/superzent_git/src/lib.rs`
  - `CreateWorkspaceOptions` currently models save intent as a single teardown-only boolean.
  - `prepare_superzent_config_for_create` stages only teardown changes before create.
  - `create_workspace_internal` already has the right transactional seam: it stages config in memory, creates the worktree, writes config, and cleans up the worktree if the write fails.
  - `workspace_teardown_script_override_for_create` already enforces the repo-default-vs-workspace-override split and should remain the only persisted exception.
  - `workspace_lifecycle_defaults` already reads repo defaults back out for modal prefill.
- `docs/plans/2026-04-10-001-refactor-managed-workspace-lifecycle-compaction-plan.md`
  - The completed compaction plan is the baseline to preserve, not something to overwrite.

### Institutional Learnings

- `docs/solutions/best-practices/managed-workspace-lifecycle-source-of-truth-and-teardown-override-contract-2026-04-10.md`
  - Repo config should remain the default lifecycle source of truth and workspace persistence should stay teardown-only.
  - Modal bootstrap should consume plain precomputed data rather than re-entering live workspace state.
  - Delete behavior should remain resolved once and executed against that same plan; this follow-up should not disturb that separation.

### External References

- None. The local codebase and recent lifecycle docs provide sufficient guidance for this follow-up.

## Key Technical Decisions

- Replace the shared lifecycle save contract with field-level save intent in the create request.
  - Rationale: the missing behavior is not another persistence layer; it is the ability to save `setup` and `teardown` independently without ambiguity.
- Keep repo-default writes staged in memory and persisted once, after worktree creation succeeds.
  - Rationale: the existing transactional seam in `create_workspace_internal` already matches R5e and should be extended rather than reworked.
- Continue resolving workspace-local teardown overrides against the staged repo default, not the pre-edit repo default.
  - Rationale: if the user saves a new teardown default during create, the same value must not also be persisted as a workspace-local override.
- Treat explicit empty-plus-save as "clear this repo default," but leave unsaved blank fields outside the scope of this follow-up.
  - Rationale: R5d is explicit about clearing repo defaults only when the save control is selected; adding a separate one-off disable mode would widen product behavior beyond the brainstorm.
- Keep modal bootstrap as pure data and make the dirty-workspace retry path reuse the same save intent.
  - Rationale: the biggest practical regression risk is losing one field's save selection or text when the create flow retries after the dirty-workspace prompt.

## Open Questions

### Resolved During Planning

- How should the source-of-truth contract stay compact? Keep repo defaults only in `.superzent/config.json` and keep workspace-local persistence teardown-only.
- How should create stay transactional? Stage setup/teardown repo-default changes in memory, write them after `git worktree add`, and reuse the existing cleanup-on-write-failure path.
- How should the UI avoid save ambiguity? Place one save control beside each script editor instead of sharing one lifecycle toggle.

### Deferred to Implementation

- Should the request contract be represented as two booleans or a small `RepoDefaultSaveSelections` helper struct once the touched call sites are in view?
- Should the dirty-workspace retry path be deduplicated by storing one reusable create-request value or by keeping the current helper call pattern and extending it?

## High-Level Technical Design

> _This illustrates the intended approach and is directional guidance for review, not implementation specification. The implementing agent should treat it as context, not code to reproduce._

| Field state at confirm time | Matching save control | Staged `.superzent/config.json` effect         | Current create behavior                      | Persisted workspace effect                                                  |
| --------------------------- | --------------------- | ---------------------------------------------- | -------------------------------------------- | --------------------------------------------------------------------------- |
| Non-empty `setup` text      | Off                   | Leave repo-default `setup` unchanged           | Use the one-off `setup` text for this create | None                                                                        |
| Non-empty `setup` text      | On                    | Replace repo-default `setup` with that text    | Use the same text for this create            | None                                                                        |
| Empty `setup` text          | On                    | Clear repo-default `setup`                     | Run no `setup` for this create               | None                                                                        |
| Non-empty `teardown` text   | Off                   | Leave repo-default `teardown` unchanged        | Keep current create result                   | Persist a teardown override only if it differs from the staged repo default |
| Non-empty `teardown` text   | On                    | Replace repo-default `teardown` with that text | Keep current create result                   | No teardown override for the same value                                     |
| Empty `teardown` text       | On                    | Clear repo-default `teardown`                  | Keep current create result                   | No teardown override                                                        |

## Implementation Units

- [ ] **Unit 1: Thread field-level save intent through the create request contract**

**Goal:** Replace the shared teardown-only save flag with independent `setup` and `teardown` save intent throughout the UI request path, including the dirty-workspace retry branch.

**Requirements:** R5, R5a, R5b, R5c, R5f, R6

**Dependencies:** None

**Files:**

- Modify: `crates/superzent_ui/src/lib.rs`
- Modify: `crates/superzent_git/src/lib.rs`
- Test: `crates/superzent_ui/src/lib.rs`
- Test: `crates/superzent_git/src/lib.rs`

**Approach:**

- Extend `NewWorkspaceModal` state and `new_workspace_create_options` so save intent is modeled per field instead of through `save_teardown_script_as_repo_default`.
- Update `spawn_new_workspace_request` to thread the new save contract through both the initial create attempt and the retry path after the dirty-workspace prompt.
- Keep modal bootstrap precomputed and data-only; do not reintroduce live workspace lookups during modal construction.

**Patterns to follow:**

- `build_new_workspace_modal_bootstrap` in `crates/superzent_ui/src/lib.rs`
- Existing helper tests around `new_workspace_create_options` in `crates/superzent_ui/src/lib.rs`
- `CreateWorkspaceOptions` usage in `crates/superzent_git/src/lib.rs`

**Test scenarios:**

- Happy path: helper-level create options preserve independent `setup` and `teardown` save selections.
- Happy path: opening the modal still preloads repo-default `setup` and `teardown` text without re-entering workspace state.
- Edge case: the dirty-workspace retry path preserves both script texts and both save selections on the second create attempt.
- Edge case: remote workspace creation still bypasses the local lifecycle save path.

**Verification:**

- The create request contract can express "save setup only," "save teardown only," "save both," and "save neither."
- No local create path silently drops one field's save intent during retry.

- [ ] **Unit 2: Stage setup and teardown repo-default writes independently in `superzent_git`**

**Goal:** Extend config staging so `setup` and `teardown` repo defaults can be saved or cleared independently while keeping config writes transactional with create success.

**Requirements:** R1, R4, R5a, R5c, R5d, R5e, R10, R11

**Dependencies:** Unit 1

**Files:**

- Modify: `crates/superzent_git/src/lib.rs`
- Test: `crates/superzent_git/src/lib.rs`

**Approach:**

- Expand `prepare_superzent_config_for_create` from a teardown-only helper into a field-aware staging step that updates only the repo-default fields whose save controls were selected.
- Preserve untouched repo-default fields exactly as they were loaded from `.superzent/config.json`.
- Interpret explicit empty-plus-save as an empty command list for that field so serialization removes the field from config.
- Reuse the existing "write after worktree add, then clean up on write failure" flow rather than adding another persistence step.

**Execution note:** Implement this characterization-first. The main risk is accidental mutation of untouched repo-default fields.

**Patterns to follow:**

- `prepare_superzent_config_for_create` and `write_superzent_config` in `crates/superzent_git/src/lib.rs`
- Existing transactional create tests such as `create_workspace_cleans_up_worktree_when_persisted_config_write_fails`

**Test scenarios:**

- Happy path: saving only `setup` updates `config.setup` and preserves the previous repo-default `teardown`.
- Happy path: saving only `teardown` updates `config.teardown` and preserves the previous repo-default `setup`.
- Happy path: saving both fields updates both in one config write.
- Edge case: saving an empty `setup` clears repo-default `setup` from `.superzent/config.json`.
- Edge case: saving an empty `teardown` clears repo-default `teardown` from `.superzent/config.json`.
- Error path: base-branch resolution or validation failure leaves `.superzent/config.json` unchanged.
- Error path: config write failure after worktree creation removes the new worktree and leaves `.superzent/config.json` unchanged.

**Verification:**

- Repo-default writes touch only the selected fields.
- Failed creates leave both the config file and the worktree graph unchanged.

- [ ] **Unit 3: Keep runtime execution, override persistence, and modal prefill aligned with staged defaults**

**Goal:** Ensure create-time script execution, teardown override persistence, and later modal prefill all consume the same staged repo-default contract after field-level saves are introduced.

**Requirements:** R2, R3, R4, R5c, R5d, R5e, R5f, R10, R11

**Dependencies:** Unit 2

**Files:**

- Modify: `crates/superzent_git/src/lib.rs`
- Modify: `crates/superzent_ui/src/lib.rs`
- Test: `crates/superzent_git/src/lib.rs`
- Test: `crates/superzent_ui/src/lib.rs`

**Approach:**

- Resolve setup execution from the same staged config/input contract used for repo-default writes so "save setup default" and "run setup for this create" stay in sync.
- Keep teardown override persistence grounded in the staged repo default so saving a new teardown default does not also persist a redundant workspace-local override.
- Update modal copy and layout to place the save controls beside the matching editors, including copy that makes "save as repo default" and "clear when empty and saved" behavior legible.
- Keep `workspace_lifecycle_defaults` as the only modal-prefill source so later create modals reflect whatever repo defaults were actually persisted.

**Patterns to follow:**

- `workspace_lifecycle_defaults` in `crates/superzent_git/src/lib.rs`
- `workspace_teardown_script_override_for_create` in `crates/superzent_git/src/lib.rs`
- Existing modal render structure and editor subscriptions in `crates/superzent_ui/src/lib.rs`

**Test scenarios:**

- Happy path: creating with a saved `setup` default writes the config and preloads the same `setup` text when the create modal is opened again later.
- Happy path: creating with a saved `teardown` default does not persist a workspace-local teardown override for the same value.
- Happy path: creating with an unsaved one-off teardown still persists a teardown override when it differs from the staged repo default.
- Edge case: saving an empty `setup` clears later modal prefill for `setup`.
- Edge case: saving an empty `teardown` clears later modal prefill for `teardown`.
- Integration: after the dirty-workspace prompt path, the final persisted config and later modal prefill still match the user's selected save controls.

**Verification:**

- Repo-default prefill, create-time execution, and teardown override persistence all reflect the same staged contract.
- The UI copy makes the field-level save behavior understandable without relying on old teardown-only assumptions.

## Dependencies & Sequencing

1. Unit 1 first, because the field-level save contract has to exist before the git layer can stage the right config.
2. Unit 2 next, because transactional config staging is the behavioral core of the fix.
3. Unit 3 last, because it validates that execution behavior, override persistence, and UI prefill all stay aligned with the new contract.

## System-Wide Impact

- **Interaction graph:** `build_new_workspace_modal_bootstrap` -> `NewWorkspaceModal::confirm` -> `spawn_new_workspace_request` -> `create_local_workspace` -> `superzent_git::create_workspace_without_setup` -> later `workspace_lifecycle_defaults` prefill on the next modal open.
- **Error propagation:** Config serialization or write failures should continue to surface as create failures; the new save controls must not swallow or downgrade those errors.
- **State lifecycle risks:** The highest-risk branch is the dirty-workspace retry flow because it reconstructs create options after an intermediate prompt. The second risk is comparing teardown overrides against the wrong repo-default snapshot.
- **API surface parity:** Remote workspace create remains unchanged. Delete resolution, delete preview, and workspace-local teardown precedence remain unchanged.
- **Integration coverage:** The strongest integration proof is a create-save-reopen cycle and a dirty-prompt-retry-create-save-reopen cycle, because those prove the full path from modal state to config write to future prefill.
- **Unchanged invariants:** Workspace-local persistence remains teardown-only, repo defaults still load only from repo-root `.superzent/config.json`, and delete behavior remains governed by the existing compaction contract.

## Risks & Dependencies

| Risk                                                                                                               | Mitigation                                                                                                                             |
| ------------------------------------------------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------- |
| Field-level save intent could be applied on the first create attempt but dropped on the dirty-workspace retry path | Cover both create attempts with helper-level tests before changing the UI plumbing                                                     |
| Config staging could accidentally overwrite the untouched repo-default field                                       | Add explicit single-field save tests before broadening `prepare_superzent_config_for_create`                                           |
| Teardown override logic could compare against the pre-edit repo default instead of the staged one                  | Add regression coverage for "save teardown default and create in one action" before changing the override helper                       |
| UI copy could imply a new one-off disable mode that the behavior does not implement                                | Keep copy scoped to "save as repo default" and "empty plus save clears the default" rather than promising per-create disable semantics |

## Documentation / Operational Notes

- No standalone docs update is required for this follow-up.
- The in-product copy inside `NewWorkspaceModal` should explain the field-level repo-default save behavior clearly enough that the old teardown-only assumption no longer appears in code or UI text.

## Sources & References

- **Origin document:** `docs/brainstorms/2026-04-10-managed-workspace-lifecycle-compaction-requirements.md`
- Related plan: `docs/plans/2026-04-10-001-refactor-managed-workspace-lifecycle-compaction-plan.md`
- Related plan: `docs/plans/2026-04-09-001-fix-managed-workspace-lifecycle-review-findings-plan.md`
- Institutional learning: `docs/solutions/best-practices/managed-workspace-lifecycle-source-of-truth-and-teardown-override-contract-2026-04-10.md`
- Related code: `crates/superzent_ui/src/lib.rs`
- Related code: `crates/superzent_git/src/lib.rs`
