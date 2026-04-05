---
title: fix: Add macOS Dock recent folders setting
type: fix
status: completed
date: 2026-04-03
origin: docs/brainstorms/2026-04-03-macos-dock-recent-folders-setting-requirements.md
---

# fix: Add macOS Dock recent folders setting

## Overview

Add a JSON-only workspace setting that controls whether Superzent contributes opened local
workspaces to macOS Dock recent folders. When the effective setting is disabled, the app
must stop adding new entries and clear any existing app-level Dock recent documents on
startup and on enabled-to-disabled transitions. In-app Recent Projects remains unchanged
(see origin: docs/brainstorms/2026-04-03-macos-dock-recent-folders-setting-requirements.md).

## Problem Frame

Today the only local call site for Dock recent items is `crates/project/src/worktree_store.rs`,
which forwards visible worktree paths into `App::add_recent_document()`. On macOS that flows
into `NSDocumentController`, so right-clicking the Dock icon surfaces prior workspaces even
when the user does not want that history exposed. The product intent is to make this
Dock-specific behavior optional without changing the app's database-backed recent-project
surfaces.

## Requirements Trace

- R1. Add a user-facing `settings.json` setting to control Dock recent-folder contribution.
- R2. Default that setting to disabled on macOS.
- R3. Keep the setting out of Settings UI for this phase.
- R4. Limit behavior changes to macOS Dock recent folders; preserve app-internal Recent Projects.
- R5. Skip adding new visible local workspaces when disabled.
- R6. Clear existing Dock recent-folder entries on startup when disabled.
- R7. Clear existing Dock recent-folder entries when the setting changes from enabled to disabled.
- R8. Preserve current non-macOS behavior.
- R9. Preserve current macOS behavior when the setting is enabled.

## Scope Boundaries

- Do not add a Settings UI control in `crates/settings_ui`.
- Do not support project-local or worktree-local overrides; this setting is user-global only.
- Do not change `crates/recent_projects`, `crates/workspace/src/history_manager.rs`, or
  workspace DB persistence semantics.
- Do not redesign the Dock menu itself; this is about AppKit recent-document integration only.

## Context & Research

### Relevant Code and Patterns

- `crates/project/src/worktree_store.rs` is the only current local caller of
  `cx.add_recent_document(...)`, guarded by `visible`.
- `crates/gpui/src/app.rs` and `crates/gpui/src/platform.rs` already expose
  `add_recent_document`, so the cleanest place for the matching clear operation is the same
  abstraction layer.
- `crates/gpui_macos/src/platform.rs` implements the AppKit bridge via
  `NSDocumentController::noteNewRecentDocumentURL`.
- `crates/workspace/src/workspace_settings.rs` is the repo pattern for turning
  `settings_content` values into runtime `WorkspaceSettings`.
- `crates/settings/src/settings_store.rs` generates distinct user and project settings schemas;
  project settings are rooted in `ProjectSettingsContent`, so a new workspace setting should stay
  user-global unless the change explicitly broadens the project schema.
- `crates/zed/src/zed.rs` already contains global settings observers that track prior values,
  for example `handle_keymap_file_changes`, and is the natural place for a one-time startup
  check plus runtime transition handling.
- `crates/recent_projects/src/recent_projects.rs` and
  `crates/workspace/src/history_manager.rs` are separate, DB-backed recent-history surfaces and
  do not currently depend on the AppKit recent-document bridge.
- `crates/workspace/src/workspace.rs` and `crates/project/tests/integration/project_tests.rs`
  show the standard settings mutation test pattern through `SettingsStore::update_global`.
- `crates/gpui/src/platform/test/platform.rs` and `crates/gpui/src/app/test_context.rs`
  provide the existing hooks for observing platform side effects in tests.

### Institutional Learnings

- No `docs/solutions/` corpus exists in this repo today, so planning is grounded in current
  code patterns rather than prior solution docs.

### External References

- None. Local patterns are strong and the change is bounded to existing platform abstractions.

## Key Technical Decisions

- `show_dock_recent_folders` is the planned `settings.json` key.
  Rationale: it is user-facing, matches existing boolean naming such as `show_call_status_icon`,
  and describes the visible Dock behavior rather than the underlying AppKit API.
- Keep the schema field as a simple boolean and apply the macOS-only default in
  `WorkspaceSettings::from_settings`.
  Rationale: this avoids adding a new tri-state enum or Settings UI complexity for a JSON-only
  setting, while still allowing explicit user override on every platform.
- Keep the setting user-global only by defining it in `WorkspaceSettingsContent` and leaving
  `ProjectSettingsContent` unchanged.
  Rationale: Dock recent documents are application-level state, so project-local overrides would
  create cross-window inconsistency and do not map cleanly to AppKit's single recent-document
  list.
- Add a matching `clear_recent_documents` platform/app API next to `add_recent_document`.
  Rationale: the add/clear lifecycle belongs in GPUI's platform boundary, not in a
  `zed`-specific Objective-C call site.
- Treat the setting as an app-global decision and read it through
  `WorkspaceSettings::get_global(cx)` in both startup and worktree code paths.
  Rationale: macOS Dock recent documents are application-level state rather than per-project
  state, so a workspace-local override would create inconsistent behavior across windows.
- Run clear logic from `crates/zed/src/zed.rs` using the effective runtime setting and prior
  value tracking.
  Rationale: startup and settings transitions are application-level lifecycle concerns, while
  worktree creation stays focused on add/skip behavior.
- Keep non-macOS logic generic but make the platform clear implementation a no-op outside macOS.
  Rationale: this preserves production behavior on other platforms while keeping the code path
  testable with the shared test platform.

## Open Questions

### Resolved During Planning

- What `settings.json` key and placement should this use?
  Resolution: a top-level workspace setting named `show_dock_recent_folders`.
- Should this support project-local overrides?
  Resolution: no; it is user-global only and should not appear in project settings schema.
- Where should startup clearing happen?
  Resolution: a dedicated helper registered from `zed::init(cx)` that evaluates the effective
  setting once at startup and then observes `SettingsStore` for transitions.
- How should runtime enabled-to-disabled transitions be detected?
  Resolution: store the previous effective boolean in the observer closure and clear only on
  `true -> false` changes.

### Deferred to Implementation

- What exact Objective-C selector signature is needed for clearing AppKit recent documents?
  Why deferred: the plan chooses the API boundary and lifecycle; the exact `msg_send!` form is a
  small implementation detail to confirm while editing `crates/gpui_macos/src/platform.rs`.
- Whether the checked-in settings reference output needs regeneration in the same change.
  Why deferred: this depends on the repo's normal docs generation flow once the schema field is
  added.

## High-Level Technical Design

> _This illustrates the intended approach and is directional guidance for review, not implementation specification. The implementing agent should treat it as context, not code to reproduce._

| Trigger                          | Effective `show_dock_recent_folders` | Intended outcome                                      |
| -------------------------------- | ------------------------------------ | ----------------------------------------------------- |
| App startup                      | `false`                              | Clear app-level recent documents once                 |
| Settings reload `true -> false`  | `false`                              | Clear app-level recent documents immediately          |
| Settings reload `false -> false` | `false`                              | No duplicate work beyond the startup clear            |
| Settings reload `false -> true`  | `true`                               | Do not repopulate history retroactively               |
| Visible local worktree created   | `true`                               | Add the worktree path to recent documents             |
| Visible local worktree created   | `false`                              | Skip platform registration                            |
| Recent Projects UI queries       | any                                  | Continue reading workspace DB with no behavior change |

## Implementation Units

- [ ] **Unit 1: Add the setting contract and platform clear API**

**Goal:** Introduce the new workspace setting and the GPUI API surface needed to clear recent
documents in addition to adding them.

**Requirements:** R1, R2, R3, R6, R7, R8

**Dependencies:** None

**Files:**

- Modify: `crates/settings_content/src/workspace.rs`
- Modify: `crates/settings/src/settings_store.rs`
- Modify: `crates/workspace/src/workspace_settings.rs`
- Modify: `crates/gpui/src/platform.rs`
- Modify: `crates/gpui/src/app.rs`
- Modify: `crates/gpui_macos/src/platform.rs`
- Modify: `crates/gpui/src/platform/test/platform.rs`
- Modify: `crates/gpui/src/app/test_context.rs`
- Test: `crates/zed/src/zed.rs`
- Test: `crates/project/tests/integration/project_tests.rs`

**Approach:**

- Add `show_dock_recent_folders: Option<bool>` to `WorkspaceSettingsContent` with a doc comment
  that marks it as macOS-only and JSON-only for now.
- Keep the field out of `ProjectSettingsContent` so it is valid in user settings schema but not
  in project settings schema.
- Thread the field into `WorkspaceSettings`, using a platform-aware fallback in
  `WorkspaceSettings::from_settings` so unspecified settings resolve to `false` on macOS and
  `true` elsewhere.
- Extend the GPUI platform trait with a `clear_recent_documents` no-op default and expose it via
  `App`, mirroring the existing `add_recent_document` shape.
- Implement the macOS clear behavior through `NSDocumentController`; leave Linux/Windows on the
  default no-op path.
- Extend the test platform so tests can inspect the recorded recent-document paths and how many
  times the clear operation ran.

**Patterns to follow:**

- `crates/workspace/src/workspace_settings.rs` runtime mapping style for workspace settings
- `crates/settings/src/settings_store.rs` user-vs-project schema split
- `crates/gpui/src/app.rs` and `crates/gpui/src/platform.rs` pairing around
  `add_recent_document`
- `crates/gpui/src/app/test_context.rs` helper exposure pattern for platform-observable effects

**Test scenarios:**

- Happy path: with explicit `show_dock_recent_folders = true`, the effective runtime setting
  remains enabled and downstream callers can still record recent documents.
- Edge case: when the setting is omitted, the effective runtime value resolves to the platform
  default (`false` on macOS, `true` elsewhere).
- Integration: the user settings schema includes `show_dock_recent_folders` while the project
  settings schema omits it, preserving the user-global-only contract.
- Integration: calling the new clear API on the test platform empties the recorded recent
  documents and increments a clear counter that later tests can assert against.
- Error path: none expected; the platform abstraction remains best-effort and non-throwing, like
  the existing add path.

**Verification:**

- The settings schema exposes the new key for `settings.json`.
- The project settings schema does not expose the new key.
- Test infrastructure can observe both "recent document added" and "recent documents cleared"
  side effects.

- [ ] **Unit 2: Gate worktree additions with the new setting**

**Goal:** Prevent visible local workspaces from being registered with recent documents when the
setting is disabled, while preserving all current worktree creation behavior.

**Requirements:** R4, R5, R8, R9

**Dependencies:** Unit 1

**Files:**

- Modify: `crates/project/src/worktree_store.rs`
- Test: `crates/project/tests/integration/project_tests.rs`

**Approach:**

- At the existing `visible` call site, read `WorkspaceSettings::get_global(cx)` and gate
  `cx.add_recent_document(...)` on the new effective boolean.
- Keep the current `visible` requirement intact so invisible worktrees continue to skip recent
  document registration regardless of the setting.
- Do not touch worktree creation, project loading, or workspace DB persistence paths.
- Read the effective value from global user settings so gating stays consistent with the
  startup clear behavior in Unit 3.

**Execution note:** Start with integration coverage that proves the only changed behavior is the
platform recent-document side effect.

**Patterns to follow:**

- The existing `visible` guard around `cx.add_recent_document(...)` in
  `crates/project/src/worktree_store.rs`
- Settings mutation style in `crates/project/tests/integration/project_tests.rs`

**Test scenarios:**

- Happy path: creating a visible local worktree with `show_dock_recent_folders = true` records
  the worktree path in the test platform's recent-document list.
- Edge case: creating a visible local worktree with `show_dock_recent_folders = false` does not
  record any new recent-document path.
- Edge case: creating an invisible worktree does not record a recent-document path even when the
  setting is enabled.
- Integration: worktree creation still returns successfully in all three scenarios above, proving
  the setting only gates the platform side effect.

**Verification:**

- The only observable behavior change is whether the recent-document recorder changes; worktree
  creation and project state remain intact.

- [ ] **Unit 3: Clear existing Dock recent documents at startup and on disable transitions**

**Goal:** Enforce the "disabled means hidden now" behavior by clearing AppKit recent documents at
startup and on enabled-to-disabled settings changes.

**Requirements:** R4, R6, R7, R8

**Dependencies:** Unit 1

**Files:**

- Modify: `crates/zed/src/zed.rs`
- Test: `crates/zed/src/zed.rs`

**Approach:**

- Add a small helper invoked from `zed::init(cx)` that:
  - reads the effective `WorkspaceSettings::get_global(cx).show_dock_recent_folders` value once
    during init and clears recent documents immediately when it is `false`
  - registers a `SettingsStore` observer that tracks the previous effective value and clears only
    on `true -> false` transitions
- Keep the helper separate from keymap and window lifecycle observers so its purpose remains
  narrowly scoped and testable.
- Do not backfill recent documents when the setting becomes enabled again; new worktree openings
  repopulate the list naturally through Unit 2.
- Leave `crates/recent_projects` and `crates/workspace/src/history_manager.rs` untouched so
  app-internal Recent Projects remains DB-backed and unchanged.

**Patterns to follow:**

- Prior-value observer logic in `crates/zed/src/zed.rs` (`handle_keymap_file_changes`)
- Existing settings observer registration style in `crates/zed/src/zed.rs`
- Existing `init_test(cx); cx.update(init);` test setup pattern in `crates/zed/src/zed.rs`

**Test scenarios:**

- Happy path: when user settings explicitly set `show_dock_recent_folders = false` before
  `zed::init`, startup clears the test platform's recent documents exactly once.
- Edge case: when the setting is omitted, startup still clears recent documents on macOS because
  the effective default resolves to disabled there.
- Happy path: when user settings explicitly set `show_dock_recent_folders = true` before
  `zed::init`, startup does not clear recent documents.
- Edge case: after init with the setting enabled, changing settings to `false` clears recent
  documents exactly once.
- Edge case: changing settings from `false -> false` or `true -> true` does not perform an extra
  clear.
- Integration: after changing settings from `false -> true`, a newly opened visible worktree is
  recorded again, proving the plan preserves forward behavior without retroactive backfill.
- Integration: after a clear, creating a visible worktree while the setting remains disabled does
  not repopulate recent documents, proving the startup/transition logic composes correctly with
  Unit 2.

**Verification:**

- Startup and runtime-disable flows both converge on the same clear side effect.
- No code path touches the DB-backed recent-project surfaces.

## System-Wide Impact

- **Interaction graph:** `WorkspaceSettings` now influences two paths: the existing
  `WorktreeStore -> App::add_recent_document` path and a new `zed::init -> App::clear_recent_documents`
  lifecycle path.
- **Error propagation:** recent-document add/clear remains best-effort platform behavior with no
  new user-visible error channel, matching the current `add_recent_document` contract.
- **State lifecycle risks:** repeated settings reloads can trigger duplicate clears unless the
  observer tracks prior state; tests should lock this down.
- **API surface parity:** Windows jump-list behavior via `update_jump_list` must remain unchanged.
- **Integration coverage:** the important cross-layer scenario is startup clear + later worktree
  creation under a disabled setting.
- **Unchanged invariants:** `crates/recent_projects/src/recent_projects.rs` and
  `crates/workspace/src/history_manager.rs` remain the source of truth for app-internal recent
  surfaces and should not be modified by this plan.

## Risks & Dependencies

| Risk                                                                                           | Mitigation                                                                                                                                                        |
| ---------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `clear_recent_documents` is app-wide and would clear any future recent-document call sites too | Keep the API addition tightly documented, note the current single caller in code comments or tests, and avoid broadening recent-document usage in the same change |
| Startup/settings observers could clear repeatedly on every settings reload                     | Track the previous effective boolean and assert idempotent behavior in tests                                                                                      |
| The new setting might accidentally leak into Settings UI                                       | Limit changes to `settings_content` and `workspace_settings`; do not add a `settings_ui` page-data entry in this phase                                            |

## Documentation / Operational Notes

- The new setting should be documented through the settings schema comments in
  `crates/settings_content/src/workspace.rs`.
- No rollout flag or migration is needed; the macOS default change takes effect immediately for
  users who do not override the new setting.
- If the repo expects checked-in generated settings reference output, regenerate it as part of the
  implementation change.

## Sources & References

- **Origin document:** `docs/brainstorms/2026-04-03-macos-dock-recent-folders-setting-requirements.md`
- Related code: `crates/project/src/worktree_store.rs`
- Related code: `crates/gpui/src/app.rs`
- Related code: `crates/gpui/src/platform.rs`
- Related code: `crates/gpui_macos/src/platform.rs`
- Related code: `crates/settings/src/settings_store.rs`
- Related code: `crates/recent_projects/src/recent_projects.rs`
- Related code: `crates/workspace/src/history_manager.rs`
- Related code: `crates/workspace/src/workspace_settings.rs`
- Related code: `crates/zed/src/zed.rs`
