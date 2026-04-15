---
title: "feat: Add preset selection and auto-launch toggle to workspace creation"
type: feat
status: active
date: 2026-04-15
origin: docs/brainstorms/2026-04-15-workspace-creation-preset-control-requirements.md
---

# feat: Add preset selection and auto-launch toggle to workspace creation

## Overview

Add a preset dropdown and an auto-launch toggle to `NewWorkspaceModal` so users can choose which agent preset to assign and whether it should automatically start after workspace creation. Currently, the default preset always auto-launches — this change makes that behavior opt-in, defaulting to OFF.

## Problem Frame

When creating a managed workspace, `spawn_new_workspace_request` unconditionally fetches the default preset and launches it after workspace creation succeeds. There is no UI in `NewWorkspaceModal` to select a different preset or to suppress auto-launch. Users experience unintended agent sessions starting every time they create a workspace (see origin: `docs/brainstorms/2026-04-15-workspace-creation-preset-control-requirements.md`).

## Requirements Trace

- R1. Preset selector dropdown in the creation modal, defaulting to `store.default_preset()`, showing all available presets.
- R2. Auto-launch toggle, default OFF. When OFF, the preset is assigned but not launched. When ON, the current `launch_workspace_preset` flow runs.
- R3. The user's preset choice and auto-launch preference flow from `confirm()` through `spawn_new_workspace_request` to the `should_launch_preset` evaluation.

## Scope Boundaries

- Do not change auto-launch behavior for existing workspaces opened from the sidebar.
- Do not add a global/persistent setting for auto-launch default preference.
- Do not change the preset selector in the status bar / pane header area.

## Context & Research

### Relevant Code and Patterns

- `crates/superzent_ui/src/lib.rs`
  - `NewWorkspaceModal` struct (line ~2792): currently has `branch_name_editor`, `base_branch_editor`, `setup_script_editor`, `teardown_script_editor`, `show_more_options`, `save_lifecycle_defaults`.
  - `NewWorkspaceModal::confirm()` (line ~3048): calls `spawn_new_workspace_request` with fixed params — does not pass preset ID or launch preference.
  - `spawn_new_workspace_request` (line ~2294): hardcodes `let preset_id = store.read(cx).default_preset().id.clone()` at line ~2307.
  - `should_launch_preset` logic (line ~2529): `setup_result.as_ref().is_none_or(|result| result.is_ok())` — unconditionally launches on success.
  - `launch_workspace_preset` (line ~1209): the actual launch function, dispatches based on `PresetLaunchMode::Terminal` or `PresetLaunchMode::Acp`.
  - Existing checkbox pattern in the modal (line ~3193): `Checkbox::new(...)` with `.label()`, `.fill()`, `.elevation()`, `.label_size()`, `.on_click(cx.listener(...))`.
  - Existing preset dropdown pattern (line ~1036): `render_hidden_preset_dropdown` uses `ContextMenu::build` with entries per preset, wrapped in `DropdownMenu::new`.
- `crates/superzent_model/src/lib.rs`
  - `AgentPreset` struct (line ~78): `id`, `label`, `launch_mode`, `command`, `args`, `env`, `acp_agent_name`, `attention_patterns`.
  - `SuperzentStore::presets()` (line ~640): returns `&[AgentPreset]`.
  - `SuperzentStore::default_preset()` (line ~644): returns first preset.

### Institutional Learnings

- From `2026-04-06` lifecycle config plan: keep UI presentation and lifecycle execution as separate contracts rather than blending them into one opaque creation path. This aligns with passing the user's choice explicitly rather than embedding launch logic in the creation flow.

## Key Technical Decisions

- **Add fields to `NewWorkspaceModal`, not new parameters to `CreateWorkspaceOptions`:** The preset selection and auto-launch toggle are UI-layer concerns. `superzent_git::CreateWorkspaceOptions` should remain unaware of agent presets.

  - Rationale: preset launch is already handled after worktree creation in the UI layer. Keeping it there maintains the existing separation.

- **Add `preset_id` and `auto_launch` parameters to `spawn_new_workspace_request`:** Instead of the function reading `store.default_preset()` internally, the caller passes the selected preset ID and launch preference.

  - Rationale: makes the function's behavior explicit and testable. The modal's `confirm()` already collects all other user inputs and passes them through.

- **Modify `should_launch_preset` to incorporate the user's choice:** The existing condition (`setup_result.is_none_or(Ok)`) remains as a prerequisite, AND-ed with the new `auto_launch` flag.

  - Rationale: even with auto-launch ON, a failed setup should still suppress launch — preserving the existing safety behavior.

- **Use `DropdownMenu` for preset selection, `Checkbox` for auto-launch:** Both patterns already exist in the modal and adjacent UI. No new component types needed.

## Open Questions

### Resolved During Planning

- **Where to place the new UI elements?** After the base-branch field, before the "More Options" expandable section. This matches the natural "what workspace will look like" → "what happens after creation" flow.
- **Should auto-launch toggle be inside "More Options"?** No — it should be always visible since it controls a behavior users frequently don't want. Hiding it defeats the purpose.

### Deferred to Implementation

- Exact styling details (spacing, label text) will be refined during implementation to match the existing modal visual rhythm.

## Implementation Units

- [ ] **Unit 1: Add preset and auto-launch fields to NewWorkspaceModal**

  **Goal:** Extend `NewWorkspaceModal` with state for tracking the selected preset and auto-launch preference.

  **Requirements:** R1, R2

  **Dependencies:** None

  **Files:**

  - Modify: `crates/superzent_ui/src/lib.rs` — `NewWorkspaceModal` struct, its constructor (`new` / initialization site)

  **Approach:**

  - Add `selected_preset_id: String` field, initialized from `store.default_preset().id` when the modal is created.
  - Add `auto_launch: bool` field, initialized to `false`.
  - Read the store to get the preset list in the constructor for initializing the default.

  **Patterns to follow:**

  - Existing field initialization pattern in `NewWorkspaceModal::new()` for `save_lifecycle_defaults`, `show_more_options`.

  **Test expectation:** none — pure state scaffolding with no behavioral change.

  **Verification:**

  - Modal compiles and opens with the new fields initialized to their defaults.

- [ ] **Unit 2: Render preset dropdown and auto-launch toggle in the modal**

  **Goal:** Add the preset selector and auto-launch checkbox to the modal's `Render` implementation.

  **Requirements:** R1, R2

  **Dependencies:** Unit 1

  **Files:**

  - Modify: `crates/superzent_ui/src/lib.rs` — `Render for NewWorkspaceModal`

  **Approach:**

  - After the base-branch editor section and before the "More Options" toggle, add:
    1. A "Preset" label + `DropdownMenu` showing all presets from the store. Selecting a preset updates `self.selected_preset_id` and calls `cx.notify()`.
    2. A `Checkbox` for "Auto-launch after creation" bound to `self.auto_launch`, using the same pattern as the existing "Save as repo default" checkboxes.
  - The dropdown should display the currently selected preset's label as trigger text.
  - Use `ContextMenu::build` with entries per preset, following the `render_hidden_preset_dropdown` pattern.

  **Patterns to follow:**

  - `Checkbox::new(...)` pattern at line ~3193 for the toggle.
  - `render_hidden_preset_dropdown` at line ~1036 for the preset menu structure.
  - `DropdownMenu::new_with_element` for the dropdown trigger.

  **Test scenarios:**

  - Happy path: modal renders with preset dropdown showing the default preset label and auto-launch checkbox unchecked.
  - Happy path: clicking a different preset in the dropdown updates the displayed label.
  - Happy path: toggling the auto-launch checkbox updates its visual state.

  **Verification:**

  - Modal visually shows the preset dropdown and auto-launch toggle in the correct position.
  - Interacting with both controls updates the modal's internal state.

- [ ] **Unit 3: Pass preset selection and auto-launch through the creation flow**

  **Goal:** Wire the user's choices from `confirm()` through `spawn_new_workspace_request` to control preset assignment and launch behavior.

  **Requirements:** R1, R2, R3

  **Dependencies:** Unit 1, Unit 2

  **Files:**

  - Modify: `crates/superzent_ui/src/lib.rs` — `NewWorkspaceModal::confirm()`, `spawn_new_workspace_request`, and the `should_launch_preset` logic block

  **Approach:**

  - In `confirm()`: pass `self.selected_preset_id.clone()` and `self.auto_launch` to `spawn_new_workspace_request`.
  - Add `preset_id: String` and `auto_launch: bool` parameters to `spawn_new_workspace_request`.
  - Remove the internal `let preset_id = store.read(cx).default_preset().id.clone()` line — use the passed `preset_id` instead.
  - Modify the `should_launch_preset` condition from:
    ```
    setup_result.as_ref().is_none_or(|result| result.is_ok())
    ```
    to:
    ```
    auto_launch && setup_result.as_ref().is_none_or(|result| result.is_ok())
    ```
  - The workspace entry is still created with `agent_preset_id` set to the user's selected preset, regardless of auto-launch.

  **Patterns to follow:**

  - Existing parameter threading pattern: `confirm()` already extracts `self.base_branch(cx)`, `self.setup_script(cx)`, etc. and passes them through.

  **Test scenarios:**

  - Happy path: creating workspace with auto-launch OFF → workspace appears in sidebar with correct preset assigned, no terminal/ACP session started.
  - Happy path: creating workspace with auto-launch ON → workspace created and preset launches (identical to current behavior).
  - Happy path: selecting a non-default preset → workspace created with that preset's ID in `agent_preset_id`.
  - Edge case: auto-launch ON but setup script fails → preset should NOT launch (existing safety behavior preserved).

  **Verification:**

  - Creating a workspace with auto-launch OFF produces a workspace entry with the selected preset but no running agent session.
  - Creating a workspace with auto-launch ON behaves identically to the current (pre-change) behavior.
  - The selected preset ID is correctly stored in the workspace entry's `agent_preset_id`.

## System-Wide Impact

- **Interaction graph:** `NewWorkspaceModal.confirm()` → `spawn_new_workspace_request` → `should_launch_preset` → `launch_workspace_preset`. Only the parameter threading and condition check change; `launch_workspace_preset` itself is untouched.
- **Error propagation:** No change — setup failure still suppresses launch regardless of auto-launch setting.
- **State lifecycle risks:** None — `auto_launch` is a transient modal field, not persisted. `selected_preset_id` flows into the existing `agent_preset_id` persistence path.
- **API surface parity:** Other callers of `spawn_new_workspace_request` (if any) will need the new parameters. Verify there are no other call sites, or update them to pass `(default_preset_id, true)` to preserve existing behavior.
- **Unchanged invariants:** The sidebar preset selector, existing workspace preset launch behavior, and `launch_workspace_preset` function signature are all unchanged.

## Risks & Dependencies

| Risk                                                                           | Mitigation                                                                                            |
| ------------------------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------- |
| Other call sites of `spawn_new_workspace_request` break after signature change | Grep for all callers during Unit 3 and update them with backward-compatible defaults                  |
| Preset list empty when modal opens                                             | `default_preset()` already panics if no presets exist — this is an existing invariant, not a new risk |

## Sources & References

- **Origin document:** [docs/brainstorms/2026-04-15-workspace-creation-preset-control-requirements.md](docs/brainstorms/2026-04-15-workspace-creation-preset-control-requirements.md)
- Related code: `crates/superzent_ui/src/lib.rs` (NewWorkspaceModal, spawn_new_workspace_request)
- Related code: `crates/superzent_model/src/lib.rs` (AgentPreset, SuperzentStore)
