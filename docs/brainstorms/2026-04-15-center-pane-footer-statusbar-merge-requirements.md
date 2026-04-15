---
date: 2026-04-15
topic: center-pane-footer-statusbar-merge
---

# Center Pane Footer / Status Bar Merge

## Problem Frame

Superzent currently maintains two separate bottom bars: `CenterPaneFooter` (visible, positioned between docks) and `StatusBar` (hidden by default, full window width). Both use the same `StatusItemStrip` infrastructure and `StatusItemView` trait. The CenterPaneFooter was introduced as Superzent's replacement for the upstream StatusBar, but maintaining a separate component increases divergence from Zed upstream and creates redundant code paths. Merging them reduces that divergence while preserving Superzent's desired layout and item composition.

## Requirements

**Structural**

- R1. Remove `CenterPaneFooter` as a separate component. The `StatusBar` becomes the single bottom information bar.
- R2. Position the `StatusBar` where `CenterPaneFooter` currently renders: inside the center pane column, between the left and right docks — not at the full window width.
- R3. The `StatusBar` must be visible by default (`show: true`). The existing `status_bar.show` setting must continue to control visibility.

**Item Composition**

- R4. The merged bar must display the following items, split left/right:

  Left (status/tool indicators):
  - Edit Prediction Button
  - LSP Button
  - Diagnostic Indicator
  - Activity Indicator

  Right (editor context info, in order):
  - Vim Mode Indicator
  - Pending Keystroke Indicator
  - Cursor Position
  - Active Toolchain
  - Buffer Language
  - Buffer Encoding
  - Line Ending Indicator
  - **Terminal Panel + Debug Panel toggle buttons** (far right, preserving current `CENTER_PANE_FOOTER_BOTTOM_PANELS` behavior)

- R5. The following current StatusBar items must be removed from registration:
  - Left/Right dock Panel Toggle Buttons
  - Search Button
  - Image Info

**Visual**

- R6. The bar's visual style (border, background, padding) should follow the existing `StatusBar` render style, adapted for the narrower center-pane-width position.
- R7. Window decoration handling (bottom corner rounding for client-side decorations) must be adjusted since the bar is no longer at the window bottom edge.

## Non-Goals

- N1. No new items or indicators are being added — this is a consolidation of existing items.
- N2. No changes to the `StatusItemView` trait or `StatusItemStrip` internals.
- N3. Left/Right dock panel toggle buttons do not need an alternative placement — they remain accessible via keyboard shortcuts and menus.

## Success Criteria

- S1. Only one bottom bar component exists in the codebase.
- S2. The bar renders between docks at the center pane bottom, matching current CenterPaneFooter position.
- S3. All specified items render correctly and respond to active pane changes.
- S4. Terminal Panel and Debug Panel toggle buttons remain at the far right.
- S5. `status_bar.show` setting still toggles visibility.
- S6. Existing tests for StatusBar visibility pass (with updated default expectation).
