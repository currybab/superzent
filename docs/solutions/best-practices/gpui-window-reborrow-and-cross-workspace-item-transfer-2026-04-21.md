---
title: "GPUI window re-borrow and cross-workspace item transfer patterns"
date: "2026-04-21"
module: "gpui / workspace"
problem_type: best_practice
component: development_workflow
severity: high
applies_when:
  - "Accessing a root entity from inside a render callback or paint context"
  - "Dispatching or handling actions from a tab context menu or right-click menu"
  - "Reading workspace state from an action handler registered at the workspace level"
  - "Moving an item across workspace boundaries (cross-workspace tab transfer)"
  - "Using SuperzentStore or other globals from non-Superzent init paths"
symptoms:
  - "Panic: attempted to read a window that is already on the stack"
  - "Wrong pane selected when workspace is focused on a dock panel"
  - "Stale dirty-item state left on source workspace after cross-workspace move"
  - "Duplicate sidebar entries when import path is opened before upsert"
root_cause: thread_violation
resolution_type: code_fix
related_components:
  - "terminal_view"
  - "multi_workspace"
  - "superzent_store"
tags:
  - gpui
  - window
  - multi-workspace
  - context-menu
  - action-dispatch
  - tab-move
  - workspace-transfer
  - re-borrow
---

# GPUI window re-borrow and cross-workspace item transfer patterns

## Context

During the implementation of cross-workspace tab moves and worktree import on the `tab-move-workspace` branch, a deceptively small feature surface — a right-click context menu entry on terminal tabs, a modal workspace picker, and a worktree import picker — exposed roughly nine distinct edge-cases across four rounds of code review. The bugs did not surface in compilation, static analysis, or unit tests; they only appeared when specific interactive paths were exercised in the running application (right-clicking a tab, confirming the picker after a terminal had already exited, returning to a workspace whose pane index was corrupted). The recurrence across four review iterations across two distinct pickers (workspace-move and import-worktree) confirmed that the underlying patterns — GPUI's window-lease exclusivity and the non-symmetry of cross-workspace item transfer — are a reliable source of subtle runtime failures and are not obvious from reading GPUI's API surface alone.

The same root patterns also reproduced in the import-worktree picker: async closures calling `Entity::update` with the wrong context type, the wrong sequencing of `upsert_workspace` vs. `open_local_workspace_path_and_resolve`, and the assumption that cross-crate globals are always initialized. Documenting both pieces of guidance together is warranted because both pickers were implemented in the same PR and the lessons are directly transferable to any future picker or modal that touches workspace state.

## Guidance

### GPUI window re-borrow

When GPUI begins a window update cycle, it moves the `Window` struct out of `App.windows` onto the call stack (the "window lease"). Any call path that tries to re-enter the window through `WindowHandle::read_with` or `WindowHandle::update` — which both route through `App::read_window` / `App::update_window` — will fail to find the window in the map and panic with:

> `attempted to read a window that is already on the stack`

This affects every context in which the window is already on the stack: `Render::render`, `Item::tab_extra_context_menu_actions`, `Item::tab_tooltip_text`, and any action handler that fires synchronously during a window update (including those dispatched via `window.dispatch_action` or `focus_handle.dispatch_action`). The panic is triggered by user interaction (a right-click is enough), not by any code path exercised during compilation or headless tests.

**Wrong — re-borrows the leased window via `WindowHandle::read_with`:**

```rust
fn tab_extra_context_menu_actions(
    &self,
    window: &mut Window,
    cx: &mut Context<Self>,
) -> Vec<(SharedString, Box<dyn gpui::Action>)> {
    // PANIC: window is on the stack; read_with tries to find it in App.windows
    let other_workspace_count = window
        .window_handle()
        .downcast::<MultiWorkspace>()
        .and_then(|handle| {
            handle
                .read_with(cx, |multi_workspace, _| multi_workspace.workspaces().len())
                .ok()
        })
        .unwrap_or(0);
    // ...
}
```

**Correct — reads the root entity directly through the already-leased window:**

```rust
fn tab_extra_context_menu_actions(
    &self,
    window: &mut Window,
    cx: &mut Context<Self>,
) -> Vec<(SharedString, Box<dyn gpui::Action>)> {
    let other_workspace_count = window
        .root::<MultiWorkspace>()
        .flatten()
        .map(|multi_workspace| multi_workspace.read(cx).workspaces().len())
        .unwrap_or(0);
    // ...
}
```

`window.root::<T>()` returns `Option<Option<Entity<T>>>` (outer `None` if the window has no root, inner `None` if the root is not `T`). The returned `Entity<T>` is read through `App` directly — no second borrow of the window is involved.

**The one safe place for `WindowHandle::*` methods:** inside `cx.spawn_in(window, async move |this, cx| { ... })`. The async closure runs after the current synchronous update returns and releases the window lease. In that context, calling `handle.update(cx, ...)` is safe.

```rust
// Safe: runs after the current update stack unwinds
cx.spawn_in(window, async move |this, cx| {
    if let Some(multi_workspace_handle) = window_handle.downcast::<MultiWorkspace>() {
        multi_workspace_handle
            .update(cx, |multi_workspace, cx| { /* ... */ })
            .ok();
    }
});
```

### Cross-workspace item transfer hygiene

Moving a tab (`Item`) from a `Pane` in workspace A to a `Pane` in workspace B is not symmetric to a within-workspace move. The following checklist captures every step that must be correct. Omitting any one of these steps produces either a runtime panic or silent state corruption that only manifests when the user returns to the affected workspace.

1. **Verify the source pane still holds the item before transferring.** Between the picker opening and the user confirming, the item may have closed independently (for example, a terminal exits and emits `CloseTerminal`). Check `source_pane.read(cx).items().any(|existing| existing.item_id() == item_id)` before proceeding; if false, emit `DismissEvent` and return. Without this guard, the picker's strong reference to the item via `Box<dyn ItemHandle>` resurrects the item in the target pane even though it was already removed from the source.

2. **Pass `activate_pane: true` to `Pane::remove_item` even when you do not want to visually activate the source pane.** `Pane::_remove_item`'s fallback-tab selection logic is gated on `has_focus || activate_pane`. When a modal picker holds focus, `has_focus` is false. With both false, the source pane's `active_item_index` is not updated after the item is removed and may point past the end of the items array, causing an index-out-of-bounds panic the next time that workspace is displayed. The target pane receives `add_item(.., activate: true, focus: true, ..)` immediately afterwards, so the brief nominal activation of the source is invisible to the user.

3. **Use `workspace.add_item_to_center(item, window, cx)` rather than `workspace.active_pane().add_item(...)` for items that belong in the center pane group.** `active_pane()` returns whichever pane last held focus, which may be a dock pane (for example, the bottom terminal panel) if the target workspace was previously viewed with the terminal panel focused. `add_item_to_center` walks `last_active_center_pane` instead, which is the correct insertion point for editor-class items. If `add_item_to_center` returns `false` (no live center pane), fall back to `active_pane()`.

4. **Clear the source workspace's dirty-item tracking explicitly after removing the item.** The dirty subscription fires when the item is fully released from all strong references; because the moved item is still alive in the target pane, that release never occurs and the source workspace retains phantom edited/dirty state indefinitely. Call `workspace.forget_item_dirty_state(item_id, window, cx)` (defined in `crates/workspace/src/workspace.rs`) immediately after `remove_item`. This method removes the `EntityId` from `dirty_items` and updates the window-edited flag.

5. **Activate the target workspace through `MultiWorkspace::activate` after the transfer.** `Pane::add_item` and `Pane::remove_item` only update pane-internal state; they do not switch which workspace is displayed in the `MultiWorkspace` split. After adding the item to the target pane, call:

   ```rust
   if let Some(Some(multi_workspace)) = window.root::<MultiWorkspace>() {
       multi_workspace.update(cx, |multi_workspace, cx| {
           multi_workspace.activate(target_workspace.clone(), cx);
       });
   }
   ```

6. **Register the tab-context-menu action on the item's element via `cx.listener`, not via `workspace.register_action`.** A workspace-level action handler resolves the source pane through `workspace.active_pane()`, which is the workspace's last-focused pane — wrong when the user right-clicked a tab in an inactive split. Registering via `.on_action(cx.listener(TerminalView::move_to_another_workspace))` on the element routes through the correct item entity. Inside that handler, resolve the source pane with `workspace.pane_for(&self_entity)`, which finds the pane that actually contains the item regardless of focus state.

7. **Use `SuperzentStore::try_global` rather than `SuperzentStore::global` in any code path that may run outside a fully initialized application.** `::global` panics if the store was not registered, which occurs in visual test runners and headless CLI contexts. `::try_global` returns `Option<Entity<SuperzentStore>>`; use the returned `Option` to provide a graceful display-name fallback rather than crashing.

### Sidebar observer ordering for workspace creation

When a sidebar like `SuperzentSidebar` observes `WorkspaceAdded` and auto-registers the new path as an unmanaged entry, opening a workspace before upserting the intended managed entry creates a duplicate (one auto-registered unmanaged entry plus the later managed upsert). Always `SuperzentStore::upsert_workspace(intended_entry)` before `open_local_workspace_path_and_resolve(path)`. On open failure, roll back via `store.remove_workspace(id)`.

`Entity::update` from a sync `Context<Picker<T>>` returns the closure's value directly, but the same call from an async closure context returns `()` instead of `Result<R>`. To get the `Result` form needed for `.ok()` chaining, capture `entity.downgrade()` once before spawning so the async block can call `entity_weak.update(cx, ..).ok()`.

## Why This Matters

These two patterns produce failures that are invisible to static analysis, the Rust compiler, and most test suites:

- The window re-borrow panic is a runtime-only crash. It fires on the first user right-click against the affected item, not during any build or test run. `clippy` does not flag `WindowHandle::read_with` calls; the type system does not distinguish between "window currently on the stack" and "window safe to access." Four rounds of code review were required before the pattern was recognized as a systemic issue rather than a one-off mistake.

- The cross-workspace transfer corruptions (wrong `active_item_index`, phantom dirty state, item insertion into a dock pane) are silent. The application does not crash immediately; it produces incorrect UI state — a workspace that appears edited when it is not, a tab that reappears in a dock panel instead of the editor area, or an index panic that occurs only when the user navigates back to the source workspace. These defects are reproducible only through specific interaction sequences and are not caught by unit tests that exercise individual pane operations in isolation.

The breadth of the issue (nine distinct edge-cases, two pickers, four review iterations) reflects the fact that neither pattern is documented at the GPUI API level and both require knowledge of runtime invariants — the window-lease cycle and the distinction between within-workspace and cross-workspace item ownership — that are not captured anywhere in the codebase's existing comments or documentation.

## When to Apply

Apply this guidance whenever you are:

- Writing or modifying a `Picker` or modal that opens over the main window and calls back into workspace or pane state upon confirmation.
- Adding an entry to `tab_extra_context_menu_actions`, `tab_tooltip_text`, or any other `Item` trait method that receives `&mut Window`.
- Registering an action on a workspace-level handler where the action may be triggered from a non-active split pane.
- Calling any `WindowHandle` method (`read_with`, `update`, `read`) anywhere downstream of a `Render::render` call or synchronous action dispatch.
- Implementing any feature that moves, copies, or transfers an `Item` between `Pane` instances in different `Workspace` entities within a `MultiWorkspace`.
- Accessing a cross-crate global (`SuperzentStore::global`) from a code path that may execute in a visual-test or headless context.
- Inserting items into a target workspace that may have its dock pane as the last-focused pane.

## Examples

The following files in the `tab-move-workspace` branch contain correct implementations of each pattern:

**`crates/terminal_view/src/terminal_view.rs`**

- `tab_extra_context_menu_actions` uses `window.root::<MultiWorkspace>()` to count workspaces without re-borrowing the leased window.
- `MoveTerminalToAnotherWorkspace` is registered via `.on_action(cx.listener(TerminalView::move_to_another_workspace))` on the item's element, not on the workspace.
- `move_to_another_workspace` resolves the source pane via `workspace.pane_for(&self_entity)` rather than `workspace.active_pane()`.

**`crates/terminal_view/src/workspace_move_picker.rs`**

- Source-still-has-item guard in `confirm` before any transfer begins.
- `pane.remove_item(item_id, true, false, window, cx)` — `activate_pane: true` to keep the source's `active_item_index` valid.
- `workspace.forget_item_dirty_state(item_id, window, cx)` called on the source workspace immediately after `remove_item`.
- `workspace.add_item_to_center(item, window, cx)` with `workspace.active_pane()` as fallback only if the center pane is not available.
- `window.root::<MultiWorkspace>()` used (not `WindowHandle::*`) to activate the target workspace after the transfer.
- `build_workspace_candidates` uses `SuperzentStore::try_global(cx)` with a path-based display-name fallback, avoiding a panic if the store is not initialized.

**`crates/workspace/src/workspace.rs`**

- `Workspace::forget_item_dirty_state` — removes the item's `EntityId` from `dirty_items` and updates the window-edited flag.
- `Workspace::add_item_to_center` — walks `last_active_center_pane` rather than `active_pane`, ensuring items land in the center pane group regardless of dock focus state.

**`crates/workspace/src/multi_workspace.rs`**

- `MultiWorkspace::workspace_entries_excluding_active` — enumerates all workspaces except the currently active one, used as the picker's candidate list so the user cannot "move" a tab to the workspace it already lives in.

**`crates/superzent_ui/src/import_worktree_picker.rs`**

- `store.upsert_workspace(workspace_entry, cx)` is called synchronously before `open_local_workspace_path_and_resolve`, ensuring the store entry exists before the workspace opens (avoids a duplicate-unmanaged-entry race).
- The open task is awaited in `cx.spawn_in`; on failure, `store_weak.update(cx, |store, cx| store.remove_workspace(...))` rolls back the upserted entry. `store_weak` (a `WeakEntity`) is captured before the spawn because `Entity::update` from an async closure context requires a weak handle to return `Result<R>`.

## Related

- [`docs/solutions/ui-bugs/managed-workspace-create-progress-toasts-can-fail-to-appear-after-local-open-2026-04-12.md`](../ui-bugs/managed-workspace-create-progress-toasts-can-fail-to-appear-after-local-open-2026-04-12.md) — Same underlying GPUI principle (hold a live `Entity` directly rather than re-resolving indirectly), applied to status toasts and workspace open flows.
- [`docs/plans/2026-04-16-001-feat-move-terminal-to-workspace-and-import-worktree-plan.md`](../../plans/2026-04-16-001-feat-move-terminal-to-workspace-and-import-worktree-plan.md) — Origin plan. Note that Units 2 and 3 prose recommended `WindowHandle::*` patterns that the implementation intentionally replaced with `window.root::<T>()`; the implementation in this doc supersedes that approach.
