---
title: Restore field-level managed workspace default saves
date: 2026-04-13
category: logic-errors
module: managed workspace create flow
problem_type: logic_error
component: tooling
symptoms:
  - The create modal only allowed saving teardown as a repo default, so setup could no longer be promoted from the create flow
  - Setup and teardown repo-default changes were no longer handled independently during workspace creation
  - Dirty-workspace retry paths needed to preserve lifecycle script text and save selections across the second create attempt
root_cause: logic_error
resolution_type: code_fix
severity: high
tags:
  [
    managed-workspace,
    create-flow,
    repo-defaults,
    lifecycle,
    setup,
    teardown,
    dirty-workspace,
  ]
---

# Restore field-level managed workspace default saves

## Problem

The managed workspace create flow regressed into teardown-only save semantics during lifecycle compaction. Users could still enter both `setup` and `teardown` scripts in the modal, but only teardown had a repo-default persistence path, so a one-off `setup` script could not be promoted back into `.superzent/config.json`.

## Symptoms

- A `setup` script could run for the current create flow but would not prefill future creates as a repo default.
- Clearing and saving one lifecycle field could not be expressed independently from the other field.
- The dirty-workspace retry path had to preserve lifecycle script text and save selections across the second create attempt.

## What Didn't Work

- Keeping the post-compaction create request as a teardown-only save contract. That preserved the narrowed teardown-override model, but it also removed legitimate repo-default `setup` authoring from the create flow. (session history)
- Treating one save toggle as enough for both lifecycle fields. Earlier review and brainstorm passes already surfaced that the create-modal save semantics were underdefined when `setup` and `teardown` shared one control. (session history)
- Rebuilding retry-time create state from scratch after the dirty-workspace prompt. That made it too easy to lose lifecycle script text or save selections on the second attempt. (session history)

## Solution

Introduce a field-level repo-default save contract and carry that same contract through config staging, retry flows, and modal UI.

In `crates/superzent_git/src/lib.rs`, replace the teardown-only boolean with an explicit per-field selection type:

```rust
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WorkspaceLifecycleDefaultSaveSelections {
    pub setup_script: bool,
    pub teardown_script: bool,
}
```

Use that selection set when staging `.superzent/config.json`, so only the selected fields are rewritten and empty selected values clear that field:

```rust
fn prepare_superzent_config_for_create(
    repo_root: &Path,
    setup_script: Option<String>,
    teardown_script: Option<String>,
    save_lifecycle_defaults: WorkspaceLifecycleDefaultSaveSelections,
) -> Result<SuperzentConfig> {
    let mut config = load_superzent_config(repo_root)?;
    if save_lifecycle_defaults.setup_script {
        config.setup = setup_script
            .as_deref()
            .map(split_commands)
            .unwrap_or_default();
    }
    if save_lifecycle_defaults.teardown_script {
        config.teardown = teardown_script
            .as_deref()
            .map(split_commands)
            .unwrap_or_default();
    }
    Ok(config)
}
```

Keep config writes transactional with worktree creation instead of writing defaults early:

```rust
let config = prepare_superzent_config_for_create(
    &repo_root,
    configured_setup_script.clone(),
    configured_teardown_script.clone(),
    options.save_lifecycle_defaults,
)?;

run_git(&repo_root, &["worktree", "add", "-b", &branch_name, ...])?;

if options.save_lifecycle_defaults.any() {
    if let Err(error) = write_superzent_config(&repo_root, &config) {
        cleanup_created_worktree(&repo_root, &worktree_path, &branch_name);
        return Err(error);
    }
}
```

In `crates/superzent_ui/src/lib.rs`, keep one `CreateWorkspaceOptions` value alive across the dirty-workspace retry path and only flip `allow_dirty` for the second attempt:

```rust
fn allow_dirty_workspace_create_options(
    mut create_options: superzent_git::CreateWorkspaceOptions,
) -> superzent_git::CreateWorkspaceOptions {
    create_options.allow_dirty = true;
    create_options
}
```

```rust
let create_options = new_workspace_create_options(
    branch_name.clone(),
    base_branch_override.clone(),
    base_workspace_path.clone(),
    setup_script.clone(),
    teardown_script.clone(),
    save_lifecycle_defaults,
    false,
);

match create_local_workspace(project.clone(), preset_id.clone(), create_options.clone(), cx).await {
    Ok(outcome) => Ok(outcome),
    Err(error) if is_dirty_workspace_create_error(&error) => {
        create_local_workspace(
            project.clone(),
            preset_id.clone(),
            allow_dirty_workspace_create_options(create_options),
            cx,
        )
        .await
    }
    Err(error) => Err(error),
}
```

Also move the save controls next to the matching modal fields so `setup` and `teardown` can be saved independently:

```rust
Checkbox::new(
    "superzent-new-workspace-save-setup-default",
    self.save_lifecycle_defaults.setup_script.into(),
)
.label("Save as repo default")
```

```rust
Checkbox::new(
    "superzent-new-workspace-save-teardown-default",
    self.save_lifecycle_defaults.teardown_script.into(),
)
.label("Save as repo default")
```

## Why This Works

This restores the missing behavior without undoing the compaction contract.

- Repo defaults still live only in repo-root `.superzent/config.json`.
- `setup` remains a one-shot create-time action because nothing about the workspace model starts persisting setup state.
- `teardown` remains the only workspace-local override because `workspace_teardown_script_override_for_create` still decides override persistence only for teardown, and only when teardown was not saved as the repo default.
- Config writes remain transactional because they happen after `git worktree add`, with cleanup on write failure.
- Dirty-workspace retries stop losing state because the retry path reuses the same `CreateWorkspaceOptions` instead of re-deriving the lifecycle selection state.

## Prevention

- When two create-time fields have different persistence rules, model save intent per field instead of hiding both behind one shared toggle.
- Keep one regression suite around the high-risk paths: setup-only save, teardown-only save, empty-plus-save clearing, and dirty-workspace/untracked retries that must preserve lifecycle selections.

## Related Issues

- [managed-workspace-lifecycle-source-of-truth-and-teardown-override-contract-2026-04-10.md](../best-practices/managed-workspace-lifecycle-source-of-truth-and-teardown-override-contract-2026-04-10.md) — the broader lifecycle source-of-truth and teardown-override contract this fix preserves
- [managed-workspace-create-progress-toasts-can-fail-to-appear-after-local-open-2026-04-12.md](../ui-bugs/managed-workspace-create-progress-toasts-can-fail-to-appear-after-local-open-2026-04-12.md) — adjacent managed-workspace create-flow UI issue
- GitHub issue search was skipped because `gh issue list` could not reach `api.github.com` from this environment
