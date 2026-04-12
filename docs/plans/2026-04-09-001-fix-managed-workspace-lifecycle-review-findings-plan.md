---
title: fix: Address managed workspace lifecycle review findings
type: fix
status: active
date: 2026-04-09
origin: docs/plans/2026-04-06-001-feat-managed-workspace-lifecycle-config-plan.md
---

# fix: Address managed workspace lifecycle review findings

## Overview

Fix the managed workspace lifecycle regressions found during review of the recent lifecycle-config feature. The follow-up should preserve the shipped v1 scope while correcting the create/delete behaviors that currently use the wrong base workspace context, can strand deleted workspaces in the store, or mutate repo config before create succeeds.

## Problem Frame

The first implementation landed the main lifecycle feature, but review surfaced a second pass of behavioral bugs rather than missing product scope. The create flow still derives some decisions from the project root instead of the active base workspace, persisted lifecycle defaults are written too early, and the delete flow can leave a broken workspace entry behind or fail closed on malformed lifecycle config. These are correctness and recovery issues inside the current feature, not reasons to widen the feature into remote workspaces, a generalized job system, or an agent-native redesign.

## Requirements Trace

- R1. Managed local workspace creation must use the active base workspace path, not only the project root, for dirty-worktree checks, base-branch fallback, and move-changes handoff.
- R2. One-off create settings that also request persistence must not rewrite `.superzent/config.json` until create has passed validation and the new worktree exists.
- R3. Managed local workspace deletion must stay recoverable when lifecycle config loading fails; malformed or deprecated config must not block the user from force-deleting.
- R4. After a managed workspace is deleted on disk, the app must remove the matching store entry even if best-effort tab/window cleanup hits an error.
- R5. The follow-up must add regression coverage for the corrected create/delete behaviors in `crates/superzent_git/src/lib.rs` and any targeted UI tests that are feasible in the touched area.

## Scope Boundaries

- Do not add lifecycle automation for SSH or other remote workspaces.
- Do not redesign the whole lifecycle system around background jobs, progress views, or a separate log subsystem.
- Do not use this pass to address subjective maintainability suggestions unless the code changes naturally simplify while fixing the concrete bugs.
- Do not broaden the work into agent-native parity beyond noting residual gaps in review.

## Context & Research

### Relevant Code

- `crates/superzent_git/src/lib.rs`
  - `create_workspace_internal` currently uses `project.local_repo_root()` for dirty checks and base-branch fallback even when the UI already resolved a more specific base workspace path.
  - `prepare_superzent_config_for_create` currently writes `.superzent/config.json` before branch validation and `git worktree add`.
  - `delete_workspace` currently hard-fails if `load_superzent_config` fails in the non-force path.
- `crates/superzent_ui/src/lib.rs`
  - `spawn_new_workspace_request` already resolves `base_workspace_path`, but the downstream git layer does not consistently honor it.
  - `run_delete_workspace_entry` deletes on disk before store cleanup and currently treats any later UI cleanup failure as a full delete failure.

### Institutional Learnings

- `docs/solutions/integration-issues/managed-terminal-popup-notifications-2026-04-04.md`
  - Keep upstream lifecycle stages observable and recoverable rather than only surfacing the final UI symptom.
- `docs/solutions/integration-issues/default-build-next-edit-surface-restoration-2026-04-05.md`
  - Keep repo defaults, runtime state, and per-invocation overrides as separate concerns rather than blending them into one helper or cached state source.

## Key Decisions

- Treat the active base workspace path as the authoritative local context for create-time dirty checks, base-branch fallback, and move-changes behavior.
  - Rationale: the recent feature explicitly introduced “base workspace current branch” semantics, so using the project root when a different worktree is active violates the intended contract.
- Keep delete recoverability by converting lifecycle-config load failures into a force-delete-capable failure path instead of silently skipping teardown.
  - Rationale: deletion should remain conservative by default, but malformed config should not trap the user with no recovery option.
- Make config persistence transactional with create.
  - Rationale: failed creates should not leave repo-level lifecycle defaults partially updated.
- Prefer “delete succeeded on disk, cleanup degraded in UI” semantics over leaving a stale workspace entry in the store.
  - Rationale: once the worktree is gone, the store must converge to disk reality even if some window cleanup needs best-effort recovery.

## Implementation Units

- [ ] **Unit 1: Honor the active base workspace across create-time git decisions**

**Goal:** Make managed local create behavior use the resolved base workspace path instead of the project root wherever the feature contract depends on the user’s current workspace context.

**Requirements:** R1

**Files:**

- Modify: `crates/superzent_git/src/lib.rs`
- Modify: `crates/superzent_ui/src/lib.rs`
- Test: `crates/superzent_git/src/lib.rs`

**Approach:**

- Thread the effective source workspace path through the create flow for dirty checks and base-branch fallback logic.
- Update any move-changes handoff to stash from the active base workspace path rather than the project’s canonical repo root.
- Preserve linked-worktree repo-root discovery for worktree placement while separating it from “current workspace” behavior.

**Execution note:** Start with regression tests for “active secondary worktree branch wins over primary repo branch” and “move changes uses active workspace source” before changing the helpers.

**Patterns to follow:**

- `spawn_new_workspace_request` in `crates/superzent_ui/src/lib.rs`
- `create_workspace_from_linked_worktree_uses_primary_repo_root` in `crates/superzent_git/src/lib.rs`

**Test scenarios:**

- Happy path: creating from a secondary worktree uses that worktree’s current branch as the fallback base branch.
- Happy path: create-and-move-changes moves staged, unstaged, and untracked changes from the active source workspace rather than the project root.
- Edge case: linked-worktree creation still places new worktrees under the primary repo’s `.superzent-worktrees` directory.

**Verification:**

- Base-branch fallback matches the active base workspace, not the repo root.
- Move-changes behavior follows the workspace the user launched create from.

- [ ] **Unit 2: Make persisted lifecycle defaults transactional with create**

**Goal:** Prevent `.superzent/config.json` from being rewritten until create has passed validation and the new worktree exists.

**Requirements:** R2, R5

**Files:**

- Modify: `crates/superzent_git/src/lib.rs`
- Test: `crates/superzent_git/src/lib.rs`

**Approach:**

- Split config preparation into an in-memory phase and a persistence phase.
- Run branch validation and `git worktree add` before writing repo defaults.
- If persistence fails after the worktree is created, clean up the just-created worktree before returning an error so the operation remains all-or-nothing.

**Execution note:** Keep this test-first; the main risk is partial mutation on failure.

**Patterns to follow:**

- Existing create/delete cleanup helpers in `crates/superzent_git/src/lib.rs`

**Test scenarios:**

- Happy path: persisted setup/teardown defaults are written on successful create.
- Error path: invalid branch input or failed base-branch resolution does not mutate `.superzent/config.json`.
- Error path: persistence failure after worktree creation removes the newly created worktree and returns an error.

**Verification:**

- Failed create attempts leave repo lifecycle config unchanged.
- Successful persisted creates still write the expected config contents.

- [ ] **Unit 3: Keep local delete recoverable and converge store state after on-disk deletion**

**Goal:** Preserve force-delete recovery for lifecycle-config failures and ensure successful on-disk deletion cannot leave a ghost workspace entry in the store.

**Requirements:** R3, R4, R5

**Files:**

- Modify: `crates/superzent_git/src/lib.rs`
- Modify: `crates/superzent_ui/src/lib.rs`
- Test: `crates/superzent_git/src/lib.rs`
- Test: `crates/superzent_ui/src/lib.rs`

**Approach:**

- Convert lifecycle-config load failures in the non-force delete path into a structured failure that the UI can present through the existing force-delete prompt.
- Restructure the UI delete flow so store removal is keyed to whether deletion on disk succeeded, not to whether every later tab/window cleanup step succeeded.
- Keep cleanup errors visible to the user as degraded follow-up behavior rather than pretending the delete fully failed.

**Execution note:** Characterize the current delete ordering first, then change the control flow with focused tests.

**Patterns to follow:**

- `delete_workspace_blocks_on_teardown_failure_until_force_delete` in `crates/superzent_git/src/lib.rs`
- `run_delete_workspace_entry` in `crates/superzent_ui/src/lib.rs`

**Test scenarios:**

- Error path: malformed `.superzent/config.json` still results in a force-delete-capable failure path instead of an unrecoverable error.
- Happy path: successful delete removes the store entry even when window cleanup reports an error after deletion.
- Edge case: teardown failure still keeps the worktree until the user explicitly retries with force delete.

**Verification:**

- Users can recover from malformed lifecycle config by force deleting.
- Deleted workspaces no longer remain in the store after the backing path is gone.

## Dependencies & Sequencing

1. Unit 1 first, because it corrects the base workspace semantics the other fixes build on.
2. Unit 2 next, because create-time transactionality is isolated to the git layer and should be stabilized before the final review pass.
3. Unit 3 last, because it depends on understanding the corrected lifecycle failure surfaces from the earlier units.

## Verification Strategy

- Run targeted crate tests for `superzent_git`.
- Run any focused `superzent_ui` tests covering delete-flow helpers if available.
- Re-run review after implementation with this plan attached so requirement completeness is checked against the follow-up scope.
