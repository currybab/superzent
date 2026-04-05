---
title: Fix managed terminal notifications in always mode
type: fix
status: completed
date: 2026-04-03
origin: docs/brainstorms/2026-04-03-managed-terminal-notifications-always-mode-requirements.md
---

# Fix managed terminal notifications in always mode

## Overview

Make macOS popup notifications for managed terminal agent sessions actually honor `terminal.agent_notifications = always`. The fix should ensure managed permission and completion events reliably produce popups even when the current workspace and pane are visible, while preserving the existing gating semantics for `app_background` and `workspace_hidden`.

## Problem Frame

The product already exposes a setting whose wording promises macOS notifications for managed Codex and Claude terminal sessions, but current behavior is unreliable enough that users do not trust it. Based on local code inspection, the notification path spans four layers: managed terminal launch tagging, hook-event ingestion, notification policy evaluation, and popup window creation. The user has explicitly narrowed the scope to managed terminals on macOS and wants `always` to mean literal always for permission/completion popups (see origin: `docs/brainstorms/2026-04-03-managed-terminal-notifications-always-mode-requirements.md`).

## Requirements Trace

- R1-R4. Scope the fix to managed terminal sessions, popup notifications only, macOS only, and exclude unmanaged terminals.
- R5-R7. In `always` mode, permission and completion popups must appear even when the target workspace and pane are currently visible.
- R8-R10. Preserve `app_background` and `workspace_hidden` semantics exactly as they exist today.

## Scope Boundaries

- Do not broaden support to unmanaged terminals in this work.
- Do not redesign notification sounds, wording, or popup interaction UX unless required to preserve the existing popup path.
- Do not change non-macOS behavior or re-specify other notification modes.

## Context & Research

### Relevant Code and Patterns

- `crates/settings_content/src/terminal.rs`
  - `TerminalAgentNotificationMode` already defines `Off`, `AppBackground`, `WorkspaceHidden`, and `Always`.
- `crates/settings_ui/src/page_data.rs`
  - Settings copy explicitly says "macOS notifications for managed Codex and Claude terminal sessions," which is the product contract this fix must honor.
- `crates/superzent_agent/src/runtime.rs`
  - Managed terminal sessions are identified by injected environment variables and wrapper-installed hook runtime (`AGENT_TERMINAL_ID_ENV_VAR`, `AGENT_WORKSPACE_ID_ENV_VAR`, and `spawn_for_workspace` / `prepare_workspace_launch`).
- `crates/superzent_ui/src/lib.rs`
  - `WorkspaceAttentionController::handle_hook_event` is the lifecycle entry point for `PermissionRequest` and `Completed`.
  - `maybe_show_terminal_notification` and `should_show_terminal_notification` are the current policy gates.
  - `show_popup_notification` is feature-gated on `acp_tabs`; the non-`acp_tabs` branch currently logs and drops popup notifications entirely.
- `crates/zed/Cargo.toml`
  - The app's default feature set includes `acp_tabs`, so popup support is expected in the default product build.

### Institutional Learnings

- No `docs/solutions/` corpus exists in this repo, so there are no prior solution docs to carry forward for this bug.

### External References

- None. The needed signals and policy contract are already present in the local codebase and settings surface.

## Key Technical Decisions

- Treat the settings copy as the product contract: if `always` still suppresses visible-pane notifications, the bug is in implementation, not in the requirements.
- Keep managed-session detection tied to existing injected terminal environment and wrapper/hook runtime rather than inventing a second managed-session concept.
- Change the narrowest gate possible: `always` should bypass visibility suppression, but `app_background` and `workspace_hidden` must continue using their existing policy checks unchanged.
- Preserve the current popup implementation path instead of inventing a separate notification surface for this fix.

## Open Questions

### Resolved During Planning

- What is the source of truth for "managed terminal session"?
  - A managed session is one launched through the existing managed terminal runtime that injects `SUPERZENT_TERMINAL_ID` and `SUPERZENT_WORKSPACE_ID` and participates in the hook runtime in `crates/superzent_agent/src/runtime.rs`.
- Is `acp_tabs` likely the intended popup surface?
  - Yes. `show_popup_notification` is only fully implemented behind `acp_tabs`, and `crates/zed/Cargo.toml` enables `acp_tabs` by default for the application build.

### Deferred to Implementation

- Which exact code path currently drops the event before popup creation for the user's repro: missing hook emission, hook parsing, event-to-workspace resolution, or visibility suppression? This must be confirmed during implementation by tracing the actual managed-session lifecycle.
- Whether the cleanest implementation is to special-case `Always` in `should_show_terminal_notification` only, or to pass an explicit "bypass visible-pane suppression" signal deeper into the popup path. The semantic result is fixed; the helper shape is implementation detail.

## High-Level Technical Design

> _This illustrates the intended approach and is directional guidance for review, not implementation specification. The implementing agent should treat it as context, not code to reproduce._

| Stage                   | Current role                                      | Planned guarantee                                                 |
| ----------------------- | ------------------------------------------------- | ----------------------------------------------------------------- |
| Managed terminal launch | Tags sessions with injected env and wrappers      | Remains the only supported scope for this fix                     |
| Hook lifecycle ingress  | Produces `PermissionRequest` / `Completed` events | Must remain sufficient to drive popup decisions                   |
| Notification policy     | Decides whether to show a popup                   | `Always` bypasses visible-pane suppression; other modes unchanged |
| Popup creation          | Creates macOS popup window                        | Continues using the existing popup implementation path            |

## Implementation Units

- [x] **Unit 1: Verify and preserve managed-session lifecycle inputs**

**Goal:** Ensure the notification pipeline continues to rely on the existing managed terminal session signals and does not accidentally broaden to unmanaged terminals.

**Requirements:** R1, R4, R5, R6

**Dependencies:** None

**Files:**

- Modify: `crates/superzent_agent/src/runtime.rs`
- Modify: `crates/superzent_ui/src/lib.rs`
- Test: `crates/superzent_ui/src/lib.rs`

**Approach:**

- Confirm the exact managed-session markers the popup pipeline depends on and preserve them as the only scope for this fix.
- If the current event resolution can silently drop managed events before policy evaluation, tighten that mapping rather than broadening to unmanaged sessions.
- Keep the fix aligned with the existing wrapper/runtime path instead of creating parallel ad-hoc detection logic.

**Patterns to follow:**

- `AGENT_TERMINAL_ID_ENV_VAR` / `AGENT_WORKSPACE_ID_ENV_VAR` handling in `crates/superzent_agent/src/runtime.rs`
- `TerminalView` observation path in `crates/superzent_ui/src/lib.rs`

**Test scenarios:**

- Happy path: a managed terminal session can be identified from the existing injected runtime markers.
- Edge case: a non-managed terminal does not accidentally qualify for popup notification support.
- Integration: the managed-session signal survives long enough to reach the notification policy path.

**Verification:**

- The popup fix remains explicitly scoped to managed sessions and does not accidentally widen product scope.

- [x] **Unit 2: Make `always` bypass visible-pane suppression**

**Goal:** Ensure `terminal.agent_notifications = always` truly always shows managed permission and completion popups on macOS.

**Requirements:** R5, R6, R7

**Dependencies:** Unit 1

**Files:**

- Modify: `crates/superzent_ui/src/lib.rs`
- Test: `crates/superzent_ui/src/lib.rs`

**Approach:**

- Narrow the policy change to the `Always` branch in the terminal notification gate.
- Keep `app_background` and `workspace_hidden` behavior unchanged.
- Do not route the change through unrelated attention-state or sound-setting logic.

**Execution note:** Start with a failing policy-level test that demonstrates visible-pane suppression is still happening in `always` mode.

**Patterns to follow:**

- `should_show_terminal_notification` in `crates/superzent_ui/src/lib.rs`
- Existing `TerminalAgentNotificationMode` enum contract in `crates/settings_content/src/terminal.rs`

**Test scenarios:**

- Happy path: `Always` shows a permission popup even when the current workspace and pane are visible.
- Happy path: `Always` shows a completion popup even when the current workspace and pane are visible.
- Edge case: `WorkspaceHidden` continues to suppress when the target workspace is visible.
- Edge case: `AppBackground` continues to depend on active-window state rather than pane visibility.
- Integration: switching from `WorkspaceHidden` to `Always` changes popup behavior without changing event ingestion.

**Verification:**

- The popup policy now matches the settings label for `always`, and only `always`.

- [x] **Unit 3: Close any popup-path gaps after policy evaluation**

**Goal:** Ensure that once a managed permission/completion event passes policy, the popup path reliably produces a macOS popup in the default app build.

**Requirements:** R2, R3, R5, R6, R8, R9

**Dependencies:** Unit 2

**Files:**

- Modify: `crates/superzent_ui/src/lib.rs`
- Modify: `crates/zed/Cargo.toml` (only if feature wiring is inconsistent with intended product behavior)
- Test: `crates/superzent_ui/src/lib.rs`

**Approach:**

- Verify the default build still routes through the `acp_tabs` popup implementation path and that events reaching `maybe_show_terminal_notification` are not dropped before popup creation.
- If the failure is due to feature wiring or popup-window setup rather than policy, fix the narrowest point in the existing path.
- Preserve current popup copy and action button behavior unless a minimal change is required for correctness.

**Patterns to follow:**

- `show_popup_notification` in `crates/superzent_ui/src/lib.rs`
- Default feature wiring in `crates/zed/Cargo.toml`

**Test scenarios:**

- Happy path: a permission event that passes policy creates a popup object/window.
- Happy path: a completion event that passes policy creates a popup object/window.
- Edge case: the non-`acp_tabs` fallback continues to be explicitly unsupported rather than failing silently in the default build.
- Integration: managed permission and completion events both traverse policy and popup creation in one consistent path.

**Verification:**

- In the default macOS build path, events that should notify actually reach popup creation instead of being silently dropped.

## System-Wide Impact

- **Interaction graph:** managed terminal runtime -> hook lifecycle events -> notification policy -> popup creation.
- **Error propagation:** failures in event mapping or popup creation should stay visible via logging rather than fail silently.
- **State lifecycle risks:** this bug spans multiple stages of the lifecycle, so implementation must avoid "fixing" policy while leaving lifecycle ingress broken.
- **API surface parity:** `Always` should gain broader popup behavior without altering the contracts of `AppBackground` or `WorkspaceHidden`.
- **Integration coverage:** unit tests alone are insufficient; at least one test per critical path should exercise the event-to-policy-to-popup chain.
- **Unchanged invariants:** unmanaged terminals remain unsupported, sound settings remain separate, and non-macOS semantics stay unchanged.

## Risks & Dependencies

| Risk                                                                       | Mitigation                                                                                                                 |
| -------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------- |
| The real bug is missing lifecycle events, not suppression logic            | Keep Unit 1 explicit and require tracing the managed-session event path before calling the work done                       |
| Fixing `Always` accidentally broadens `WorkspaceHidden` or `AppBackground` | Write policy-preservation tests for the existing gated modes before or alongside the `Always` change                       |
| Popup support is silently feature-gated in some builds                     | Verify default feature wiring and call out unsupported fallback paths explicitly rather than assuming popup support exists |

## Documentation / Operational Notes

- If the final implementation changes the effective meaning of `always` from current behavior, the settings description should already be correct and may not need wording changes. Re-check after implementation.
- If the final fix depends on feature wiring assumptions, note that in the PR description so reviewers can validate the build path.

## Sources & References

- **Origin document:** `docs/brainstorms/2026-04-03-managed-terminal-notifications-always-mode-requirements.md`
- Related code: `crates/superzent_ui/src/lib.rs`
- Related code: `crates/superzent_agent/src/runtime.rs`
- Related code: `crates/settings_content/src/terminal.rs`
- Related code: `crates/settings_ui/src/page_data.rs`
- Related code: `crates/zed/Cargo.toml`
