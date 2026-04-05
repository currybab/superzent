---
date: 2026-04-03
topic: managed-terminal-notifications-always-mode
---

# Managed Terminal Notifications In Always Mode

## Problem Frame

Managed terminal agent sessions are expected to raise popup notifications when they need input or when they finish, but the current behavior is unreliable enough that users do not trust the notification system. This is especially confusing when `terminal.agent_notifications` is set to `always`, because the setting name implies that popup notifications should appear regardless of whether the relevant workspace or pane is currently visible.

## Requirements

| Mode               | Permission popup  | Completion popup  | Scope                                           |
| ------------------ | ----------------- | ----------------- | ----------------------------------------------- |
| `always`           | Always show       | Always show       | Even when the current workspace/pane is visible |
| `app_background`   | Existing behavior | Existing behavior | Preserve current semantics                      |
| `workspace_hidden` | Existing behavior | Existing behavior | Preserve current semantics                      |

**Scope**

- R1. This work applies only to managed terminal agent sessions.
- R2. This work is limited to popup notifications. Sound behavior and workspace/sidebar attention indicators are out of scope for this topic.
- R3. This work is scoped to macOS behavior only.
- R4. Unmanaged terminals, including users directly launching `codex` or `claude` outside the managed terminal flow, are explicitly out of scope for this change.

**Always Mode Behavior**

- R5. When `terminal.agent_notifications = always`, a managed terminal session that reaches a permission-request state must show a popup notification.
- R6. When `terminal.agent_notifications = always`, a managed terminal session that completes must show a popup notification.
- R7. In `always` mode, the popup notification must still appear even if the target workspace is the active workspace and the target pane is currently visible.

**Mode Preservation**

- R8. `app_background` mode must preserve its current meaning and continue to gate notifications based on whether the app has an active window.
- R9. `workspace_hidden` mode must preserve its current meaning and continue to gate notifications based on whether the target workspace is currently visible.
- R10. This change must not broaden `app_background` or `workspace_hidden` behavior just because `always` mode becomes less suppressive.

## Success Criteria

- With `terminal.agent_notifications = always`, a managed terminal permission request reliably creates a popup notification on macOS.
- With `terminal.agent_notifications = always`, a managed terminal completion reliably creates a popup notification on macOS.
- Users still see the existing gated behavior for `app_background` and `workspace_hidden`.
- The repo's settings copy and implementation semantics no longer disagree about what `always` means.

## Scope Boundaries

- Do not expand support to unmanaged terminals in this work.
- Do not redesign notification sounds, notification wording, or action-button UX unless required to preserve existing behavior.
- Do not change non-macOS notification semantics in this work.

## Key Decisions

- Limit the scope to managed terminals first: the codebase and settings text already describe managed terminal sessions as the supported surface, so fixing that path is the highest-leverage move.
- Treat `always` literally: if the popup still suppresses when the current pane is visible, the setting name is misleading and user expectations are violated.
- Preserve other modes: `app_background` and `workspace_hidden` already encode narrower policies and should not be redefined as part of fixing `always`.

## Dependencies / Assumptions

- The current project already has a managed terminal concept and a popup notification path for terminal lifecycle events.
- The current settings surface already exposes `terminal.agent_notifications`, so this work is expected to align behavior with the setting rather than inventing a new product control.

## Outstanding Questions

### Deferred to Planning

- [Affects R1][Technical] What is the exact source of truth for “managed terminal session” in the existing notification pipeline?
- [Affects R5][Needs research] Which part of the pipeline currently prevents permission and completion popup notifications from appearing in `always` mode: missing lifecycle events, event mapping, build gating, or popup suppression logic?
- [Affects R7][Technical] What is the narrowest implementation change that makes `always` bypass visibility suppression without changing the other notification modes?

## Next Steps

→ /prompts:ce-plan for structured implementation planning
