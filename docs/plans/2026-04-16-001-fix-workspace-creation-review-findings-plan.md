---
title: "fix: Preserve selected preset when reusing SSH workspace entries"
type: fix
status: active
date: 2026-04-16
origin: docs/plans/2026-04-15-001-feat-workspace-creation-preset-control-plan.md
---

# fix: Preserve selected preset when reusing SSH workspace entries

## Overview

Fix the review finding from the workspace creation preset-control work: when SSH workspace creation reuses an existing `WorkspaceEntry` for the target location, the selected preset from `NewWorkspaceModal` can be overwritten by stale stored workspace metadata. The fix should keep the selected preset as the source of truth for the current create request and add regression coverage for the SSH reuse path.

## Problem Frame

The preset-control change correctly threads `preset_id` through `confirm()` and `spawn_new_workspace_request`, but `create_remote_workspace()` still rebuilds the returned `WorkspaceEntry` by preferring `existing_workspace.agent_preset_id` over the newly selected `preset_id`. That breaks the intended contract from the origin plan: the preset chosen in the creation modal should become the workspace's assigned preset even when auto-launch is OFF.

## Requirements Trace

- R1. SSH workspace creation must preserve the selected preset from the current create request, even when an existing workspace entry already exists for the target SSH location.
- R2. The fix must not change the existing behavior for unrelated reused metadata such as `id`, `display_name`, `git_summary`, and `attention_status`.
- R3. Add regression coverage proving the SSH reuse path keeps the new `preset_id` instead of stale stored preset state.

## Scope Boundaries

- Do not change local workspace creation behavior.
- Do not change sidebar preset-launch behavior for existing workspaces.
- Do not add broader end-to-end modal UI tests in this pass unless they are required to verify the SSH reuse fix.

## Context & Research

### Relevant Code and Patterns

- `crates/superzent_ui/src/lib.rs`
  - `create_remote_workspace()` currently prefers `existing_workspace.agent_preset_id` when rebuilding the returned `WorkspaceEntry`.
  - `build_synced_local_workspace_entry()`, `build_local_workspace_bundle()`, and `build_remote_workspace_bundle()` show the existing pattern of selectively reusing workspace metadata from stored entries.
  - Existing test helpers `preset()`, `ssh_connection()`, and `ssh_workspace_entry()` can support a focused unit regression test in the same file.

### Institutional Learnings

- `docs/solutions/logic-errors/restore-field-level-managed-workspace-default-saves-2026-04-13.md`
  - Create-time selections should be threaded explicitly through retries and async create flows rather than reconstructed from stored defaults later.
- `docs/solutions/best-practices/managed-workspace-lifecycle-source-of-truth-and-teardown-override-contract-2026-04-10.md`
  - Keep transient create-time UI choices separate from long-lived workspace persistence.

## Key Technical Decisions

- **Prefer the incoming `preset_id` over `existing_workspace.agent_preset_id` in the SSH create path.**

  - Rationale: the current create request is the authoritative source for the workspace's assigned preset. Reused metadata should preserve identity and display state, but not override a fresh user choice.

- **Extract or isolate the workspace-entry construction enough to unit test the preset merge rule directly.**
  - Rationale: a focused unit test in `crates/superzent_ui/src/lib.rs` is cheaper and more stable than trying to stand up a full SSH create flow test, while still proving the regression is fixed.

## Implementation Units

- [x] **Unit 1: Fix SSH workspace preset reuse**

  **Goal:** Ensure `create_remote_workspace()` keeps the selected `preset_id` when reusing an existing workspace entry for the same SSH location.

  **Requirements:** R1, R2

  **Files:**

  - Modify: `crates/superzent_ui/src/lib.rs`

  **Approach:**

  - Update the `WorkspaceEntry` construction in the SSH create path so `agent_preset_id` comes from the incoming `preset_id`, not the reused workspace entry.
  - Preserve reuse for the unrelated fields that intentionally carry over (`id`, `display_name`, `git_summary`, `attention_status`, `review_pending`, `last_attention_reason`, `teardown_script_override`, timestamps).

  **Patterns to follow:**

  - Existing merge pattern in `build_remote_workspace_bundle()` and `build_synced_local_workspace_entry()`, while explicitly overriding only the field that should be request-owned.

  **Verification:**

  - Code review of the reconstructed `WorkspaceEntry` shows only `agent_preset_id` behavior changes for reused SSH entries.

- [x] **Unit 2: Add regression coverage for the SSH reuse path**

  **Goal:** Lock down the selected-preset behavior with a focused test.

  **Requirements:** R3

  **Files:**

  - Modify: `crates/superzent_ui/src/lib.rs` (test module)

  **Test file paths:**

  - `crates/superzent_ui/src/lib.rs`

  **Approach:**

  - Add a pure helper if needed to make the merge rule testable without constructing a full async SSH workspace flow.
  - Add a test that starts with an existing SSH workspace entry whose `agent_preset_id` differs from the requested `preset_id`, then asserts the rebuilt/reused workspace entry uses the requested preset.
  - Keep the test narrow: verify preset assignment while preserving representative reused metadata such as `id`.

  **Test scenarios:**

  - Happy path: reused SSH workspace entry with stale preset + new requested preset -> resulting workspace entry uses the new requested preset.
  - Happy path: reused SSH workspace entry still preserves stable metadata like `id`.

  **Verification:**

  - `cargo test -p superzent_ui --lib`

## Risks & Dependencies

| Risk                                                                        | Mitigation                                                                        |
| --------------------------------------------------------------------------- | --------------------------------------------------------------------------------- |
| The fix accidentally changes other reuse semantics beyond `agent_preset_id` | Keep the implementation narrow and verify preserved fields in the regression test |
| The test is hard to write against the async SSH create flow                 | Extract a small pure helper and test the merge rule directly                      |

## Sources & References

- Origin plan: `docs/plans/2026-04-15-001-feat-workspace-creation-preset-control-plan.md`
- Related solution: `docs/solutions/logic-errors/restore-field-level-managed-workspace-default-saves-2026-04-13.md`
- Related solution: `docs/solutions/best-practices/managed-workspace-lifecycle-source-of-truth-and-teardown-override-contract-2026-04-10.md`
