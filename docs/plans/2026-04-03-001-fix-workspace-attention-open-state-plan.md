---
title: Fix workspace attention and open state signaling
type: fix
status: completed
date: 2026-04-03
origin: docs/brainstorms/2026-04-03-workspace-attention-and-open-state-requirements.md
---

# Fix workspace attention and open state signaling

## Overview

Separate two meanings that are currently blended in the workspace sidebar row: `attention` and `open`. The left dot should only communicate attention state, while the right-side row status should only communicate whether the workspace is loaded in the current window and, if open, whether it has git changes. The implementation also needs to stop treating startup-time terminal input as proof of live work, and it now also needs to close the preset launch lifecycle so terminal presets reliably transition from yellow to green on completion.

## Problem Frame

The current sidebar behavior makes an open workspace look like a working workspace. In practice, users are trying to answer two different questions: "is this workspace loaded right now?" and "does this workspace need my attention?" The current implementation answers both through attention-derived state and terminal-input heuristics, which causes false `Working` yellow dots at startup and makes closed workspaces look more active than they are. This plan preserves the product intent defined in the origin doc while grounding the solution in the existing `SuperzentSidebar`, `WorkspaceAttentionController`, and `SuperzentStore` code paths (see origin: `docs/brainstorms/2026-04-03-workspace-attention-and-open-state-requirements.md`).

## Requirements Trace

- R1-R4. Attention remains a dedicated semantic channel and no longer restores `Working` from non-live startup signals.
- R12-R13. Preset-launched terminal runs transition to `Review`/green on completion and use the same lifecycle whether or not they were launched with an initial task prompt.
- R5-R7. Open state is derived from workspaces loaded in the current window and rendered independently from attention.
- R8-R11. Right-side row status is shown only for open workspaces, with `Open` replacing the git pill only when the workspace is open and has no git changes.

## Scope Boundaries

- This plan changes workspace sidebar semantics only; it does not redefine title bar badges, notification popups, or other workspace indicators.
- This plan preserves the existing meaning of `Review`, `Working`, and `Permission` colors instead of redesigning the attention model.
- This plan does not introduce multi-window open-state logic; "open" stays scoped to the current `MultiWorkspace` window.

## Context & Research

### Relevant Code and Patterns

- `crates/superzent_ui/src/lib.rs`
  - `WorkspaceAttentionController::handle_hook_event` is the existing source of truth for `Start`, `PermissionRequest`, and `Stop` transitions.
  - `WorkspaceAttentionController::handle_terminal_input` currently allows `None -> Working`, which is the most direct false-yellow path.
  - `launch_workspace_preset_in_terminal` and `launch_workspace_preset_task` currently split preset lifecycle handling into two paths, only one of which tracks session completion.
  - `render_workspace_row` already computes row-local render state and is the correct seam for open-status gating.
  - `render_workspace_git_status_pill` already encapsulates the right-side git summary pill styling and should remain the git-changes renderer.
  - `workspace_for_entry_in_window` and `workspace_matches_entry` already express "does this stored workspace entry correspond to a workspace loaded in this window?"
- `crates/superzent_model/src/lib.rs`
  - `aggregate_workspace_attention_status` defines the persistent aggregation contract between live attention and `review_pending`.
  - `clear_transient_workspace_attention` already clears stale `Working` and `Permission` during store load, so persisted model state is not the main bug source.
  - `record_workspace_opened` updates active/open bookkeeping but only for the active entry; it is not sufficient as the source of truth for all currently loaded workspaces.
  - `update_session_status` updates session metadata but does not currently project completion into workspace attention state.
- `crates/workspace/src/multi_workspace.rs`
  - `MultiWorkspace::workspaces()` is the current-window inventory of loaded workspaces and is the correct backing collection for open-state derivation.

### Institutional Learnings

- No `docs/solutions/` corpus or critical patterns file exists in this repo, so there are no institutional solution docs to carry forward for this change.

### External References

- None. The repo already has strong local patterns for both attention-state aggregation and current-window workspace matching, so external research would add little value here.

## Key Technical Decisions

- Derive live `Working` only from confirmed live-attention context, not from raw terminal input alone: this directly addresses the startup false-yellow behavior while keeping `Start` and `PermissionRequest` hooks authoritative.
- Use `MultiWorkspace::workspaces()` plus `workspace_matches_entry` as the open-state source of truth: this answers the current-window question the user actually cares about and avoids confusing "active workspace" with "loaded workspace".
- Keep right-side row status rendering separate from attention rendering: the git/change/Open badge can change without altering the left attention dot, which keeps the two semantics independent.
- Treat preset completion as review-worthy even when the workspace is currently visible: recent user validation showed that "finished" should become green, not disappear back to idle.
- Unify terminal preset lifecycle handling for prompt and non-prompt launches: the same preset command should not have different completion semantics depending on whether it started with an initial prompt.

## Open Questions

### Resolved During Planning

- What is the lowest-risk source of truth for "workspace is loaded in the current window"?
  - Use `SuperzentSidebar`'s `multi_workspace` handle and the existing `workspace_matches_entry` / `workspace_for_entry_in_window` matching helpers. Do not use `SuperzentStore::active_workspace_id()` as the open-state source, because it only identifies the active workspace, not every workspace loaded in the current window.
- Which startup or terminal-input paths should be allowed to create `Working`?
  - `Start` and `PermissionRequest` hook events remain authoritative for bootstrapping live attention. Terminal input may preserve or refresh already-live attention, but it should not bootstrap `None -> Working` for a terminal that has not been confirmed live in the current app session.

### Deferred to Implementation

- Whether the new `Open` pill should reuse the existing git-pill container styling verbatim or extract a tiny shared row-status helper. This is a presentation refactor choice, not a planning blocker.
- Whether the safest implementation is to make `next_terminal_input_attention_status` stricter or to move the guard up into `handle_terminal_input`. The plan requires the semantic result, not one specific helper shape.

## High-Level Technical Design

> _This illustrates the intended approach and is directional guidance for review, not implementation specification. The implementing agent should treat it as context, not code to reproduce._

| Signal source                                    | Can bootstrap live attention?                 | Expected result                                                                          |
| ------------------------------------------------ | --------------------------------------------- | ---------------------------------------------------------------------------------------- |
| Persisted store state at startup                 | No                                            | Preserve `Review`; clear stale `Working` / `Permission` via existing model normalization |
| `AgentHookEventType::Start`                      | Yes                                           | Workspace enters `Working`                                                               |
| `AgentHookEventType::PermissionRequest`          | Yes                                           | Workspace enters `Permission`                                                            |
| `AgentHookEventType::Stop`                       | Yes                                           | Workspace leaves live attention and falls back to `Review` or `Idle`                     |
| Terminal input for already-tracked live terminal | No new bootstrap, but may preserve live state | Keep `Working`; never downgrade `Permission`                                             |
| Terminal input for untracked terminal            | No                                            | Leave workspace at `Idle` or `Review`                                                    |

For row presentation, compute two independent values in the sidebar render path:

- `attention_status`: existing attention model, rendered only through the left dot.
- `is_open_in_current_window`: derived from loaded workspaces in the current `MultiWorkspace`; used only to decide whether the right-side row status area renders `Open`, the git pill, or nothing.

## Implementation Units

- [x] **Unit 1: Tighten live attention bootstrapping**

**Goal:** Prevent startup or restored terminal input from marking a workspace as `Working` unless the workspace has confirmed live agent activity in the current app session.

**Requirements:** R1, R2, R3, R4

**Dependencies:** None

**Files:**

- Modify: `crates/superzent_ui/src/lib.rs`
- Test: `crates/superzent_ui/src/lib.rs`
- Reference: `crates/superzent_model/src/lib.rs`

**Approach:**

- Narrow `WorkspaceAttentionController::handle_terminal_input` so it no longer uses raw terminal input as an unconditional bootstrap to `Working`.
- Keep `handle_hook_event` as the authoritative entry point for creating `Working` and `Permission` live attention records.
- Preserve the current "don't downgrade permission" behavior while removing the `None -> Working` fallback that causes false startup yellow states.
- Reuse the existing `aggregate_workspace_attention_status` contract instead of adding another persisted attention state.

**Execution note:** Start with failing state-transition tests around terminal-input and startup behavior before changing the controller logic.

**Patterns to follow:**

- `WorkspaceAttentionController::handle_hook_event` in `crates/superzent_ui/src/lib.rs`
- `aggregate_workspace_attention_status` in `crates/superzent_model/src/lib.rs`
- `clear_transient_workspace_attention` in `crates/superzent_model/src/lib.rs`

**Test scenarios:**

- Happy path: after a `Start`-equivalent live state is present, terminal input keeps the workspace in `Working`.
- Edge case: terminal input for a terminal with no tracked live attention leaves the workspace at `Idle`.
- Edge case: terminal input for a workspace already in `Review` does not promote it back to `Working` on startup.
- Error path: terminal input while the workspace is in `Permission` does not clear or downgrade the red attention state.
- Integration: a workspace loaded from persisted state with no new live hook events does not show `Working` after the sidebar initializes.

**Verification:**

- A workspace only turns yellow after a live hook establishes active work in the current session, not merely because a restored terminal emits or receives input during startup.

- [x] **Unit 2: Derive current-window open state for sidebar rows**

**Goal:** Give each sidebar row an explicit `is_open_in_current_window` signal based on loaded workspaces in the current `MultiWorkspace`, independent of active selection.

**Requirements:** R5, R6, R7, R8

**Dependencies:** None

**Files:**

- Modify: `crates/superzent_ui/src/lib.rs`
- Test: `crates/superzent_ui/src/lib.rs`
- Reference: `crates/workspace/src/multi_workspace.rs`

**Approach:**

- Add a row-friendly helper in `SuperzentSidebar` or adjacent sidebar helpers that answers whether a given `WorkspaceEntry` is currently loaded in this window.
- Base that helper on `self.multi_workspace`, `MultiWorkspace::workspaces()`, and the existing `workspace_matches_entry` / `workspace_for_entry_in_window` matching logic.
- Keep `selected` tied to `active_workspace_id`, but stop using active-selection state as a proxy for open state.
- Use the helper only inside the current-window sidebar render path; avoid any fallback to "any window" helpers, because the product definition is explicitly current-window-only.

**Patterns to follow:**

- `workspace_for_entry_in_window` in `crates/superzent_ui/src/lib.rs`
- `matching_workspace_indexes` in `crates/superzent_ui/src/lib.rs`
- `MultiWorkspace::workspaces()` in `crates/workspace/src/multi_workspace.rs`

**Test scenarios:**

- Happy path: a stored workspace entry with a matching loaded workspace in the current `MultiWorkspace` reports `open`.
- Happy path: multiple loaded workspaces in the same window each report `open` regardless of which one is active.
- Edge case: the active workspace entry reports `closed` if it is no longer present in `MultiWorkspace::workspaces()`.
- Edge case: a closed workspace with matching persisted metadata but no live workspace match reports `closed`.
- Integration: changing the set of loaded workspaces triggers the sidebar to recompute open-state correctly when `WorkspaceAdded` or `WorkspaceRemoved` events fire.

**Verification:**

- Sidebar row rendering can reliably distinguish "loaded in this window" from "selected" and from stored-but-closed workspaces.

- [x] **Unit 3: Update row-status rendering to show `Open` or git changes only for open workspaces**

**Goal:** Restrict the right-side row status area to open workspaces and make it show a muted `Open` pill only when the workspace is open and has no git changes.

**Requirements:** R7, R8, R9, R10, R11

**Dependencies:** Unit 2

**Files:**

- Modify: `crates/superzent_ui/src/lib.rs`
- Test: `crates/superzent_ui/src/lib.rs`
- Optional visual verification: `crates/zed/src/visual_test_runner.rs`

**Approach:**

- Keep `render_workspace_git_status_pill` as the renderer for real git changes.
- Add a small open-status render branch that returns a muted `Open` pill only when the row is open and the workspace has no diff/sync/file-status summary to show.
- Ensure closed workspaces render no right-side status, even if the store has cached git metadata for them.
- Leave the left attention dot untouched by row-status rendering, so a workspace can be open-with-`Open`, open-with-git-pill, review-green, or permission-red without semantic overlap.

**Execution note:** Preserve the existing git pill styling conventions; the change should be semantic, not a visual redesign.

**Patterns to follow:**

- `render_workspace_git_status_pill` in `crates/superzent_ui/src/lib.rs`
- Existing row composition in `render_workspace_row` in `crates/superzent_ui/src/lib.rs`
- Existing small badge usage such as `Chip` in `crates/superzent_ui/src/lib.rs`

**Test scenarios:**

- Happy path: an open workspace with no git changes renders a muted `Open` pill on the right.
- Happy path: an open workspace with git changes renders the existing git status pill and does not render `Open`.
- Edge case: a closed workspace with cached git summary renders no right-side status.
- Edge case: an open workspace with unavailable git metadata still shows `Open` rather than an empty gap.
- Integration: an open workspace with no attention shows no left dot while still showing `Open` on the right.
- Integration: a workspace in `Review` or `Permission` keeps the correct left-dot color while the right side follows the open/gitsummary rules independently.

**Verification:**

- In the sidebar, users can distinguish open/no-change rows, open/changed rows, and closed rows without using the left attention dot as a proxy.

- [x] **Unit 4: Close preset completion lifecycle for terminal launches**

**Goal:** Make terminal preset launches with or without an initial task prompt share one tracked lifecycle, and ensure completion turns the workspace green instead of leaving it yellow or dropping straight back to idle.

**Requirements:** R2, R12, R13

**Dependencies:** Unit 1

**Files:**

- Modify: `crates/superzent_ui/src/lib.rs`
- Modify: `crates/superzent_model/src/lib.rs`
- Test: `crates/superzent_ui/src/lib.rs`

**Approach:**

- Remove the split between untracked `launch_workspace_preset_in_terminal` behavior and tracked `launch_workspace_preset_task` behavior so both prompt and non-prompt terminal preset launches create session state and observe terminal completion.
- Ensure completion paths project into workspace attention as `Review`/green rather than relying exclusively on visible/background heuristics from hook-driven stop events.
- Keep hook-driven `Permission` and popup behavior intact, but add completion-state fallback so preset launches still converge to the correct final attention state when hooks are late or absent.

**Patterns to follow:**

- `launch_workspace_preset_task` in `crates/superzent_ui/src/lib.rs`
- `WorkspaceAttentionController::handle_hook_event` in `crates/superzent_ui/src/lib.rs`
- `update_session_status` in `crates/superzent_model/src/lib.rs`

**Test scenarios:**

- Happy path: launching a terminal preset without an initial task prompt enters `Working` and then transitions to `Review` when the terminal task exits successfully.
- Happy path: launching a terminal preset with an initial task prompt follows the same completion semantics as the no-prompt path.
- Edge case: a visible workspace still turns green on completion instead of reverting directly to idle.
- Error path: a failed terminal preset run still exits `Working` and leaves the workspace in a completed-attention state rather than staying yellow forever.
- Integration: hook-driven completion and session-driven completion converge on the same final green state instead of racing to different colors.

**Verification:**

- Clicking a preset shows yellow while it is active and green when it finishes, regardless of whether the preset started with an initial prompt.

## System-Wide Impact

- **Interaction graph:** `TerminalView` input events feed `WorkspaceAttentionController`; `AgentHookEvent`s establish live attention; `SuperzentSidebar` renders both attention and row-status state from `SuperzentStore` plus live `MultiWorkspace` membership.
- **Error propagation:** This change should stay local to sidebar semantics. A matching failure in open-state derivation should at worst hide the `Open` pill or a git pill, not corrupt persisted workspace metadata.
- **State lifecycle risks:** The main lifecycle risk is mixing persisted state with current-session live state. The implementation must keep persisted `Review` behavior while ensuring startup-time restored terminals cannot resurrect `Working`.
- **Completion lifecycle risks:** Terminal presets currently have two different launch paths; the implementation must ensure both paths feed the same final attention transition so prompt/no-prompt launches cannot diverge.
- **API surface parity:** Any helper added for current-window open-state should be usable from both row rendering and existing "close workspace in this window" behaviors to avoid divergent matching logic.
- **Integration coverage:** Tests need to cover the interaction between persisted store state, live terminal hook events, and current-window workspace membership rather than only pure helper behavior.
- **Unchanged invariants:** `WorkspaceAttentionStatus` meanings stay the same, `Review` still survives restart when `review_pending` is true, and git summary computation remains unchanged for live workspaces.

## Risks & Dependencies

| Risk                                                                                       | Mitigation                                                                                                                                                                                                         |
| ------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Tightening terminal-input semantics delays yellow-state updates for reused agent terminals | Prefer hook-confirmed liveness as the semantic source of truth; if reused-terminal responsiveness still matters, allow input to preserve only already-tracked live attention rather than bootstrapping from `None` |
| Open-state helper drifts from other workspace-matching code                                | Reuse `workspace_matches_entry` / `workspace_for_entry_in_window` instead of inventing a new matching contract                                                                                                     |
| Closed workspaces unexpectedly lose visible git context that some internal code relied on  | Limit the behavior change to sidebar row rendering; keep stored git metadata and refresh logic intact                                                                                                              |

## Documentation / Operational Notes

- No user documentation update appears necessary unless the team maintains a dedicated visual reference for the Superzent workspace sidebar.
- If the repo relies on visual baselines for sidebar regressions, refresh or extend the existing multi-workspace sidebar visual test to capture the new `Open` badge and the absence of false-yellow attention on startup.

## Sources & References

- **Origin document:** `docs/brainstorms/2026-04-03-workspace-attention-and-open-state-requirements.md`
- Related code: `crates/superzent_ui/src/lib.rs`
- Related code: `crates/superzent_model/src/lib.rs`
- Related code: `crates/workspace/src/multi_workspace.rs`
- Related visual coverage: `crates/zed/src/visual_test_runner.rs`
