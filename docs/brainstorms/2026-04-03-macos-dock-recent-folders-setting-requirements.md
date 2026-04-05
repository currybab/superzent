---
date: 2026-04-03
topic: macos-dock-recent-folders-setting
---

# macOS Dock Recent Folders Setting

## Problem Frame

When users right-click the app icon in the macOS Dock, macOS shows recently opened local
folders. For users who work with many temporary or disposable workspaces, this creates
clutter and can expose folders they do not want surfaced from the Dock. The product should
let users suppress this Dock-specific recent-folder behavior without removing the app's own
Recent Projects experience.

## Requirements

**Settings**

- R1. Add a user-facing `settings.json` setting that controls whether the app contributes
  local folders to macOS Dock recent items.
- R2. The setting must default to disabled on macOS.
- R3. The setting must not be exposed in Settings UI in this phase.
- R4. The setting's effect must be limited to macOS Dock recent folders; the app's internal
  Recent Projects surfaces must continue to work unchanged.

**Behavior**

- R5. When the setting is disabled, newly opened visible local workspaces must not be added
  to the macOS Dock recent-folder list.
- R6. When the app starts on macOS with the setting disabled, any existing Dock recent-folder
  entries previously contributed by the app should be cleared automatically.
- R7. When a user changes the setting from enabled to disabled on macOS, the existing Dock
  recent-folder entries should be cleared immediately.

**Platform Boundaries**

- R8. Non-macOS platforms must not have their current recent-workspace behavior changed by
  this setting.
- R9. On macOS, enabling the setting should preserve current behavior for future entries.

## Success Criteria

- Right-clicking the Dock icon on macOS no longer shows previously recorded recent folders
  after startup when the setting is disabled.
- Opening additional local workspaces while the setting is disabled does not repopulate Dock
  recent folders.
- In-app Recent Projects and workspace history continue to behave as they do today.
- Non-macOS behavior is unchanged.

## Scope Boundaries

- No Settings UI control in this phase.
- No change to app-internal Recent Projects lists, launchpad content, or workspace history
  persistence.
- No attempt to change system-wide recent item behavior outside the app's Dock recent-folder
  contribution.

## Key Decisions

- JSON-only control for this phase: keep the change targeted and avoid expanding the Settings
  UI surface.
- macOS default is disabled: optimize for privacy and clutter reduction in Dock behavior.
- Disabling clears existing items automatically: users should not need a second manual cleanup
  step.
- Scope is Dock-only: in-app Recent Projects remains the intentional history surface.

## Dependencies / Assumptions

- The Dock recent-folder entries are currently being populated through the app's macOS
  recent-document integration rather than through a custom Dock menu.
- Clearing the app's macOS recent-document list can be done without affecting the app's
  internal recent-project persistence.

## Outstanding Questions

### Deferred to Planning

- [Affects R1][Technical] What exact `settings.json` key name and schema placement best match
  existing workspace-setting conventions?
- [Affects R6][Technical] What is the safest lifecycle hook for clearing macOS recent-document
  entries at startup without causing repeated unnecessary work?
- [Affects R7][Technical] What is the cleanest way to detect and handle runtime setting
  transitions from enabled to disabled?

## Next Steps

-> /prompts:ce-plan for structured implementation planning
