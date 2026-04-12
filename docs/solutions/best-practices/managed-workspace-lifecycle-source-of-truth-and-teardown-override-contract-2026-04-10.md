---
title: Managed workspace lifecycle should keep repo config as the default and persist only teardown overrides
date: 2026-04-10
category: best-practices
module: managed workspace lifecycle
problem_type: best_practice
component: tooling
severity: high
applies_when:
  - Refactoring or extending managed local workspace create and delete behavior
  - Changing how repo-root `.superzent/config.json` lifecycle defaults interact with workspace persistence
  - Adding or reshaping delete confirmations that must preview the exact teardown behavior
  - Narrowing persisted state so repo defaults do not get copied through unrelated sync paths
symptoms:
  - Lifecycle defaults and workspace persistence were coupled across model, git, and UI layers
  - The create modal save toggle implied broader persistence than the actual one-shot setup contract
  - Delete confirmation could not reliably present the final teardown behavior up front
tags:
  [
    managed-workspace-lifecycle,
    teardown-override,
    source-of-truth,
    delete-preview,
    workspace-persistence,
  ]
---

# Managed workspace lifecycle should keep repo config as the default and persist only teardown overrides

## Context

Managed workspace lifecycle behavior had drifted into a multi-source implementation. Repo config, modal inputs, workspace persistence, sync builders, and delete helpers all carried lifecycle meaning at once. That made small behavior changes expensive because each change needed updates across unrelated layers.

The fix was to make the contract narrower and more honest: repo-root `.superzent/config.json` stays the default source of truth, and workspace state is allowed to persist only one explicit exception, a teardown override chosen at workspace creation time.

## Guidance

Keep repo defaults and workspace-local exceptions separate.

- Treat repo-root `.superzent/config.json` as the default source of truth for `base_branch`, `setup`, and `teardown`.
- Treat `setup` as create-time-only input and one-shot execution, not ongoing workspace state.
- Persist only `teardown`, and only when the user entered a non-saved teardown script that differs from the repo default.
- Precompute modal bootstrap data before constructing the modal so the UI consumes plain data instead of re-entering live workspace state.
- Resolve delete behavior before opening the destructive confirmation, then execute deletion against that same resolved plan so the preview and the real delete path cannot drift.

## Why This Matters

This pattern removes persistence ambiguity and makes delete preview deterministic. Workspace sync no longer carries repo defaults through unrelated paths, the save toggle no longer implies that `setup` persists, and the delete prompt no longer guesses at behavior from a later config read or failure branch.

## When to Apply

- When repo-scoped defaults coexist with one-off per-instance input
- When create-time setup should never become persisted workspace behavior
- When only one delete-time choice must survive restart
- When a destructive confirmation must preview the exact action that will run, even if config is unreadable

## Examples

Store a workspace-local teardown override only when it is both unsaved and meaningfully different from the repo default:

```rust
fn workspace_teardown_script_override_for_create(
    config: &SuperzentConfig,
    teardown_script: Option<&str>,
    save_teardown_script_as_repo_default: bool,
) -> Option<String> {
    if save_teardown_script_as_repo_default {
        return None;
    }

    let teardown_script = normalize_command(teardown_script);
    let repo_default_teardown_script = commands_to_script(&config.teardown);

    if teardown_script == repo_default_teardown_script {
        None
    } else {
        teardown_script
    }
}
```

Resolve delete behavior once, preview it, then execute against that same plan:

```rust
pub enum WorkspaceDeleteResolution {
    RunTeardownScript { script: String },
    SkipTeardown,
    BlockedByConfig(WorkspaceLifecycleFailure),
}
```

```rust
let delete_resolution =
    resolve_workspace_delete_resolution(&session.workspace_entry, repo_root)?;
let prompt_receiver = show_delete_workspace_modal(
    workspace,
    session.workspace_entry.clone(),
    delete_resolution.clone(),
    window,
    cx,
);
let delete_result = perform_workspace_delete(
    &session,
    &store,
    Some(&delete_resolution),
    force,
    cx,
)
.await;
```

In the blocked-config branch, the prompt must explicitly say that normal delete is blocked and that `Delete Anyway` skips teardown, rather than retrying an unknown script.

## Related

- [managed-terminal-popup-notifications-2026-04-04.md](/Users/junpark/codingcoding/.superzent-worktrees/superzent/worktree-setup/docs/solutions/integration-issues/managed-terminal-popup-notifications-2026-04-04.md) — similar pattern: fix the earliest lifecycle/config stage instead of patching the final UI symptom
- [default-build-next-edit-surface-restoration-2026-04-05.md](/Users/junpark/codingcoding/.superzent-worktrees/superzent/worktree-setup/docs/solutions/integration-issues/default-build-next-edit-surface-restoration-2026-04-05.md) — another example of narrowing sources of truth and removing helper-level coupling
- Related requirements: `docs/brainstorms/2026-04-10-managed-workspace-lifecycle-compaction-requirements.md`
- Related plan: `docs/plans/2026-04-10-001-refactor-managed-workspace-lifecycle-compaction-plan.md`
