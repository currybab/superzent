---
title: refactor: Compact managed workspace lifecycle internals
type: refactor
status: completed
date: 2026-04-10
origin: docs/brainstorms/2026-04-10-managed-workspace-lifecycle-compaction-requirements.md
---

# refactor: Compact managed workspace lifecycle internals

## Overview

Keep the current managed workspace lifecycle feature set, but reduce the internal coupling between repo config, workspace persistence, modal bootstrap, and delete recovery. The target state is one default lifecycle source of truth (`.superzent/config.json`) plus one narrow persisted exception (workspace-local teardown override), with create and delete flows shaped around that contract instead of letting helper state leak across layers.

## Problem Frame

The current implementation works, but it got there by threading lifecycle state through several unrelated surfaces: repo config, modal inputs, workspace persistence, sync builders, and delete cleanup helpers. The user-facing behavior is mostly right, but the code now pays a carrying cost each time lifecycle behavior changes. This refactor keeps the current feature contract while shrinking the number of places that need to know lifecycle details.

## Requirements Trace

- R1-R3. Keep repo-root `.superzent/config.json` as the default source of truth while preserving a persisted workspace-local teardown override.
- R4-R6. Keep `setup` create-time-only, preserve both modal inputs, and avoid fragile re-entrant modal initialization paths.
- R7-R9a. Always show the final teardown script or lack thereof in delete confirmation, use a scrollable code block, and make unreadable config a blocked-but-recoverable delete state.
- R10-R12. Reduce lifecycle duplication and make delete ownership easier to reason about without trimming current feature scope.

## Scope Boundaries

- Do not remove `.superzent/config.json` support or the existing create modal inputs.
- Do not add an edit UI for persisted workspace-local teardown overrides.
- Do not change remote workspace behavior.
- Do not use this refactor to add timeout/cancellation behavior for lifecycle commands; that remains separate follow-up work.
- Do not broaden this into agent-native parity or a generalized logging subsystem.

## Context & Research

### Relevant Code

- `crates/superzent_git/src/lib.rs`
  - Owns repo-config loading, create-time setup, delete-time teardown, and the workspace-local teardown persistence hook.
  - Already exposes `resolve_workspace_base_branch_from_workspace`, `WorkspaceLifecycleFailure`, and the create/delete primitives the UI can build on.
- `crates/superzent_model/src/lib.rs`
  - `WorkspaceEntry` currently stores `lifecycle_teardown_script`, which is semantically broader than the actual user contract.
- `crates/superzent_ui/src/lib.rs`
  - `NewWorkspaceModal` owns setup/teardown input and the single “save these script commands” checkbox.
  - `open_new_workspace_modal` now precomputes `initial_base_workspace_path`, which is the pattern to extend for compact create bootstrap.
  - `run_delete_workspace_entry` currently combines delete prompting, in-flight registration, forced recovery, window cleanup, and store cleanup.

### Institutional Learnings

- `docs/solutions/integration-issues/managed-terminal-popup-notifications-2026-04-04.md`
  - Early-stage pipeline failures should be surfaced from their true source rather than only from the final UI symptom. This supports resolving delete behavior up front before prompting.

## Key Technical Decisions

- Keep repo config as the baseline lifecycle contract and narrow workspace persistence to an explicit teardown override field only.
  - Rationale: the user agreed that persisted teardown overrides are the one exception that should survive restarts; everything else should derive from repo defaults or create-time input.
- Treat `setup` as one-shot even when the modal still offers both text areas.
  - Rationale: compactness comes from removing hidden post-create behavior, not from removing current input affordances.
- Reinterpret the current save checkbox as teardown-default persistence only.
  - Rationale: this preserves a compact single-control UI while aligning with the agreed contract that `setup` is never persisted.
- Resolve delete behavior before opening the destructive confirmation.
  - Rationale: the delete prompt must always be able to show the exact final teardown script or clearly explain that no script will run.
- Use a dedicated delete-resolution helper or small session helper rather than scattering state across global flags, result enums, and post-hoc cleanup checks.
  - Rationale: the current pain is not lack of behavior but ownership split.

## Implementation Units

- [x] **Unit 1: Narrow lifecycle persistence to a teardown override contract**

**Goal:** Replace the generic persisted lifecycle script field with a narrowly named teardown-override concept and stop copying repo defaults into workspace metadata.

**Requirements:** R1, R2, R3, R11

**Files:**

- Modify: `crates/superzent_model/src/lib.rs`
- Modify: `crates/superzent_git/src/lib.rs`
- Modify: `crates/superzent_ui/src/lib.rs`
- Test: `crates/superzent_git/src/lib.rs`
- Test: `crates/superzent_ui/src/lib.rs`

**Approach:**

- Rename `lifecycle_teardown_script` to a field that clearly means persisted workspace-local teardown override.
- Persist that field only when the user supplied a non-saved teardown value at create time.
- Make delete resolution follow `workspace override -> repo config -> no teardown`.
- Keep legacy-state conversion and workspace bundle sync, but only for the narrow override field rather than a generic lifecycle script concept.

**Execution note:** Start with characterization tests for “persisted teardown override survives restart/sync paths” before renaming the field through the model and UI.

**Patterns to follow:**

- Existing `WorkspaceEntry` persistence in `crates/superzent_model/src/lib.rs`
- Current create/delete lifecycle tests in `crates/superzent_git/src/lib.rs`

**Test scenarios:**

- Happy path: a non-saved teardown value is stored as a workspace-local override and used after restart.
- Happy path: when no override exists, delete falls back to repo config teardown.
- Edge case: repo defaults are not mirrored into workspace persistence when the user did not create a workspace-specific override.

**Verification:**

- Workspace metadata contains only the narrow persisted override contract.
- Repo defaults no longer need to be propagated through unrelated sync code.

- [x] **Unit 2: Compact the create modal bootstrap and make save-toggle semantics explicit**

**Goal:** Keep the current create modal inputs while moving fragile initialization and ambiguous persistence semantics out of the constructor path.

**Requirements:** R4, R5, R5a, R6, R10

**Files:**

- Modify: `crates/superzent_ui/src/lib.rs`
- Modify: `crates/superzent_git/src/lib.rs`
- Test: `crates/superzent_ui/src/lib.rs`

**Approach:**

- Precompute all create-time values needed to bootstrap the modal rather than letting the constructor reach back into live workspace state.
- Introduce a small helper or draft type that makes the save-toggle meaning explicit: it persists repo-default teardown only and never persists `setup`.
- Update the checkbox copy to match its real behavior.
- Keep base-branch preview and both script inputs, but make constructor inputs purely data, not live entity lookups.

**Execution note:** Implement helper-level tests first for the checkbox semantics and modal bootstrap inputs so the UI refactor stays small and verifiable.

**Patterns to follow:**

- `open_new_workspace_modal` precomputation in `crates/superzent_ui/src/lib.rs`
- `resolve_workspace_base_branch_from_workspace` usage in the modal bootstrap path

**Test scenarios:**

- Happy path: modal still shows both setup and teardown inputs and preloads repo defaults correctly.
- Happy path: selecting the save toggle persists repo-default teardown but does not persist `setup`.
- Edge case: opening the modal from a local workspace does not re-enter workspace state during modal construction.

**Verification:**

- The modal no longer depends on fragile entity re-entry.
- A reader can tell from the UI copy and code path that `setup` is one-shot and save applies to repo-default teardown only.

- [x] **Unit 3: Reshape delete resolution around the final teardown preview**

**Goal:** Make delete prompting and recovery flow derive from one explicit delete-resolution step that always knows the final teardown script or blocked state.

**Requirements:** R7, R8, R9, R9a, R12

**Files:**

- Modify: `crates/superzent_git/src/lib.rs`
- Modify: `crates/superzent_ui/src/lib.rs`
- Test: `crates/superzent_git/src/lib.rs`
- Test: `crates/superzent_ui/src/lib.rs`

**Approach:**

- Introduce a delete-resolution helper that returns one of:
  - run this exact teardown script
  - no teardown script will run
  - normal delete is blocked because config could not be read
- Use that helper before the main delete confirmation so the confirmation can always render the final teardown preview in a scrollable code block.
- When config is unreadable, render a blocked recovery state whose `Delete Anyway` path explicitly skips teardown and proceeds to force delete.
- Reuse the existing `WorkspaceLifecycleFailure` surface where useful, but stop making the prompt body infer behavior from later failure handling.

**Execution note:** Characterize the current force-delete flow before restructuring prompts so the final recovery path stays behaviorally equivalent.

**Patterns to follow:**

- Existing teardown failure recovery in `run_delete_workspace_entry`
- Existing `workspace_lifecycle_failure_prompt_detail` helper

**Test scenarios:**

- Happy path: delete confirmation shows the final repo-config teardown script in a scrollable block before delete.
- Happy path: delete confirmation shows the workspace-local override script when one exists.
- Edge case: when no teardown exists, the confirmation explicitly says so instead of rendering an empty script block.
- Error path: unreadable config blocks normal delete and offers `Delete Anyway`, which skips teardown.

**Verification:**

- Delete confirmation always explains the final teardown behavior up front.
- The unreadable-config path is explicit rather than implicit.

- [x] **Unit 4: Collapse delete-flow ownership after on-disk deletion**

**Goal:** Keep the explicit delete recovery behavior while reducing ownership split across global delete state, result plumbing, and post-delete cleanup.

**Requirements:** R10, R12

**Dependencies:** Unit 3

**Files:**

- Modify: `crates/superzent_ui/src/lib.rs`
- Test: `crates/superzent_ui/src/lib.rs`

**Approach:**

- Consolidate the delete session lifecycle into one helper object or one tightly scoped orchestration helper that owns registration, prompt branching, cleanup, and store removal.
- Keep the in-flight open guard if still necessary, but make its setup/teardown happen in one place.
- Preserve the current rule that store state must converge to disk reality once deletion succeeded on disk, even if later UI cleanup degrades.

**Execution note:** This is the only unit that should touch the delete-session orchestration shape; avoid mixing it with repo-config semantics once Unit 3 has stabilized the prompt contract.

**Patterns to follow:**

- Current `InFlightWorkspaceDeletes` protection in `crates/superzent_ui/src/lib.rs`
- Existing best-effort cleanup behavior after delete succeeds on disk

**Test scenarios:**

- Happy path: successful delete removes the store entry and closes windows through the consolidated helper path.
- Edge case: cleanup errors after on-disk deletion still remove the store entry and show a degraded cleanup message.
- Edge case: cancel paths and blocked paths still unregister any in-flight delete guard correctly.

**Verification:**

- Delete ownership is concentrated enough that new branches do not need to remember three separate cleanup responsibilities.

## Dependencies & Sequencing

1. Unit 1 first, because the persistence contract defines what the rest of the system is actually allowed to store.
2. Unit 2 next, because create semantics should be aligned with the new persistence contract before delete is reshaped.
3. Unit 3 then sets the explicit final-teardown preview and unreadable-config recovery contract.
4. Unit 4 last, because it is mostly orchestration cleanup once the delete contract is already stable.

## Risks & Mitigations

| Risk                                                                                  | Mitigation                                                                                                   |
| ------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------ |
| Narrowing persistence could accidentally drop restart behavior for teardown overrides | Start with characterization coverage for persisted override restoration before renaming or collapsing fields |
| Reinterpreting the save checkbox could confuse users if copy changes are too subtle   | Update the label explicitly and add helper-level tests for its semantics                                     |
| Delete confirmation could become too large with multiline scripts                     | Use a scrollable code block and explicit “no teardown” messaging instead of raw text concatenation           |
| Delete compaction could regress the in-flight open guard                              | Keep the guard behavior until equivalent protection exists in the consolidated helper                        |

## Verification Strategy

- Run targeted crate tests for `superzent_git`.
- Run focused `superzent_ui` tests for modal bootstrap helpers and delete helper behavior.
- Re-run `ce:review` against the refactor diff to check whether the source-of-truth and delete-flow complexity findings have been reduced.

## Sources & References

- Origin requirements: `docs/brainstorms/2026-04-10-managed-workspace-lifecycle-compaction-requirements.md`
- Related plan: `docs/plans/2026-04-09-001-fix-managed-workspace-lifecycle-review-findings-plan.md`
- Current feature baseline: `docs/plans/2026-04-06-001-feat-managed-workspace-lifecycle-config-plan.md`
- Institutional learning: `docs/solutions/integration-issues/managed-terminal-popup-notifications-2026-04-04.md`
