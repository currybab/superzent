---
title: Managed workspace create progress toasts can fail to appear after local open
date: 2026-04-12
category: ui-bugs
module: managed workspace create flow
problem_type: ui_bug
component: tooling
symptoms:
  - During managed local workspace creation, progress status toasts such as `Running setup…`, `Moving local changes…`, and the final setup success toast could fail to appear even when the work visibly took time
  - The workspace could open and setup could run, but the user got no visible in-context progress feedback
  - Debug logs could report `opened_workspace_present=false` after open, leaving the create flow without a reliable toast target
root_cause: wrong_api
resolution_type: code_fix
severity: medium
tags:
  [
    managed-workspace,
    create-flow,
    status-toast,
    workspace-resolution,
    ui-feedback,
  ]
---

# Managed workspace create progress toasts can fail to appear after local open

## Problem

The managed local workspace create flow opened the workspace successfully, but progress status toasts were often not visible because the code tried to rediscover the newly opened workspace later from `workspace_entry`. When that second lookup missed, the status toast had no reliable `Entity<Workspace>` target, so transient progress UI never rendered where the user could see it.

## Symptoms

- `Running setup…`, `Moving local changes…`, and `Setup finished...` could fail to appear even when workspace setup visibly took time.
- Debug logs showed the flow could not reliably resolve the opened workspace after the open step.

## What Didn't Work

- Re-resolving the new workspace later with `resolve_opened_workspace(&workspace_entry, ...)` was too indirect for the local path.
- The flow already knew when it opened a brand-new local workspace, but it threw away the returned UI identity and later tried to infer it from matching logic.
- The older persistent “setup in progress” sidebar state was the wrong abstraction. It stored long-lived UI state for a short-lived create/setup step and still did not guarantee visible progress feedback.

## Solution

Add a local-only helper that opens the worktree and immediately returns the live `Entity<Workspace>` instead of `()`. For local paths, that helper either reuses an already-open workspace or returns the newly opened one directly from the open result.

Use that returned handle in the managed create flow and pin progress status toasts to it:

```rust
let visible_workspace = match &workspace_entry.location {
    WorkspaceLocation::Local { worktree_path } => {
        let open_task = cx.update(|window, cx| {
            open_local_workspace_path_and_resolve(
                worktree_path.clone(),
                app_state.clone(),
                window,
                cx,
            )
        })?;
        match open_task.await {
            Ok(workspace) => Some(workspace),
            Err(error) => {
                show_workspace_toast_async(
                    &workspace_handle,
                    format!("Failed to open workspace: {error}"),
                    cx,
                );
                return Ok::<(), anyhow::Error>(());
            }
        }
    }
    WorkspaceLocation::Ssh { .. } => {
        open_workspace_entry(workspace_entry.clone(), app_state.clone(), window, cx).await?;
        resolve_opened_workspace(&workspace_entry, current_window_handle, cx).await
    }
};
let opened_workspace = visible_workspace.clone();

show_resolved_workspace_status_toast(
    opened_workspace.as_ref(),
    "Running setup…",
    ToastIcon::new(IconName::ArrowCircle).color(Color::Muted),
    cx,
);
```

The same opened workspace handle is reused for `Moving local changes…` and the final setup success toast.

## Why This Works

`StatusToast` is workspace-scoped UI. It must be toggled on the actual `Entity<Workspace>` that is rendering the status layer. The local open path already knows exactly which workspace was created or re-activated, so carrying that handle forward removes the fragile second lookup.

## Prevention

- When a flow opens a workspace and immediately needs workspace-scoped UI, prefer an open API that returns `Entity<Workspace>` instead of reopening or re-resolving later from `WorkspaceEntry`.
- Use `resolve_opened_workspace` as a fallback for paths like SSH where the open primitive cannot directly hand back the live workspace, not as the default for local opens.
- Keep create/setup progress non-persistent unless users need resumable state; transient status belongs in toasts, not store-backed row state.
- Add an integration test around the managed local create flow that asserts the opened workspace handle is available immediately after open and is the same handle used for progress toasts.

## Related Issues

- Related solution: [managed-workspace-lifecycle-source-of-truth-and-teardown-override-contract-2026-04-10.md](/Users/junpark/codingcoding/.superzent-worktrees/superzent/worktree-setup/docs/solutions/best-practices/managed-workspace-lifecycle-source-of-truth-and-teardown-override-contract-2026-04-10.md)
- Related solution: [managed-terminal-popup-notifications-2026-04-04.md](/Users/junpark/codingcoding/.superzent-worktrees/superzent/worktree-setup/docs/solutions/integration-issues/managed-terminal-popup-notifications-2026-04-04.md)
