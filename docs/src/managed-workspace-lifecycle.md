---
title: Managed Workspace Lifecycle
description: Configure setup, teardown, and default base branches for managed local workspaces.
---

# Managed Workspace Lifecycle

Managed local workspaces can run repo-defined setup and teardown commands through a repo-root `.superzent/config.json`.

This is useful for tasks like:

- copying or generating local env files
- starting or stopping local services
- choosing a default base branch for new managed workspaces

## Config File

Create `.superzent/config.json` in the repository root:

```json
{
  "base_branch": "main",
  "setup": ["./.superzent/setup.sh"],
  "teardown": ["./.superzent/teardown.sh"]
}
```

Supported keys:

- `base_branch`: default base branch for new managed local workspaces
- `setup`: commands run after a managed local workspace is created
- `teardown`: commands run before a managed local workspace is deleted

## How It Works

`superzent` runs these commands only for managed local workspace create/delete flows.

- Create workspace: create git worktree, then run `setup`
- Delete workspace: run `teardown`, then remove the git worktree

Opening, closing, or re-opening an existing workspace does not run `setup` or `teardown`.

## Base Branch Selection

When you create a managed local workspace, `superzent` resolves the base branch in this order:

1. One-off base branch override from the create flow
2. `.superzent/config.json` `base_branch`
3. Base workspace's current branch
4. Repository default branch

If the configured `base_branch` does not exist, `superzent` falls back to the base workspace's current branch when available. If the base workspace has no current branch, it falls back to the repository default branch and warns before creation continues.

## Environment Variables

Lifecycle commands run in the new or existing worktree directory and receive:

- `SUPERZENT_ROOT_PATH`
- `SUPERZENT_WORKTREE_PATH`
- `SUPERZENT_WORKSPACE_NAME`

For compatibility during the transition, `SUPERSET_ROOT_PATH` and `SUPERSET_WORKSPACE_NAME` are also exported.

## Failure Behavior

### Setup failure

If `setup` fails:

- the worktree is kept
- the workspace is still added and opened
- `superzent` shows the failure and command output so you can fix the workspace in place

### Teardown failure

If `teardown` fails:

- deletion stops
- the workspace and worktree are kept
- `superzent` shows the failure and command output
- you can explicitly choose `Delete Anyway` to retry deletion without running teardown again

## Scope

Current v1 scope:

- repo-root `.superzent/config.json` only
- managed local workspaces only
- create/delete lifecycle only

Not included in this phase:

- remote workspace lifecycle automation
- `.superzent/config.local.json`
- home-directory override layers
- generic git worktree flows outside the managed workspace shell
