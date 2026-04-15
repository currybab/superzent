# Workspace Creation Preset Control

**Date:** 2026-04-15
**Status:** Draft

## Problem

When creating a new workspace, the assigned agent preset is automatically launched without any user opt-in. There is no UI in the creation modal to choose which preset to use or to disable auto-launch. This leads to unintended preset execution every time a workspace is created.

## Goals

- Give users explicit control over preset selection and auto-launch at workspace creation time.
- Default to NOT auto-launching, so preset execution only happens when the user consciously opts in.

## Non-Goals

- Changing the preset auto-launch behavior for existing workspaces opened from the sidebar.
- Adding a global setting for default auto-launch preference (can be explored later).

## Requirements

### R1: Preset Selector in Creation Modal

Add a preset dropdown to `NewWorkspaceModal` that lets the user choose which agent preset to assign to the new workspace.

- Default selection: the project's current default preset (from `store.default_preset()`).
- Shows all available presets from the store.

### R2: Auto-Launch Toggle

Add a toggle/checkbox in `NewWorkspaceModal` to control whether the selected preset should be automatically launched after workspace creation.

- **Default: OFF** — preset is assigned to the workspace but not launched.
- When ON, the current `launch_workspace_preset` flow runs as it does today.
- When OFF, the workspace is created with the preset assigned (via `agent_preset_id`) but `should_launch_preset` is skipped.

### R3: Pass Launch Preference Through Creation Flow

The user's auto-launch choice needs to flow from `NewWorkspaceModal.confirm()` through `spawn_new_workspace_request` to the point where `should_launch_preset` is evaluated.

## UI Placement

The preset selector and auto-launch toggle should appear in the creation modal below the branch/base-branch fields and above (or alongside) the existing "More Options" section for setup/teardown scripts.

## Success Criteria

- Creating a workspace with auto-launch OFF results in the workspace appearing in the sidebar with its preset assigned but no terminal/ACP session started.
- Creating a workspace with auto-launch ON behaves identically to current behavior.
- The preset dropdown correctly reflects available presets and the selection is persisted to the workspace entry.
