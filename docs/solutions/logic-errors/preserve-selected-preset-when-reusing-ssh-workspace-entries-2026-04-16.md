---
title: Preserve selected preset when reusing SSH workspace entries
date: 2026-04-16
category: logic-errors
module: managed workspace create flow
problem_type: logic_error
component: tooling
symptoms:
  - The SSH create flow could ignore the preset selected in `NewWorkspaceModal` when an existing `WorkspaceEntry` was reused for the target location
  - The rebuilt remote workspace entry could carry forward a stale `agent_preset_id` from stored workspace state instead of the current request's `preset_id`
  - The created workspace could end up assigned the wrong preset even though the modal and request plumbing passed the selected preset through correctly
root_cause: logic_error
resolution_type: code_fix
severity: medium
tags: [managed-workspace, create-flow, ssh, preset, workspace-entry]
---

# Preserve selected preset when reusing SSH workspace entries

## Problem

A review of the workspace creation preset-control work found that SSH
workspace creation could silently ignore the preset selected in
`NewWorkspaceModal`. The request path threaded `preset_id` correctly
through `confirm()` and `spawn_new_workspace_request`, but
`create_remote_workspace()` rebuilt the returned `WorkspaceEntry` by
preferring stale metadata from an existing workspace entry for the same
SSH location.

## Symptoms

- Creating an SSH workspace for a location that already had a
  `WorkspaceEntry` could keep the old `agent_preset_id` instead of the
  preset the user just picked in the modal.
- The create flow looked successful in the UI, but the resulting
  workspace could still be assigned the wrong agent preset.
- The bug only became visible when the SSH create path reused existing
  workspace metadata, which made it easy to miss during the initial
  feature work.

## What Didn't Work

- Threading `preset_id` through the modal and into
  `spawn_new_workspace_request` was necessary, but it did not fix the
  SSH reuse branch. `create_remote_workspace()` still reconstructed the
  entry from `existing_workspace` and could overwrite the fresh request
  with stale `agent_preset_id` (session history).

```rust
agent_preset_id: existing_workspace
    .as_ref()
    .map(|workspace| workspace.agent_preset_id.clone())
    .unwrap_or(preset_id),
```

- Reviewing only the modal plumbing gave false confidence. The request
  shape was correct, but the later SSH reconciliation step still
  re-derived a request-owned field from stored state (session history).

## Solution

`crates/superzent_ui/src/lib.rs` now uses a dedicated helper,
`build_created_remote_workspace_entry(...)`, to rebuild the SSH
workspace entry while preserving identity and display metadata from any
reused workspace, but always taking the preset from the current
request.

The critical change is that `agent_preset_id` now comes from the
requested `preset_id`, not from `existing_workspace`:

```rust
// Before
agent_preset_id: existing_workspace
    .as_ref()
    .map(|workspace| workspace.agent_preset_id.clone())
    .unwrap_or(preset_id),

// After
agent_preset_id: preset_id.to_string(),
```

The helper is called from `create_remote_workspace()` like this:

```rust
build_created_remote_workspace_entry(
    &project,
    &branch_name,
    workspace_location,
    existing_workspace.as_ref(),
    &preset_id,
)
```

A regression test,
`build_created_remote_workspace_entry_uses_requested_preset_when_reusing_workspace`,
verifies the reuse case directly. It constructs an existing SSH
workspace entry with one preset, calls the helper with a different
requested preset, and asserts that the resulting entry keeps the
existing workspace identity while using the new preset:

```rust
let workspace_entry = build_created_remote_workspace_entry(
    &project,
    "feature-a",
    workspace_location,
    Some(&existing_workspace),
    "claude-code",
);

assert_eq!(workspace_entry.id, existing_workspace_id);
assert_eq!(workspace_entry.agent_preset_id, "claude-code".to_string());
```

## Why This Works

The create request is now the source of truth for `agent_preset_id`.
The helper still reuses durable metadata from the existing SSH workspace
entry, but it stops treating the old preset as reusable state. That
keeps workspace identity reuse intact without overriding the user's
current selection.

## Prevention

- For create flows that can reuse existing objects, treat request-owned
  fields as authoritative and reuse stored state only for identity or
  durable metadata.
- Keep a focused regression test around the reuse merge rule. If the SSH
  path starts preferring stale workspace state again,
  `build_created_remote_workspace_entry_uses_requested_preset_when_reusing_workspace`
  should fail immediately.
- There is still no full end-to-end SSH create-flow test covering modal
  state through async creation and open. That remains a useful future
  hardening step, but the helper-level regression test is the minimum
  guardrail for this bug.

## Related Issues

- [docs/plans/2026-04-16-001-fix-workspace-creation-review-findings-plan.md](../../plans/2026-04-16-001-fix-workspace-creation-review-findings-plan.md)
- [docs/plans/2026-04-15-001-feat-workspace-creation-preset-control-plan.md](../../plans/2026-04-15-001-feat-workspace-creation-preset-control-plan.md)
- [restore-field-level-managed-workspace-default-saves-2026-04-13.md](./restore-field-level-managed-workspace-default-saves-2026-04-13.md)
- [managed-workspace-create-progress-toasts-can-fail-to-appear-after-local-open-2026-04-12.md](../ui-bugs/managed-workspace-create-progress-toasts-can-fail-to-appear-after-local-open-2026-04-12.md)
- Verified with `cargo test -p superzent_ui --lib`
