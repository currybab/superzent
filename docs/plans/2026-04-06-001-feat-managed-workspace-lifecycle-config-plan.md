---
title: feat: Add managed workspace lifecycle config
type: feat
status: completed
date: 2026-04-06
origin: docs/brainstorms/2026-04-06-managed-workspace-lifecycle-config-requirements.md
---

# feat: Add managed workspace lifecycle config

## Overview

Add a repo-committed `.superzent/config.json` contract for managed local workspace lifecycle automation and default base-branch policy. The implementation should make `setup` and `teardown` first-class for managed local workspace create/delete flows, replace the hidden `copy` behavior with script-driven setup, and let the create flow surface and override the effective base branch without widening scope into remote workspaces or every generic worktree entry point (see origin: `docs/brainstorms/2026-04-06-managed-workspace-lifecycle-config-requirements.md`).

## Problem Frame

Today `superzent` already routes managed local workspace creation and deletion through `crates/superzent_git`, but the lifecycle contract is partial and internally inconsistent. The code still chooses a new worktree base from the current branch, still carries hidden `copy` behavior inspired by `superset`, and only exposes teardown hooks indirectly. The product intent is to turn this into an explicit `superzent` contract: repo-root config only, scripts only, create/delete semantics only, local managed workspaces only, and a predictable base-branch resolution order that users can see before creating a workspace.

## Requirements Trace

- R1-R5. Define the v1 config surface as repo-root `.superzent/config.json` with only `setup`, `teardown`, and `base_branch`, and no config layering.
- R6. Remove `copy` as a first-class lifecycle concept and make setup scripts the supported way to copy or prepare files.
- R7-R14. Run lifecycle commands only for managed local workspace create/delete, sequentially, with a stable environment contract; keep setup failures recoverable and teardown failures blocking unless force-deleted.
- R15-R19. Add a one-off base-branch override, default to configured `base_branch`, otherwise fall back to the base workspace current branch, and make the effective choice visible in the create flow.

## Scope Boundaries

- Do not add lifecycle automation for SSH or other remote workspaces in this phase.
- Do not add `.superzent/config.local.json`, home-directory overrides, or merge rules.
- Do not broaden the feature to the generic `git_ui` worktree picker or every worktree creation entry point in v1.
- Do not keep `copy` as a supported public config field.
- Do not turn this plan into a general-purpose task runner or background-job system; the scope is limited to synchronous create/delete lifecycle commands.

## Context & Research

### Relevant Code and Patterns

- `crates/superzent_git/src/lib.rs`
  - `create_workspace` and `delete_workspace` already own managed local workspace lifecycle for the product surface that matters here.
  - `CreateWorkspaceOptions` currently only carries `branch_name`.
  - `load_superzent_config`, `prepare_workspace_contents`, and `run_repo_hooks` show that `.superzent/config.json`, hidden `copy`, and teardown hooks already exist in partial form.
  - `run_shell_command` already executes shell commands in zsh with environment injection, but it currently exports `SUPERSET_*` variable names rather than a `SUPERZENT_*` contract.
  - The inline tests in this file are already the primary regression harness for managed workspace creation and deletion behavior.
- `crates/superzent_ui/src/lib.rs`
  - `spawn_new_workspace_request` is the current local/remote branching seam for managed workspace creation.
  - `NewWorkspaceModal` currently only collects a branch name, so it is the narrowest place to add base-branch preview and override for the managed local flow.
  - `run_delete_workspace_entry` currently has a simple confirm/delete prompt and no structured teardown-failure recovery path.
  - The current `warning` flow already shows post-create notices via toast after the workspace is opened, which is the natural seam for non-fatal setup failures.
- `crates/git/src/repository.rs`
  - `default_branch` already implements robust repository-default resolution (`upstream/HEAD`, `origin/HEAD`, `init.defaultBranch`, then `master`), which is better aligned with the new requirements than the current `superzent_git` "use the current branch" behavior.
- `docs/src/getting-started.md`, `README.md`, and `docs/src/SUMMARY.md`
  - These are the current user-facing docs surfaces for explaining core `superzent` workflows and are the right places to document repo lifecycle config.

### Institutional Learnings

- `docs/solutions/integration-issues/managed-terminal-popup-notifications-2026-04-04.md`
  - Cross-crate lifecycle failures in `superzent` often look like UI bugs but actually fail upstream. The plan should keep lifecycle outcomes structured and observable from the earliest failing stage instead of only surfacing a final generic toast.
- `docs/solutions/integration-issues/default-build-next-edit-surface-restoration-2026-04-05.md`
  - Product regressions came from coupling multiple concerns under one broad switch. This feature should keep config parsing, lifecycle execution, and UI presentation as separate contracts instead of blending them into one opaque workspace-creation path.

### External References

- None. Local patterns are strong enough, and the work is primarily about tightening `superzent`'s own product contract rather than adopting external framework behavior.

## Key Technical Decisions

- Scope the v1 implementation to the existing managed local workspace flow in `crates/superzent_git` and `crates/superzent_ui`.
  - Rationale: this is the product seam already used for managed local workspace creation/deletion, and widening into `git_ui` or SSH paths would multiply scope without helping the first shipped behavior.
- Resolve the effective base branch in `superzent_git` as `override -> configured base_branch -> base workspace current branch -> repository default branch`.
  - Rationale: the workspace shell should branch from what the user is actively basing work on by default, while still preserving explicit repo policy and a final repository-default fallback.
- Remove hidden copy preparation from the create flow and rely on scripts plus normal git-tracked worktree contents.
  - Rationale: `.superzent` files that are committed already appear in the worktree after `git worktree add`, so manual copy behavior is redundant, non-obvious, and conflicts with the goal of making scripts the only supported lifecycle mechanism.
- Model setup failure as successful worktree creation with attached lifecycle failure details, not as a top-level create error.
  - Rationale: users need the created workspace left intact so they can inspect or repair the environment in place.
- Model teardown failure as a structured failure that the UI can inspect and then bypass on an explicit force-delete retry.
  - Rationale: the current plain `anyhow` surface is not expressive enough to support logs plus a deliberate "Delete Anyway" path.
- Export `SUPERZENT_*` as the documented environment contract, while also exporting the legacy `SUPERSET_*` names during the transition.
  - Rationale: the current hidden hook path already uses `SUPERSET_*`; dual export avoids silently breaking existing internal adopters while making `SUPERZENT_*` the supported contract going forward.
- Keep setup-failure and teardown-failure logs in a small `superzent_ui`-owned lifecycle failure surface instead of inventing a global logs subsystem.
  - Rationale: both failure modes originate in one product feature, and the repo does not already show a better generic log-viewing abstraction for this path.

## Open Questions

### Resolved During Planning

- Which create flow should own the base-branch override and preview?
  - Resolution: `NewWorkspaceModal` in `crates/superzent_ui/src/lib.rs` for managed local workspaces only. Remote create remains unchanged in v1.
- Should the generic Git worktree picker adopt the same config and base-branch policy in this phase?
  - Resolution: no. Keep the first implementation scoped to managed local workspace flows and treat generic worktree parity as a follow-up.
- How should base-branch resolution behave when `base_branch` is absent or stale?
  - Resolution: use the base workspace current branch before falling back to repository-default semantics, then warn when a configured branch cannot be resolved.
- What should replace the hidden `copy` behavior?
  - Resolution: remove the manual copy preparation path and require repo setup steps to live in `setup` scripts or in files already tracked into the worktree.

### Deferred to Implementation

- Whether `superzent_git` should depend directly on the `git` crate for default-branch resolution or copy the current fallback logic into a local helper.
  - Why deferred: both choices are valid at plan time; the implementer can pick the smaller dependency and code-movement cost once editing begins.
- Whether the smallest log-viewing surface is a reusable lifecycle modal or a dedicated failure prompt followed by a read-only modal.
  - Why deferred: the plan fixes the product behavior and ownership boundary, but the precise widget shape is easier to finalize against the current `superzent_ui` helpers during implementation.
- Whether the force-delete retry should reuse the original confirmation path or branch into a dedicated teardown-failure prompt object.
  - Why deferred: the semantic contract is fixed, but the cleanest prompt composition depends on the final structured delete error shape.

## High-Level Technical Design

> _This illustrates the intended approach and is directional guidance for review, not implementation specification. The implementing agent should treat it as context, not code to reproduce._

| Stage                      | Owner                             | Planned contract                                                                                                                                        |
| -------------------------- | --------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Repo config parse          | `crates/superzent_git/src/lib.rs` | Read repo-root `.superzent/config.json`; support `setup`, `teardown`, `base_branch`; no public `copy` path                                              |
| Base branch resolution     | `crates/superzent_git/src/lib.rs` | Resolve `override -> configured base_branch -> base workspace current branch -> repo default`; return both effective branch and source/fallback warning |
| Managed create UI          | `crates/superzent_ui/src/lib.rs`  | Collect branch name, optional local-only base-branch override, and show the effective base branch before create                                         |
| Setup execution            | `crates/superzent_git/src/lib.rs` | Run sequential commands in the new worktree; capture stdout/stderr and keep the workspace on failure                                                    |
| Create result presentation | `crates/superzent_ui/src/lib.rs`  | Upsert and open the workspace even when setup fails, then surface a concise failure summary with log access                                             |
| Teardown execution         | `crates/superzent_git/src/lib.rs` | Run sequential commands before deletion; return structured failure details instead of flattening everything to one string                               |
| Delete result presentation | `crates/superzent_ui/src/lib.rs`  | On teardown failure, keep the workspace, show logs, and allow an explicit force-delete retry that skips teardown                                        |

Lifecycle outcome matrix:

| Operation | Command phase result                       | Expected product behavior                                                                                  |
| --------- | ------------------------------------------ | ---------------------------------------------------------------------------------------------------------- |
| Create    | No config or all setup commands succeed    | Workspace is created, opened, and behaves like today's managed local flow                                  |
| Create    | Setup command fails                        | Worktree stays on disk, workspace is still registered/opened, user sees failure summary and logs           |
| Delete    | No config or all teardown commands succeed | Worktree is removed and workspace entry is deleted                                                         |
| Delete    | Teardown command fails                     | Worktree and workspace stay intact, user sees failure summary and logs, force-delete is offered explicitly |

## Implementation Units

- [x] **Unit 1: Normalize the managed workspace config and base-branch contract**

**Goal:** Replace the hidden partial config behavior with the v1 public config contract and compute the effective base branch from the right sources.

**Requirements:** R1, R2, R4, R5, R6, R15, R16, R17, R18

**Dependencies:** None

**Files:**

- Modify: `crates/superzent_git/src/lib.rs`
- Modify: `crates/superzent_git/Cargo.toml`
- Test: `crates/superzent_git/src/lib.rs`

**Approach:**

- Replace the current config struct with a v1-focused contract containing `setup`, `teardown`, and `base_branch`.
- Remove `prepare_workspace_contents` and the hidden `copy` path from managed workspace creation, relying on the checked-out worktree contents plus scripts instead.
- Extend the create input to carry an optional one-off base-branch override.
- Add a helper that resolves the effective base branch and, when relevant, a fallback warning/source marker that the UI can display.
- Stop basing new managed worktrees on `current_branch()`; use repo-default semantics when there is no explicit or configured override.

**Execution note:** Start with failing `superzent_git` tests for "configured base branch beats current branch" and "stale configured base branch falls back to repo default" before changing the helper logic.

**Patterns to follow:**

- `create_workspace` and `CreateWorkspaceOptions` in `crates/superzent_git/src/lib.rs`
- `default_branch` semantics in `crates/git/src/repository.rs`
- Existing inline test style in `crates/superzent_git/src/lib.rs`

**Test scenarios:**

- Happy path: when `.superzent/config.json` sets `base_branch` to an existing branch, a new managed workspace is created from that branch even if the repo is currently checked out elsewhere.
- Happy path: a one-off create-time override wins over the configured `base_branch`.
- Edge case: when `base_branch` is absent, the create flow uses the repository default branch instead of the current branch.
- Edge case: when `base_branch` points at a missing branch, the helper falls back to the repository default branch and returns a warning source that can be surfaced to the UI.
- Error path: invalid `.superzent/config.json` shape causes an explicit create failure before any lifecycle command runs.
- Integration: committed `.superzent` files remain available in the new worktree through normal git checkout without the old manual copy path.

**Verification:**

- Managed workspace creation no longer depends on the current checked-out branch.
- The create flow has one authoritative config contract and no hidden copy semantics.

- [x] **Unit 2: Add structured lifecycle command execution and failure outcomes**

**Goal:** Make setup and teardown first-class command phases with captured output, stable environment variables, and result shapes that match the product requirements.

**Requirements:** R3, R7, R8, R10, R11, R12, R13, R14

**Dependencies:** Unit 1

**Files:**

- Modify: `crates/superzent_git/src/lib.rs`
- Test: `crates/superzent_git/src/lib.rs`

**Approach:**

- Introduce a structured lifecycle execution helper that captures phase, command, stdout, stderr, and a concise summary instead of flattening everything into one opaque `anyhow` string.
- Run `setup` after the worktree exists and before returning the final create outcome.
- Keep the created worktree intact on setup failure and return a success outcome that includes lifecycle failure details the UI can inspect.
- Run `teardown` before `git worktree remove`; on failure, return a structured delete failure without removing the worktree unless the caller retries in force-delete mode.
- Export the documented `SUPERZENT_ROOT_PATH`, `SUPERZENT_WORKTREE_PATH`, and `SUPERZENT_WORKSPACE_NAME` variables, while also exporting legacy `SUPERSET_ROOT_PATH` and `SUPERSET_WORKSPACE_NAME` during the transition.

**Execution note:** Start with failing lifecycle tests for setup-failure retention and teardown-failure blocking before reshaping the return types.

**Patterns to follow:**

- `run_shell_command` in `crates/superzent_git/src/lib.rs`
- Existing `WorkspaceCreateOutcome.warning` path in `crates/superzent_git/src/lib.rs`
- Existing stale-path deletion fallbacks in `delete_workspace` in `crates/superzent_git/src/lib.rs`

**Test scenarios:**

- Happy path: multiple setup commands run sequentially in the new worktree and each command sees the expected environment variables.
- Happy path: multiple teardown commands run sequentially before `git worktree remove`.
- Edge case: a setup command failure leaves the new worktree on disk and returns structured failure output rather than deleting the worktree.
- Edge case: a teardown command failure leaves the worktree on disk and returns a structured failure that identifies the teardown phase.
- Error path: a force-delete retry skips teardown and still removes the worktree when teardown had previously failed.
- Integration: both `SUPERZENT_*` and legacy `SUPERSET_*` variables are visible to lifecycle commands during the transition window.

**Verification:**

- Lifecycle command failures are inspectable as structured outcomes.
- Setup failure no longer collapses into "workspace creation failed" semantics.
- Teardown failure no longer forces the UI to choose between silent deletion and a generic error toast.

- [x] **Unit 3: Add local create-flow base-branch preview and setup-failure presentation**

**Goal:** Make the managed local create flow show the effective base branch before creation and preserve/open the workspace when setup fails.

**Requirements:** R12, R15, R16, R17, R18, R19

**Dependencies:** Unit 1, Unit 2

**Files:**

- Modify: `crates/superzent_ui/src/lib.rs`
- Test: `crates/superzent_ui/src/lib.rs`

**Approach:**

- Extend `NewWorkspaceModal` so local managed workspace creation can carry an optional base-branch override and show the resolved effective base branch plus any fallback warning before the user confirms.
- Keep the remote create path unchanged for v1 rather than pretending the new local config applies there.
- Thread the optional override through `spawn_new_workspace_request` into `superzent_git::create_workspace`.
- When setup fails but creation succeeds, keep the existing open-and-toast success path, but upgrade the notice from a plain warning string to a structured setup-failure summary with access to full logs.
- Reuse the current post-open toast seam for concise notices so the create flow stays lightweight when setup succeeds.

**Execution note:** Implement helper-level tests for base-branch preview state before wiring the modal interactions.

**Patterns to follow:**

- `NewWorkspaceModal` and `spawn_new_workspace_request` in `crates/superzent_ui/src/lib.rs`
- Current `warning` toast handling after workspace open in `crates/superzent_ui/src/lib.rs`
- Existing local-vs-remote branch in `spawn_new_workspace_request`

**Test scenarios:**

- Happy path: for a local project with configured `base_branch`, the modal shows the effective base branch before create and passes it through when the user does not override it.
- Happy path: entering a one-off override changes the effective base branch shown to the user and is passed into the create request.
- Edge case: when the configured `base_branch` is stale, the create UI shows the fallback effective branch plus a warning before the user confirms.
- Edge case: remote projects continue to use the existing branch-name-only create path and do not incorrectly show local lifecycle config UI.
- Error path: setup failure still results in the workspace being upserted and opened, followed by a visible failure summary rather than a top-level create abort.
- Integration: the branch preview state and the create request stay in sync when the user edits the override field multiple times before confirming.
- Integration: re-opening an already-created workspace through the normal workspace-open path does not invoke setup again, proving lifecycle execution remains confined to managed create/delete seams.

**Verification:**

- Users can tell which base branch will be used before creating a managed local workspace.
- Setup failure no longer prevents the new workspace from opening.

- [x] **Unit 4: Add teardown-failure logs and force-delete recovery in the delete flow**

**Goal:** Turn teardown failure into an explicit recovery path rather than a terminal generic error toast.

**Requirements:** R8, R13, R14

**Dependencies:** Unit 2

**Files:**

- Modify: `crates/superzent_ui/src/lib.rs`
- Test: `crates/superzent_ui/src/lib.rs`
- Test: `crates/superzent_git/src/lib.rs`

**Approach:**

- Keep the current delete confirmation prompt for the initial destructive action.
- When local managed deletion fails specifically in teardown, surface the structured failure details in a dedicated lifecycle-failure recovery surface that offers log viewing and an explicit force-delete retry.
- Ensure force-delete retries call back into `superzent_git::delete_workspace` with teardown skipped, then continue through the current workspace/store cleanup path.
- Preserve current success behavior and remote deletion behavior outside the new local teardown-failure branch.
- Reset sidebar "deleting" state on all exit paths, including cancel-after-failure.

**Execution note:** Start with failing UI-level tests for "teardown failure leaves workspace intact" and "force delete removes it on retry".

**Patterns to follow:**

- `run_delete_workspace_entry` in `crates/superzent_ui/src/lib.rs`
- Existing delete confirmation prompt and sidebar deleting-state handling in `crates/superzent_ui/src/lib.rs`
- Existing `delete_workspace` stale-path cleanup tests in `crates/superzent_git/src/lib.rs`

**Test scenarios:**

- Happy path: successful delete still removes the worktree, closes the workspace in all windows, and removes the store entry.
- Edge case: teardown failure leaves the worktree and workspace entry intact and resets the UI deleting state.
- Happy path: selecting force delete after teardown failure retries without teardown and completes the delete.
- Edge case: cancelling after teardown failure leaves the workspace available and does not leak a stuck deleting indicator.
- Error path: a second failure during force delete is still surfaced as a delete failure without partially removing the store entry.
- Integration: remote workspace deletion remains on the existing path and is unaffected by the local teardown-recovery UI.

**Verification:**

- Users cannot accidentally lose teardown failures behind a generic toast.
- Force delete is explicit, local to teardown failure, and does not change the normal delete path.

- [x] **Unit 5: Document the repo lifecycle contract**

**Goal:** Teach the new `.superzent/config.json` behavior in the main `superzent` docs so teams can adopt it without reading implementation details.

**Requirements:** R1, R2, R3, R11, R12, R13

**Dependencies:** Unit 1, Unit 2, Unit 3, Unit 4

**Files:**

- Modify: `README.md`
- Modify: `docs/src/getting-started.md`
- Modify: `docs/src/SUMMARY.md`
- Create: `docs/src/managed-workspace-lifecycle.md`

**Approach:**

- Add a dedicated docs page that explains the config file location, supported keys, create/delete-only semantics, local-only scope, environment variables, setup-failure behavior, and teardown-failure force-delete behavior.
- Link that page from the getting-started flow and the docs summary so it is discoverable from the core workspace/worktree onboarding path.
- Keep examples aligned with the product contract: repo-root `.superzent/config.json`, tracked setup scripts, and `base_branch` plus one-off override.

**Patterns to follow:**

- User-facing workflow explanations in `docs/src/getting-started.md`
- Navigation style in `docs/src/SUMMARY.md`
- Top-level docs linking style in `README.md`

**Test scenarios:**

- Test expectation: none -- documentation-only unit.

**Verification:**

- A user can discover and understand the managed workspace lifecycle contract from the repo docs without reading source code or hidden historical behavior.

## System-Wide Impact

- **Interaction graph:** `NewWorkspaceModal` -> `spawn_new_workspace_request` -> `superzent_git::create_workspace` -> config parse -> base-branch resolution -> setup execution -> store upsert/open; and `run_delete_workspace_entry` -> `superzent_git::delete_workspace` -> teardown execution -> either failure recovery or workspace/store removal.
- **Error propagation:** setup and teardown failures should propagate as structured lifecycle outcomes to `superzent_ui`; only the UI should flatten them into user-facing summaries or recovery prompts.
- **State lifecycle risks:** create-time partial side effects are now intentional; the plan must preserve the worktree and avoid accidental cleanup on setup failure. Delete-time partial side effects must not remove the store entry until teardown succeeds or the user explicitly force deletes.
- **API surface parity:** the new config contract and environment variables are repo-facing surfaces, so accidental divergence between docs and implementation would create silent workflow breakage. The generic `git_ui` worktree picker is an explicit non-goal in this phase and should remain behaviorally unchanged.
- **Integration coverage:** the highest-value cross-layer scenarios are "configured base branch preview matches the eventual create request", "setup failure still opens the workspace", and "teardown failure plus force delete keeps store and filesystem state coherent".
- **Unchanged invariants:** remote workspace creation/deletion semantics remain unchanged; non-managed and primary workspaces remain undeletable; generic git worktree flows outside managed local creation keep their existing behavior.

## Risks & Dependencies

| Risk                                                                                                   | Mitigation                                                                                                                                                                        |
| ------------------------------------------------------------------------------------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Existing internal repos may already depend on hidden `SUPERSET_*` env names or `copy` behavior         | Dual-export env names during the transition, document `SUPERZENT_*` as canonical, and remove `copy` only where tests prove tracked-file/script workflows cover the supported path |
| Base-branch preview in the create modal can drift from the branch actually used by `superzent_git`     | Use one shared resolution helper or shared result shape between UI preview and create execution rather than re-implementing fallback logic twice                                  |
| Setup-failure retention can leave partially initialized local resources that users mistake for success | Surface concise failure messaging immediately on open and provide log access rather than hiding the failure inside background logs                                                |
| Teardown force-delete could remove a workspace even though cleanup commands never ran                  | Keep force delete as an explicit second-step recovery action only after a visible teardown failure, not as part of the normal delete path                                         |
| Adding `git` crate dependency to `superzent_git` could be heavier than expected                        | Evaluate reuse-vs-copy during implementation and choose the smaller, testable option without changing the planned semantics                                                       |

## Documentation / Operational Notes

- The docs should call out that lifecycle config is repo-root only in v1 and that remote workspaces are intentionally out of scope.
- Examples should prefer tracked scripts like `./.superzent/setup.sh` rather than inline long shell commands.
- The PR description should note that `SUPERZENT_*` is the supported env contract and whether legacy `SUPERSET_*` compatibility is still present.

## Sources & References

- **Origin document:** `docs/brainstorms/2026-04-06-managed-workspace-lifecycle-config-requirements.md`
- Related code: `crates/superzent_git/src/lib.rs`
- Related code: `crates/superzent_ui/src/lib.rs`
- Related code: `crates/git/src/repository.rs`
- Related docs: `docs/src/getting-started.md`
- Related docs: `README.md`
- Institutional learning: `docs/solutions/integration-issues/managed-terminal-popup-notifications-2026-04-04.md`
- Institutional learning: `docs/solutions/integration-issues/default-build-next-edit-surface-restoration-2026-04-05.md`
