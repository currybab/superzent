---
date: 2026-04-03
topic: workspace-attention-and-open-state
---

# Workspace Attention And Open State

## Problem Frame

Workspace sidebar row left-side dot is currently being read as both "this workspace is open" and "this workspace needs attention". That makes startup behavior confusing, because an open workspace can appear yellow even when no agent is actively working. The sidebar needs separate semantics for attention and open state so users can tell whether a workspace is merely loaded in the current window, actively working, waiting for review, or blocked on permission.

## Requirements

| Situation                                                            | Left attention dot | Right-side row status         |
| -------------------------------------------------------------------- | ------------------ | ----------------------------- |
| Closed workspace, no attention                                       | Hidden             | Hidden                        |
| Open workspace, no changes, no attention                             | Hidden             | Muted `Open` pill             |
| Open workspace, has changes, no attention                            | Hidden             | Git change pill               |
| Any workspace with completed agent response awaiting acknowledgement | Green              | Open-state rule still applies |
| Any workspace with live work in progress                             | Yellow             | Open-state rule still applies |
| Any workspace waiting on permission or urgent action                 | Red                | Open-state rule still applies |

**Attention Semantics**

- R1. The left color dot must represent workspace attention only. It must not be used to indicate that a workspace is open.
- R2. The attention states must map as follows: `Review -> green`, `Working -> yellow`, `Permission/Urgent attention -> red`, `Idle -> hidden`.
- R3. On app startup, a workspace must not show `Working` unless there is live agent activity for that workspace in the current session.
- R4. A previously open or previously active workspace must not restore as `Working` only because it was loaded before restart.
- R12. Completing a preset-launched agent run must transition the workspace from `Working` to `Review`/green even when the workspace is currently visible in the current window.
- R13. Terminal preset launches with and without an initial task prompt must use the same completion semantics.

**Open-State Semantics**

- R5. "Open" means the workspace is currently loaded in the current window.
- R6. Because the product no longer uses multiple windows for this workflow, open-state detection only needs to consider workspaces loaded in the current window.
- R7. Open state must be communicated separately from the attention dot.

**Row Status Presentation**

- R8. The right-side row status area should only be shown for open workspaces.
- R9. An open workspace with no git changes should show a muted `Open` pill.
- R10. An open workspace with git changes should show the git change pill instead of the `Open` pill.
- R11. A closed workspace should not show the right-side row status area, even if it has stored git metadata.

## Success Criteria

- On startup, a workspace that is merely open but not actively running an agent no longer appears yellow.
- A preset-launched workspace turns green when the agent run completes instead of remaining yellow.
- Users can tell, at a glance, whether a workspace is open independently from whether it needs attention.
- Users can distinguish `review complete`, `actively working`, and `permission needed` from the left-side indicator without using the right-side row status.
- In the workspace sidebar, open workspaces with no changes show `Open`, while open workspaces with changes show the change pill instead.

## Scope Boundaries

- This change applies to the workspace sidebar row behavior, not to unrelated title bar or panel notification badges.
- This change defines product behavior and semantic meaning, not the final visual styling details of the `Open` pill.
- This change does not introduce multi-window open-state logic.

## Key Decisions

- Separate attention from open-state: the same visual channel should not answer both "is it open?" and "does it need attention?" because that is what caused the startup confusion.
- Use the right-side row status area for open-state: this keeps the left dot reserved for urgency and workflow state.
- Hide right-side status for closed workspaces: this prevents closed workspaces from looking currently active.

## Dependencies / Assumptions

- The current product model treats open-state as "loaded in the current window".
- Git change summaries may still exist for closed workspaces, but they should not be surfaced in the row status area while closed.

## Outstanding Questions

### Deferred to Planning

- [Affects R7][Technical] What is the lowest-risk source of truth for "workspace is loaded in the current window" in the sidebar render path?
- [Affects R3][Needs research] Which existing startup or terminal-input paths currently mark a workspace as `Working`, and which of them should be ignored until live activity is confirmed?

## Next Steps

→ /prompts:ce-plan for structured implementation planning
