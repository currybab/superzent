---
title: "Reuse GitPanel modes for Superzent sidebar tabs"
date: "2026-05-17"
category: architecture-patterns
module: superzent_ui
problem_type: architecture_pattern
component: tooling
severity: low
related_components:
  - "git_ui"
  - "ui"
applies_when:
  - "Adding a Superzent right details sidebar tab backed by an existing Zed panel"
  - "Surfacing branch commit history without building a separate Git history view"
  - "Keeping full and compact right sidebar tab icons visually aligned with selected label color"
tags:
  - superzent-ui
  - right-sidebar
  - git-panel
  - history-tab
  - commit-history
  - icon-button
  - selected-state
---

# Reuse GitPanel modes for Superzent sidebar tabs

## Context

Superzent needed a right details sidebar `History` tab next to `Changes` and `Files`, in the user-facing order `Changes / History / Files`. The goal was branch commit history in the details sidebar, not a full Git Graph lane view or GitLens-style CodeLens feature.

The existing `git_ui::GitPanel` already had the right ownership boundary for changes and history data. The durable pattern is to treat `History` as another mode of the shared Git panel while letting `SuperzentRightSidebar` own the outer sidebar tab state.

## Guidance

When a new top-level Superzent sidebar tab is another mode of an existing panel, reuse the existing panel entity and synchronize sidebar tab state into that panel. Avoid creating a parallel view that duplicates repository subscriptions, keyboard handling, commit opening, or scroll state.

For the history sidebar work, `SuperzentRightSidebar` owns `RightSidebarTab::History`, while `GitPanel` owns `GitPanelTab::History` and the commit-history rendering. The bridge is intentionally small:

```rust
fn set_active_tab(&mut self, tab: RightSidebarTab, cx: &mut Context<Self>) {
    self.tab = tab;
    self.sync_git_panel_tab(cx);
    cx.notify();
}

fn sync_git_panel_tab(&self, cx: &mut Context<Self>) {
    match self.tab {
        RightSidebarTab::Changes => {
            self.git_panel
                .update(cx, |git_panel, cx| git_panel.show_changes_tab(cx));
        }
        RightSidebarTab::History => {
            self.git_panel
                .update(cx, |git_panel, cx| git_panel.show_history_tab(cx));
        }
        RightSidebarTab::Files | RightSidebarTab::Panel(_) => {}
    }
}
```

Keep legacy actions predictable. In Superzent, the Git panel toggle/focus action should still land on `Changes`, even when `History` exists:

```rust
.on_action(
    cx.listener(|this, _: &git_ui::git_panel::ToggleFocus, window, cx| {
        this.set_active_tab(RightSidebarTab::Changes, cx);
        window.focus(&this.focus_handle, cx);
    }),
)
```

Keep `GitPanel` responsible for history internals. The panel should expose mode setters such as `show_changes_tab` and `show_history_tab`, and it should clear history-specific state when leaving history mode:

```rust
pub fn show_changes_tab(&mut self, cx: &mut Context<Self>) {
    self.set_active_tab(GitPanelTab::Changes, cx);
}

pub fn show_history_tab(&mut self, cx: &mut Context<Self>) {
    self.set_active_tab(GitPanelTab::History, cx);
}
```

For selected tab icon polish in GPUI, label and icon color do not automatically stay in sync. Set both the label color and the icon color from the same selected-state token:

```rust
let color = if active {
    Color::Selected
} else {
    Color::Default
};

Button::new(id, label)
    .color(color)
    .selected_label_color(color)
    .when_some(icon, |button, icon| {
        button.start_icon(Icon::new(icon).size(IconSize::Small).color(color))
    })
    .toggle_state(active)
    .selected_style(ui::ButtonStyle::Filled);
```

For compact icon-only tabs, `IconButton` needs the same selected color contract. `selected_icon_color` should take precedence over the color implied by `selected_style`:

```rust
self.selected_icon_color
    .or_else(|| self.selected_style.map(Into::into))
    .unwrap_or(Color::Selected)
```

## Why This Matters

Reusing `GitPanel` keeps one source of truth for branch history, commit details, repository updates, keyboard navigation, and commit opening. A separate Superzent-only history view would have copied behavior from `git_ui`, increasing the chance that history rows, shortcuts, or repository refreshes drift from the standard Git panel.

Separating outer tab state from inner panel mode also keeps product behavior clear. Users get `Changes / History / Files` as first-class Superzent tabs, while existing Git actions continue to mean "show changes" unless the action itself is intentionally redesigned.

The selected icon color fix prevents a common GPUI polish issue: selected labels can use `Color::Selected` while icons silently inherit a filled or tinted button-style color. Keeping both on the same token makes full-width and compact sidebar tabs render consistently.

## When to Apply

- Adding a first-class Superzent sidebar tab that is really a mode of an existing panel.
- Backporting or adapting an upstream panel feature without importing a broader layout architecture.
- Preserving existing shortcuts while adding a secondary tab under the same panel namespace.
- Styling GPUI `Button` or `IconButton` instances where selected icon color must match selected text color.

## Examples

Prefer this structure:

- `SuperzentRightSidebar` owns the product-level tab order and close/focus behavior.
- `GitPanel` owns Git-specific data loading, repository subscriptions, list rendering, and row interaction.
- Sidebar actions switch `RightSidebarTab`; the sidebar then calls the panel's public mode setters.
- Shared UI components such as `IconButton` encode selected-state precedence once, then callers pass selected colors directly.

Avoid this structure:

- A separate Superzent history component that independently queries branch history.
- A top-level `History` tab that bypasses `GitPanel` keyboard navigation or commit opening behavior.
- Treating `History` like an external dock panel when close behavior should match built-in sidebar tabs.
- Relying on `selected_style` to color icons when a specific selected label color is required.

## Verification

The history sidebar change was verified with:

- `cargo test -p git_ui test_history_tab_loads_branch_commit_history -- --nocapture`
- `cargo test -p superzent test_superzent_history -- --nocapture`
- `cargo test -p superzent test_superzent_git_panel_toggle_opens_changes_tab -- --nocapture`
- `cargo fmt --check`
- `cargo check -p git_ui -p superzent_ui -p superzent`

The selected icon color polish was verified with:

- `cargo fmt --check`
- `cargo check -p superzent_ui`

## Related

- Related local pattern: [Backport markdown preview search against the current renderer](../best-practices/markdown-preview-search-backport-current-renderer-2026-05-05.md)
- Related local GPUI pattern: [GPUI window re-borrow and cross-workspace item transfer patterns](../best-practices/gpui-window-reborrow-and-cross-workspace-item-transfer-2026-04-21.md)
- Upstream source feature: [zed-industries/zed#56500](https://github.com/zed-industries/zed/pull/56500)
- Upstream keyboard/tab behavior: [zed-industries/zed#56743](https://github.com/zed-industries/zed/pull/56743)
