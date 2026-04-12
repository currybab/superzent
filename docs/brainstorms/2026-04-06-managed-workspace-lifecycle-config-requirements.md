---
date: 2026-04-06
topic: managed-workspace-lifecycle-config
---

# Managed Workspace Lifecycle Config

## Problem Frame

`superzent` already creates and deletes managed local workspaces through a dedicated flow, but repo-specific lifecycle behavior is still implicit and incomplete. Teams want a committed way to run setup and cleanup steps for each managed workspace, and they also want predictable control over which branch new worktrees are based on.

Without an explicit repo-level lifecycle contract, common tasks like copying env files, starting or stopping local services, and choosing a stable base branch are handled ad hoc. The current behavior is also inconsistent: teardown logic exists in a hidden form, setup is not first-class, and managed workspace creation still relies on implicit branch selection instead of an explicit policy.

## Requirements

**Config Surface**

- R1. For v1, `superzent` must read managed workspace lifecycle config only from the repo-root `.superzent/config.json`.
- R2. `.superzent/config.json` must support three top-level user-facing fields: `setup`, `teardown`, and `base_branch`.
- R3. `setup` and `teardown` must be arrays of shell commands executed in order.
- R4. If `.superzent/config.json` is absent, managed workspace creation and deletion must still work with no lifecycle commands.
- R5. v1 must not add `.superzent/config.local.json`, home-directory overrides, or any other config precedence layer.
- R6. Copying or deleting repo files as part of workspace lifecycle must be expressed through `setup` or `teardown` commands rather than a separate first-class `copy` config field.

**Lifecycle Execution**

- R7. `setup` commands must run only when a managed local workspace is created, after the worktree directory exists.
- R8. `teardown` commands must run only when a managed local workspace is deleted.
- R9. Opening, focusing, closing, or re-opening an existing workspace in the current window must not trigger `setup` or `teardown`.
- R10. Lifecycle commands must run sequentially with the managed worktree as the working directory.
- R11. Lifecycle commands must receive a stable environment contract containing `SUPERZENT_ROOT_PATH`, `SUPERZENT_WORKTREE_PATH`, and `SUPERZENT_WORKSPACE_NAME`.
- R12. If `setup` fails, `superzent` must keep the new worktree and workspace, surface the failure and logs, and leave the workspace available for manual recovery.
- R13. If `teardown` fails, `superzent` must block deletion, surface the failure and logs, and offer an explicit force-delete path that skips the failing teardown on retry.
- R14. v1 lifecycle execution applies only to managed local workspaces and must not attempt to run repo lifecycle automation for SSH or other remote workspaces.

**Base Branch Selection**

- R15. Managed workspace creation must allow a one-off base branch override at create time.
- R16. When no one-off override is provided, managed workspace creation must use `.superzent/config.json` `base_branch` if it is configured.
- R17. When neither a one-off override nor a configured `base_branch` is available, managed workspace creation must fall back to the base workspace's current branch.
- R18. If the configured `base_branch` is missing or no longer exists, `superzent` must fall back to the base workspace's current branch when available, otherwise the repository default branch, and tell the user the configured branch could not be used.
- R19. Before a managed workspace is created, the create flow must make the effective base branch clear to the user.

## Success Criteria

- A repo can commit `.superzent/config.json` and use it to standardize managed workspace setup, cleanup, and default base branch behavior.
- Creating a managed local workspace runs `setup` once, uses the expected effective base branch, and does not re-run setup when the workspace is later re-opened.
- A failed `setup` leaves behind a usable workspace with visible logs so the user can fix the problem in place.
- A failed `teardown` does not silently remove the workspace; the user must explicitly force deletion to continue.
- Teams can express env-file copy, local service startup, cleanup, and similar lifecycle behavior through scripts instead of one-off manual steps.

## Scope Boundaries

- This change applies to managed local workspace creation and deletion flows, not to simple open/close behavior inside the current window.
- This change does not add lifecycle automation for SSH or other remote workspaces in v1.
- This change does not add repo-local or user-local config override layers beyond repo-root `.superzent/config.json`.
- This change does not introduce a first-class `copy` field in the public config surface.
- This change does not require the generic Git worktree picker and every other worktree creation entry point to honor the new config in v1.

## Key Decisions

- Create/delete semantics only: lifecycle automation should map to actual resource creation and removal, not transient window open/close actions.
- Repo-root config only in v1: the first shipped version should avoid config merge and precedence rules.
- Scripts instead of declarative copy rules: one mechanism is easier to teach and extend than both shell hooks and a separate copy DSL.
- Base branch resolution order is `create-time override -> configured base_branch -> base workspace current branch -> repository default branch`.
- Setup failures are recoverable, while teardown failures are protective: creation should continue with visibility, but deletion should stop unless the user explicitly forces it.

## Dependencies / Assumptions

- Managed local workspace creation and deletion already flow through `crates/superzent_git`, which is the natural integration point for v1 lifecycle behavior.
- The repository default branch can be resolved at managed workspace creation time for local repos.

## Outstanding Questions

### Deferred to Planning

- [Affects R6][Technical] What is the lowest-risk way to remove or phase out the hidden `copy` handling that already exists in `crates/superzent_git`?
- [Affects R12][Technical] What is the best existing UI surface for showing setup failure logs and recovery actions during managed workspace creation?
- [Affects R13][Technical] What is the best existing UI surface for showing teardown failure logs and the force-delete action during managed workspace deletion?
- [Affects R19][Needs research] Which managed workspace creation UI should own the base-branch override so the effective branch is visible before creation without widening scope into unrelated worktree flows?
- [Affects R15][Technical] Should the generic Git worktree picker adopt the same base-branch policy in a later follow-up, or remain separate from managed workspace config?

## Next Steps

-> /prompts:ce-plan for structured implementation planning
