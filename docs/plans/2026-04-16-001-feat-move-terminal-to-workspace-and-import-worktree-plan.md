---
title: "feat: Move terminal tab to workspace & import worktree"
type: feat
status: active
date: 2026-04-16
origin: docs/brainstorms/2026-04-15-move-terminal-tab-to-workspace-requirements.md
---

# Move Terminal Tab to Workspace & Import Worktree

## Overview

Two connected features: (A) move a terminal tab between workspaces while preserving the terminal session, and (B) import a single git worktree by branch name with an optional post-import terminal move prompt.

## Problem Frame

Superzent's `MultiWorkspace` hosts multiple workspaces in one window, but there is no cross-workspace tab movement. Terminal users must destroy and recreate sessions. Separately, "Sync Worktrees" imports all discovered worktrees, which is too coarse when agents create many worktrees and only one is needed. (see origin: `docs/brainstorms/2026-04-15-move-terminal-tab-to-workspace-requirements.md`)

## Requirements Trace

- A1. Terminal tab movable between any two open workspaces
- A2. Move adds to target workspace's center active pane
- A3. Terminal process, session state, and workspace/project bindings re-established for target
- A4. Target workspace activated and moved tab focused
- A5. Action hidden/disabled with fewer than 2 workspaces
- A6. Command palette `MoveTerminalToWorkspace` action
- A7. Tab context menu "Move to Workspace" below "Pin Tab"
- A8. Workspace picker lists other open workspaces
- A9. Single-tab source workspace stays open with empty pane
- A10. Source activates next tab per existing close ordering
- B1. Import single worktree by branch name
- B2. Picker shows unimported worktree branches with search/filter
- B3. Same mechanism as sync but for one worktree
- B4. Project context menu entry above "Sync Worktrees"
- B5. Command palette `ImportWorktree` action
- B6. Only for local projects
- B7. Post-import: if center pane has active terminal, prompt to move
- B8. If confirmed, move terminal via Feature A
- B9. If declined, activate new workspace without moving

## Scope Boundaries

- Terminal tabs only; editor and ACP chat tabs are out of scope
- Drag-and-drop between workspace tab bars is out of scope
- Managed terminal environment (ZDOTDIR, hooks, wrapper PATH) is not re-bootstrapped — the running shell keeps its environment, only UI workspace/project bindings are updated

## Context & Research

### Relevant Code and Patterns

- `crates/workspace/src/multi_workspace.rs` — `MultiWorkspace` struct, `add_workspace`, `activate_index`, `workspaces()`
- `crates/workspace/src/workspace.rs` — `move_active_item` (pane-to-pane move), `MoveItemToPane`, `Workspace::weak_handle()`, `Workspace::project()`
- `crates/workspace/src/pane.rs` — `add_item`, `remove_item`, tab context menu in `render_tab`, `pin_tab_entries`
- `crates/workspace/src/item.rs` — `added_to_pane` calls `added_to_workspace` on item when added to a new pane (line 743)
- `crates/terminal_view/src/terminal_view.rs` — `TerminalView::new`, `subscribe_for_terminal_events` (captures `WeakEntity<Workspace>` in closures), `added_to_workspace` (currently only updates `workspace_id`)
- `crates/superzent_ui/src/lib.rs` — `run_sync_project_worktrees`, `build_synced_local_workspace_entry`, `deploy_project_context_menu`, `open_local_workspace_path`
- `crates/superzent_git/src/lib.rs` — `discover_worktrees` (parses `git worktree list --porcelain`), `DiscoveredWorktree`
- `crates/picker/src/picker.rs` — `Picker<D: PickerDelegate>` pattern
- `crates/git_ui/src/branch_picker.rs` — Branch picker delegate example

### Institutional Learnings

- **Workspace handle resolution**: Prefer APIs that return `Entity<Workspace>` directly over re-resolving from `WorkspaceEntry` (from `managed-workspace-create-progress-toasts` solution)
- **Managed terminal environment**: Running terminals carry workspace-specific env vars (ZDOTDIR, hook bin dir). These are baked into the shell process and cannot be updated. The plan accepts this limitation — UI-level bindings are updated but the shell environment stays as-is
- **State transfer**: Do not bleed workspace-local lifecycle state (teardown overrides) across workspaces (from `managed-workspace-lifecycle-source-of-truth` solution)

## Key Technical Decisions

- **Rebind via `added_to_workspace` lifecycle**: Instead of creating a new `TerminalView`, enhance `TerminalView::added_to_workspace` to update `self.workspace`, `self.project`, and re-subscribe terminal events. The existing `add_item` → `added_to_pane` → `added_to_workspace` chain already fires when items are added to a pane, so rebinding happens automatically. Rationale: preserves all `TerminalView` state (scroll, blink, focus) and requires no `ItemHandle` trait changes.
- **`subscribe_for_terminal_events` re-invocation**: The function captures `WeakEntity<Workspace>` by move in closures. The only way to update is to drop old subscriptions and re-create them. `added_to_workspace` will replace `self._terminal_subscriptions` with a fresh call.
- **Action namespace**: Use `terminal_view` namespace for `MoveTerminalToWorkspace` since it is terminal-specific. Use `superzent_ui` namespace for `ImportWorktree` since it interacts with the sidebar/store layer.
- **Workspace picker reuse**: A single `WorkspaceMovePickerDelegate` serves both the command palette action and the tab context menu entry.
- **Import Worktree filters `discover_worktrees`**: Reuses existing `superzent_git::discover_worktrees` and filters by branch, rather than adding a new git command.

## Open Questions

### Resolved During Planning

- **How to get workspace entity in `added_to_workspace`?** `workspace.weak_handle()` returns `WeakEntity<Workspace>`, and `workspace.project().downgrade()` gives `WeakEntity<Project>`.
- **Where does the workspace picker live?** In `crates/terminal_view/` as a new module `workspace_move_picker.rs`, since the action is terminal-specific.
- **Where does the import worktree picker live?** In `crates/superzent_ui/src/` as a new module or inline in `lib.rs`, colocated with existing worktree sync code.

### Deferred to Implementation

- **Exact `subscribe_for_terminal_events` refactoring**: May need to adjust the function signature to accept `&mut Context<TerminalView>` when called from `added_to_workspace`. The current signature already takes this context type.
- **Tab context menu insertion point**: Multiple branches in pane.rs build the context menu. The implementer should verify which branches need the new entry.

## Implementation Units

- [x] **Unit 1: Enhance `TerminalView::added_to_workspace` to rebind workspace references**

**Goal:** Make cross-workspace terminal moves work by updating workspace/project bindings and event subscriptions when a terminal is added to a new workspace's pane.

**Requirements:** A3

**Dependencies:** None

**Files:**

- Modify: `crates/terminal_view/src/terminal_view.rs`
- Test: `crates/terminal_view/src/terminal_view.rs` (in `#[cfg(test)]` module)

**Approach:**

- In `added_to_workspace`, after the existing `workspace_id` update, add: set `self.workspace = workspace.weak_handle()`, set `self.project = workspace.project().downgrade()`, drop and re-create `self._terminal_subscriptions` via `subscribe_for_terminal_events` with the new workspace weak entity.
- The `window` parameter in `added_to_workspace` is currently `_: &mut Window` — rename to `window` since it is now needed for `subscribe_for_terminal_events`.

**Patterns to follow:**

- `TerminalView::new` (line 235) — same pattern of calling `subscribe_for_terminal_events` and storing the result
- Existing `added_to_workspace` (line 1701) — extend, don't replace

**Test scenarios:**

- Happy path: After calling `added_to_workspace` with a different workspace, `self.workspace` points to the new workspace and `self.project` points to the new project
- Happy path: `_terminal_subscriptions` is refreshed (old subscriptions dropped, new ones created)
- Edge case: `added_to_workspace` called with the same workspace — no-op behavior, subscriptions still valid

**Verification:**

- Terminal events (path hover, file open from terminal) resolve against the correct workspace after rebinding

---

- [x] **Unit 2: Cross-workspace terminal move function and workspace picker**

**Goal:** Implement the core move operation and a picker UI for selecting the target workspace.

**Requirements:** A1, A2, A4, A5, A6, A8, A9, A10

**Dependencies:** Unit 1

**Files:**

- Create: `crates/terminal_view/src/workspace_move_picker.rs`
- Modify: `crates/terminal_view/src/terminal_view.rs` (register action, add mod declaration)
- Modify: `crates/workspace/src/multi_workspace.rs` (add helper to get workspaces with names)

**Approach:**

- Define `MoveTerminalToWorkspace` action in terminal_view via `actions!` macro
- Create `WorkspaceMovePickerDelegate` implementing `PickerDelegate`:
  - Stores `source_pane: Entity<Pane>`, `item_to_move: Box<dyn ItemHandle>`, `multi_workspace: WindowHandle<MultiWorkspace>`
  - `update_matches`: filters workspace list (excluding current) by query using `fuzzy::match_strings`
  - `confirm`: removes item from source pane, adds to target workspace's `active_pane()`, activates target workspace via `multi_workspace.activate_index`
  - Each workspace entry shows workspace name (derived from worktree path or custom name)
- Register action handler on workspace: verifies active item is a `TerminalView`, checks 2+ workspaces, opens picker via `workspace.toggle_modal`
- When removing the terminal from the source pane, pass `close_pane_if_empty=false` to ensure the source workspace remains open with an empty pane per A9
- Note: cross-workspace removes fire `pane::Event::RemoveItem` on the source workspace's pane subscription — this is a different execution path than intra-workspace `move_active_item`. Verify serialization and other side effects triggered by `RemoveItem` behave correctly
- Add a public helper on `MultiWorkspace` (e.g., `workspace_entries_excluding`) that returns `(index, name, Entity<Workspace>)` tuples for all workspaces except the active one, so the picker can store target workspace entity references for use in the confirm handler

**Patterns to follow:**

- `crates/git_ui/src/branch_picker.rs` — picker delegate structure
- `crates/workspace/src/workspace.rs` `move_active_item` — remove + add pattern

**Test scenarios:**

- Happy path: active terminal tab moved to target workspace, source pane no longer contains it, target pane now has it, target workspace is active
- Happy path: workspace picker shows all workspaces except the current one
- Edge case: action does nothing when active item is not a TerminalView
- Edge case: action does nothing (or is hidden) when only 1 workspace exists
- Edge case: picker dismissed without selection — no move occurs

**Verification:**

- `MoveTerminalToWorkspace` from command palette opens picker listing other workspaces
- Selecting a workspace moves the terminal and switches to target

---

- [x] **Unit 3: Tab context menu "Move to Workspace" entry**

**Goal:** Add "Move to Workspace" to the tab right-click menu for terminal tabs.

**Requirements:** A5, A7

**Dependencies:** Unit 2

**Files:**

- Modify: `crates/terminal_view/src/terminal_view.rs` (implement `tab_extra_context_menu_actions`)

**Approach:**

- Extend the existing `tab_extra_context_menu_actions` implementation on TerminalView (which already returns "Rename" for non-task terminals at line 1607) to also include a "Move to Workspace" entry when 2+ workspaces exist. Append the new entry to the existing vec
- The entry dispatches `MoveTerminalToWorkspace` which opens the picker from Unit 2
- Access workspace count via `window.window_handle().downcast::<MultiWorkspace>()` → `workspaces().len()`
- This approach adds the entry via the existing per-item extension point rather than modifying generic pane.rs context menu code

**Patterns to follow:**

- Existing `tab_extra_context_menu_actions` implementations in the codebase (search for other items that override this method)

**Test scenarios:**

- Happy path: right-clicking a terminal tab shows "Move to Workspace" when 2+ workspaces exist
- Edge case: "Move to Workspace" not shown when only 1 workspace exists
- Edge case: "Move to Workspace" not shown for non-terminal tabs

**Verification:**

- Context menu entry appears for terminal tabs, opens picker, completes move

---

- [x] **Unit 4: Import Worktree picker and action**

**Goal:** Allow importing a single git worktree by branch name via command palette and project context menu.

**Requirements:** B1, B2, B3, B4, B5, B6

**Dependencies:** None (independent from Feature A)

**Files:**

- Create: `crates/superzent_ui/src/import_worktree_picker.rs` (or add inline in `lib.rs` — implementer decides based on size)
- Modify: `crates/superzent_ui/src/lib.rs` (add context menu entry, register action, add mod declaration)
- Modify: `crates/superzent_git/src/lib.rs` (optional: add `discover_worktrees_for_branch` helper)

**Approach:**

- Define `ImportWorktree` action in superzent_ui
- Create `ImportWorktreePickerDelegate` implementing `PickerDelegate`:
  - On construction: run `superzent_git::discover_worktrees` on background thread, filter out branches already open as workspaces (check `SuperzentStore::workspace_for_location`)
  - Show branch names in picker list with `fuzzy::match_strings` filtering
  - Allow direct text input (matches unimported branches)
  - `confirm`: call `build_synced_local_workspace_entry` for the selected worktree, `SuperzentStore::upsert_workspace`, then `open_local_workspace_path_and_resolve` (make it `pub(crate)`) to open in MultiWorkspace and obtain `Entity<Workspace>` directly for the post-import prompt
- Add "Import Worktree" to `deploy_project_context_menu` in superzent_ui, positioned above the existing "Sync Worktrees" entry
- Register `ImportWorktree` action on workspace for command palette access
- Guard both entry points with `ProjectLocation::Local` check (same as Sync Worktrees)

**Patterns to follow:**

- `run_sync_project_worktrees` (line 6349) — discovery + upsert + open flow
- `build_synced_local_workspace_entry` (line 7629) — creating workspace entries from discovered worktrees
- `deploy_project_context_menu` (line 4026) — context menu construction
- `crates/git_ui/src/worktree_picker.rs` — worktree picker with create/open

**Test scenarios:**

- Happy path: picker shows branches of worktrees not already imported as workspaces
- Happy path: selecting a branch imports it as a managed workspace and opens it
- Edge case: no unimported worktrees available — picker shows empty state message
- Edge case: selected worktree path no longer exists on disk — show error toast
- Error path: `discover_worktrees` fails (not a git repo, git not installed) — show error toast

**Verification:**

- "Import Worktree" in command palette and project context menu opens branch picker
- Selecting a branch creates and opens the workspace

---

- [x] **Unit 5: Post-import terminal move prompt**

**Goal:** After importing a worktree, offer to move the current center pane terminal to the new workspace.

**Requirements:** B7, B8, B9

**Dependencies:** Unit 2, Unit 4

**Files:**

- Modify: `crates/superzent_ui/src/import_worktree_picker.rs` (or `lib.rs` — wherever the import flow lives)

**Approach:**

- After successful import and workspace open, check if the source workspace's center active pane has an active `TerminalView` item
- If yes, show `window.prompt(PromptLevel::Info, "Move terminal?", Some("Move the active terminal to the new workspace?"), &["Keep Here", "Move Terminal"], cx)`
- If user selects "Move Terminal" (index 1): remove terminal from source pane, add to newly opened workspace's active pane (same mechanism as Unit 2's move)
- If user selects "Keep Here" (index 0) or dismisses: activate the new workspace without moving. Unit 5 owns workspace activation for the B9 path (not Unit 2, which only activates as part of terminal moves)
- Obtain the target `Entity<Workspace>` directly from the open result (per institutional learning about handle resolution)

**Patterns to follow:**

- `window.prompt` usage in existing managed workspace flows (e.g., dirty-workspace confirmation in `spawn_new_workspace_request`)
- `open_local_workspace_path_and_resolve` (line 6988) — returns `Entity<Workspace>` directly

**Test scenarios:**

- Happy path: after import with active terminal in center pane, dialog appears; confirming moves terminal to new workspace
- Happy path: declining the dialog activates new workspace without moving terminal
- Edge case: no terminal in center pane — no dialog, just activate new workspace
- Edge case: active item is a terminal but in dock (TerminalPanel) — no dialog (only center pane terminals trigger prompt)

**Verification:**

- Full flow: Import Worktree → select branch → workspace opens → terminal move dialog → confirm → terminal moves to new workspace

## System-Wide Impact

- **Interaction graph:** `TerminalView::added_to_workspace` now does real work (rebind) instead of just updating DB id. Any code path that adds a terminal to a pane will trigger this — verify that adding a terminal to a pane in the _same_ workspace (normal open flow) still works correctly with the rebind logic.
- **Error propagation:** Import worktree failures show toasts. Terminal move failures (e.g., target workspace closed between picker open and confirm) should be handled gracefully — check that the item isn't lost.
- **State lifecycle risks:** Between removing an item from the source pane and adding it to the target pane, the item must be held by a strong reference to prevent entity drop.
- **Unchanged invariants:** `move_item` and `move_active_item` (intra-workspace pane moves) are not modified.

## Risks & Dependencies

| Risk                                                                | Mitigation                                                                                                                                          |
| ------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------- |
| `added_to_workspace` rebind breaks normal terminal-to-pane add      | Guard rebind with a check: only update workspace/project refs and re-subscribe if `self.workspace` entity differs from the workspace being added to |
| Terminal move loses the item if target workspace is closed mid-move | Hold strong `Entity<TerminalView>` reference through the entire move operation; if add-to-target fails, re-add to source                            |
| Managed terminal hooks/env degrade after cross-workspace move       | Accepted limitation — document in release notes that managed terminal environment stays from the original workspace                                 |

## Sources & References

- **Origin document:** [docs/brainstorms/2026-04-15-move-terminal-tab-to-workspace-requirements.md](docs/brainstorms/2026-04-15-move-terminal-tab-to-workspace-requirements.md)
- Related code: `crates/workspace/src/workspace.rs` `move_active_item`, `crates/terminal_view/src/terminal_view.rs`, `crates/superzent_ui/src/lib.rs`
- Institutional learnings: `docs/solutions/ui-bugs/managed-workspace-create-progress-toasts-can-fail-to-appear-after-local-open-2026-04-12.md`, `docs/solutions/integration-issues/managed-zsh-terminals-can-lose-codex-and-claude-wrapper-resolution-after-shell-startup-2026-04-07.md`
