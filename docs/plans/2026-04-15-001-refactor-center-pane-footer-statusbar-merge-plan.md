---
title: "refactor: Merge CenterPaneFooter into StatusBar"
type: refactor
status: active
date: 2026-04-15
origin: docs/brainstorms/2026-04-15-center-pane-footer-statusbar-merge-requirements.md
---

# Merge CenterPaneFooter into StatusBar

## Overview

Remove the `CenterPaneFooter` component and consolidate all bottom-bar functionality into the existing `StatusBar`. Reposition `StatusBar` from the window bottom edge to the center pane footer position (between left and right docks). This reduces upstream divergence while preserving Superzent's desired item composition and layout.

## Problem Frame

Superzent maintains two structurally near-identical bottom bars — `CenterPaneFooter` (visible, positioned between docks) and `StatusBar` (hidden by default, full window width). Both wrap the same `StatusItemStrip` infrastructure. The CenterPaneFooter exists solely because Superzent wanted a different item set and position than the upstream StatusBar. Maintaining two components increases merge friction with Zed upstream and duplicates active-pane tracking, item registration, and rendering logic. (see origin: `docs/brainstorms/2026-04-15-center-pane-footer-statusbar-merge-requirements.md`)

## Requirements Trace

- R1. Remove `CenterPaneFooter` as a separate component
- R2. Position `StatusBar` in the center pane footer location (between docks)
- R3. `StatusBar` visible by default (`show: true`), setting still controls visibility
- R4. Item composition: Left (Edit Prediction, LSP, Search, Diagnostics, Activity Indicator) / Right (Vim Mode, Pending Keystroke, Cursor Position, Active Toolchain, Buffer Language, Buffer Encoding, Line Endings, Terminal+Debug panel buttons)
- R5. Remove: Left/Right dock panel toggle buttons, Image Info
- R6. Visual style follows existing StatusBar render style, adapted for center-pane width
- R7. Window decoration handling adjusted for non-window-bottom position

## Scope Boundaries

- No changes to `StatusItemView` trait or `StatusItemStrip` internals
- No new items or indicators
- Left/Right dock panel toggle buttons are not relocated — accessible via shortcuts and menus

## Context & Research

### Relevant Code and Patterns

- `crates/workspace/src/status_bar.rs` — `StatusBar` (line 147) and `CenterPaneFooter` (line 153) are thin wrappers around `StatusItemStrip`. CenterPaneFooter has `has_items()` (line 296) that StatusBar lacks. StatusBar has `item_of_type`, `position_of_item`, `insert_item_after`, `remove_item_at` that CenterPaneFooter lacks.
- `crates/workspace/src/workspace.rs` — Construction at lines 1639-1657, fields at 1312-1313, accessors at 2192-2213, active pane sync at 4794-4807, layout rendering across 4 `BottomDockLayout` variants (7850-8012), footer helpers at 7102-7136.
- `crates/zed/src/zed.rs` lines 530-548 — Single external mutation point for both bars.
- `CENTER_PANE_FOOTER_BOTTOM_PANELS` constant (workspace.rs line 176) — splits Terminal/Debug buttons to footer, rest to StatusBar.
- `assets/settings/default.json` line 1637 — `"experimental.show": false`.

### Institutional Learnings

- The footer vs status bar distinction is a discoverability decision. When consolidating, items must land on the surface the shell actually renders (docs/solutions: `default-build-next-edit-surface-restoration`).
- `#[cfg(feature = "next_edit")]` gates Edit Prediction Button — must be preserved after consolidation.

## Key Technical Decisions

- **StatusBar render style**: Use `CenterPaneFooter`'s current style (top border, `panel_background`) rather than StatusBar's current style (`status_bar_background`, no top border) since the bar is now inside the pane area, not at the window edge. This matches the visual expectation and avoids the window decoration rounding question entirely.
- **Remove `workspace_sidebar_open` from StatusBar**: This field only controlled bottom-left corner rounding for client-side window decorations at the window bottom edge. Since the bar moves away from the window edge, this field and its propagation chain become dead code.
- **Keep `has_items()` as belt-and-suspenders**: Add `has_items()` to StatusBar even though it will always have items. The check is cheap and prevents rendering an empty bar if item registration changes in the future.
- **Rename `CENTER_PANE_FOOTER_BOTTOM_PANELS` to `STATUS_BAR_BOTTOM_PANELS`**: The constant still serves the same purpose (identifying which bottom dock panels get toggle buttons in the bar) but the old name references a component that no longer exists.

## Open Questions

### Resolved During Planning

- **Q: Should the bar use `status_bar_background` or `panel_background`?** Resolution: Use `panel_background` with top border, matching the current CenterPaneFooter visual. The bar sits inside the pane area, not at the window edge.
- **Q: What happens to `which_key` modal positioning?** Resolution: `which_key_modal.rs` line 152 checks `status_bar_visible()` to add bottom padding. This still works correctly — it will now always account for the bar since `show` defaults to `true`.

### Deferred to Implementation

- Exact layout adjustments if any `BottomDockLayout` variant positions the center pane footer in a surprising way after testing.

## Implementation Units

- [ ] **Unit 1: Prepare StatusBar to absorb CenterPaneFooter's role**

  **Goal:** Add missing capabilities to StatusBar and update its render style to match the center-pane-footer position.

  **Requirements:** R1, R6, R7

  **Dependencies:** None

  **Files:**

  - Modify: `crates/workspace/src/status_bar.rs`

  **Approach:**

  - Add `has_items() -> bool` method to StatusBar (delegate to `self.items.has_items()`)
  - Remove `workspace_sidebar_open` field and `set_workspace_sidebar_open()` method from StatusBar
  - Change StatusBar's `Render` impl to match CenterPaneFooter's style: top border (`border_t_1`), `panel_background` background, remove all client-side window decoration rounding logic
  - Delete the entire `CenterPaneFooter` struct, its `Render` impl, and its `impl` block

  **Patterns to follow:**

  - CenterPaneFooter's `Render` impl (status_bar.rs lines 187-200) is the target style
  - CenterPaneFooter's `has_items()` (line 296) is the pattern to port

  **Test scenarios:**

  - Happy path: StatusBar renders with top border and `panel_background` when items are present
  - Happy path: `has_items()` returns true when items are added, false when empty
  - Edge case: StatusBar with no items renders nothing (or empty bar) — verify `has_items()` returns false

  **Verification:**

  - `CenterPaneFooter` type no longer exists in the codebase
  - StatusBar compiles with new render style and `has_items()` method

- [ ] **Unit 2: Consolidate workspace construction and field management**

  **Goal:** Remove all CenterPaneFooter references from Workspace struct and consolidate PanelButtons setup.

  **Requirements:** R1, R4, R5

  **Dependencies:** Unit 1

  **Files:**

  - Modify: `crates/workspace/src/workspace.rs`

  **Approach:**

  - Remove `center_pane_footer: Entity<CenterPaneFooter>` field from Workspace struct
  - Remove `center_pane_footer()` accessor and `center_pane_footer_visible()` method
  - Remove `CenterPaneFooter` from the `use status_bar::{...}` import
  - In construction (lines 1638-1657): remove `center_pane_footer_bottom_dock_buttons` creation. Change `status_bar_bottom_dock_buttons` to use `PanelButtons::new_only` (was `new_except`) with `STATUS_BAR_BOTTOM_PANELS` — now the StatusBar gets _only_ Terminal+Debug buttons instead of _everything except_ them
  - Remove `left_dock_buttons` and `right_dock_buttons` creation and their `add_left_item`/`add_right_item` calls on StatusBar (R5: removing left/right dock panel toggle buttons)
  - Remove `center_pane_footer` from struct initialization
  - In `set_status_item_active_pane()` (line 4794): remove the `center_pane_footer.update()` call, keep only the `status_bar.update()` call
  - Remove `set_workspace_sidebar_open()` method from Workspace (dead code after Unit 1 removed the StatusBar field)
  - Rename `CENTER_PANE_FOOTER_BOTTOM_PANELS` to `STATUS_BAR_BOTTOM_PANELS`

  **Patterns to follow:**

  - Existing StatusBar construction pattern (lines 1651-1656)

  **Test scenarios:**

  - Happy path: Workspace initializes successfully with only StatusBar, no CenterPaneFooter
  - Happy path: Active pane changes propagate to StatusBar items
  - Integration: Terminal and Debug panel buttons appear in StatusBar's right items

  **Verification:**

  - No references to `CenterPaneFooter` or `center_pane_footer` remain in workspace.rs
  - No references to `left_dock_buttons` or `right_dock_buttons` PanelButtons remain
  - Project compiles

- [ ] **Unit 3: Reposition StatusBar rendering in workspace layout**

  **Goal:** Move StatusBar from the window bottom edge to the center pane footer position, respecting all four `BottomDockLayout` variants.

  **Requirements:** R2

  **Dependencies:** Unit 2

  **Files:**

  - Modify: `crates/workspace/src/workspace.rs`

  **Approach:**

  - Remove the old StatusBar rendering at the window bottom (lines 8039-8041)
  - Replace every `center_pane_footer_visible(cx)` / `center_pane_footer.clone()` occurrence in the four `BottomDockLayout` branches with `status_bar_visible(cx)` / `status_bar.clone()`
  - In `render_center_pane_footer_row()` and `render_center_pane_footer_with_left_offset()`: replace `self.center_pane_footer.clone()` with `self.status_bar.clone()`. Consider renaming these methods to `render_status_bar_row()` and `render_status_bar_with_left_offset()` for clarity.
  - The four layout variants are:
    - **Contained**: footer rendered inside center column with no left offset — use `status_bar_visible(cx)` check, render `status_bar`
    - **LeftAligned**: footer rendered inside left column with left dock width offset — same approach
    - **Full**: footer rendered as separate row after main content with dock-width spacers — same approach
    - **RightAligned**: footer rendered as separate row after main content with dock-width spacers — same approach

  **Patterns to follow:**

  - The existing four-variant layout rendering pattern (lines 7850-8012) — same structure, just swapping the entity reference and visibility check

  **Test scenarios:**

  - Happy path: StatusBar renders in center pane footer position with `BottomDockLayout::Contained`
  - Happy path: StatusBar respects dock widths in `Full` and `RightAligned` layouts (dock-width spacers present)
  - Happy path: StatusBar respects left dock width in `LeftAligned` layout
  - Edge case: `status_bar.show = false` hides the bar in all four layout variants
  - Integration: `which_key` modal still adds bottom padding when StatusBar is visible

  **Verification:**

  - StatusBar no longer renders at window bottom edge
  - StatusBar renders between docks in all four layout variants
  - Visual inspection confirms correct positioning

- [ ] **Unit 4: Consolidate item registration**

  **Goal:** Merge all status item registrations into a single `status_bar().update()` call with the correct item composition and ordering.

  **Requirements:** R4, R5

  **Dependencies:** Unit 2

  **Files:**

  - Modify: `crates/zed/src/zed.rs`

  **Approach:**

  - Remove the `center_pane_footer().update()` call (lines 530-538)
  - Replace the `status_bar().update()` call (lines 540-548) with a single call registering all items in order:
    - Left: `edit_prediction_ui` (cfg-gated with `#[cfg(feature = "next_edit")]`), `lsp_button`, `search_button`, `diagnostic_summary`, `activity_indicator`
    - Right: `vim_mode_indicator`, `pending_keystroke_indicator`, `cursor_position`, `active_toolchain_language`, `active_buffer_language`, `active_buffer_encoding`, `line_ending_indicator`
    - Note: Terminal+Debug panel buttons are already added during workspace construction (Unit 2), so they should appear at the far right after these items
  - Remove creation of `image_info` variable after removing its registration
  - Verify right-side ordering: items are rendered in reverse order by `StatusItemStrip` (line 62 of status_bar.rs), so registration order must be reversed from desired display order. The desired display is: `[Vim][Keys][Cursor][Toolchain][Lang][Encoding][LineEnding][Terminal][Debug]`. Since Terminal+Debug are added first during construction, the remaining items added in zed.rs should be added in reverse display order: `line_ending_indicator`, `active_buffer_encoding`, `active_buffer_language`, `active_toolchain_language`, `cursor_position`, `pending_keystroke_indicator`, `vim_mode_indicator`

  **Patterns to follow:**

  - Current registration pattern in zed.rs (lines 530-548)
  - `StatusItemStrip::render_right_tools()` reverses item order (status_bar.rs line 62)

  **Test scenarios:**

  - Happy path: All 11 items render in correct left/right positions
  - Happy path: Terminal+Debug panel buttons appear at the far right
  - Happy path: Edit Prediction Button only appears when `next_edit` feature is enabled
  - Integration: Items respond to active pane changes (cursor position updates, language changes, etc.)

  **Verification:**

  - No `center_pane_footer()` calls remain in zed.rs
  - `image_info` variable is removed
  - Visual inspection confirms item ordering matches the spec

- [ ] **Unit 5: Update settings default and tests**

  **Goal:** Change StatusBar default visibility to `true` and update tests to match.

  **Requirements:** R3, S5, S6

  **Dependencies:** Units 1-4

  **Files:**

  - Modify: `assets/settings/default.json`
  - Modify: `crates/workspace/src/workspace.rs` (test at line 13498)

  **Approach:**

  - In `assets/settings/default.json` line 1637: change `"experimental.show": false` to `"experimental.show": true`
  - In `test_status_bar_visibility` (workspace.rs line 13498): flip the first assertion from `assert!(!visible, "Status bar should be hidden by default")` to `assert!(visible, "Status bar should be visible by default")`
  - Update the test's comment `"Superzent hides the status bar by default"` to reflect the new default

  **Patterns to follow:**

  - Existing test structure at workspace.rs lines 13498-13535

  **Test scenarios:**

  - Happy path: `status_bar_visible()` returns `true` by default
  - Happy path: Setting `show: false` hides the bar
  - Happy path: Setting `show: true` shows the bar (no change from current)

  **Verification:**

  - `test_status_bar_visibility` passes with updated assertions
  - `./script/clippy` passes

## System-Wide Impact

- **Interaction graph:** `MultiWorkspace` currently propagates sidebar state to `Workspace::set_workspace_sidebar_open()` → `StatusBar::set_workspace_sidebar_open()`. After removing this chain, verify `MultiWorkspace` callers compile (they will get a compile error pointing to the removed method, which is the desired signal).
- **Error propagation:** No error paths affected — this is a pure UI restructuring.
- **State lifecycle risks:** None — both bars already share the same active-pane subscription pattern.
- **API surface parity:** `workspace.center_pane_footer()` accessor is removed. The only external caller is `zed.rs`, which will be updated in Unit 4.
- **Integration coverage:** The `which_key` modal (line 152) uses `status_bar_visible()` for positioning. Since `show` now defaults to `true`, the modal will consistently add bottom padding. The `vim_test_context` (line 137) and `go_to_line` tests (lines 499-727) add items to `status_bar()` — these continue to work since StatusBar remains.
- **Unchanged invariants:** `StatusItemView` trait, `StatusItemStrip` internals, and all individual status item components are untouched.

## Risks & Dependencies

| Risk                                                                                   | Mitigation                                                                                  |
| -------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------- |
| Right-item ordering is reversed by `StatusItemStrip` — easy to get display order wrong | Carefully trace `render_right_tools()` reversal logic; verify visually after implementation |
| `set_workspace_sidebar_open` removal may break callers in `MultiWorkspace`             | Compile errors will surface immediately; search for all callers before removing             |
| `BottomDockLayout::Full` and `RightAligned` variants have distinct footer placement    | Test all four variants manually or with layout tests                                        |

## Sources & References

- **Origin document:** [docs/brainstorms/2026-04-15-center-pane-footer-statusbar-merge-requirements.md](docs/brainstorms/2026-04-15-center-pane-footer-statusbar-merge-requirements.md)
- Related code: `crates/workspace/src/status_bar.rs`, `crates/workspace/src/workspace.rs`, `crates/zed/src/zed.rs`
- Institutional learning: `docs/solutions/integration-issues/default-build-next-edit-surface-restoration-2026-04-05.md`
