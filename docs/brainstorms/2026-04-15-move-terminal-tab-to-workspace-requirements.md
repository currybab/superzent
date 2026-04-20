---
date: 2026-04-15
topic: move-terminal-tab-to-workspace-and-import-worktree
---

# Move Terminal Tab to Workspace & Import Worktree

## Problem Frame

Superzent supports multiple workspaces in a single window via `MultiWorkspace`, but there is no way to move a tab from one workspace to another. Within a workspace, tabs can already be moved between panes (`MoveItemToPane`, `MoveItemToPaneInDirection`), but cross-workspace movement is not supported. Users who open a terminal in one workspace and later want it in a different workspace must close and re-create the terminal session, losing shell state.

Separately, when using multiple AI coding agents (Claude Code, Codex, etc.) that create git worktrees automatically, users need to import specific worktrees into Superzent. The existing "Sync Worktrees" discovers and imports all worktrees at once, which includes unwanted ones. A targeted "Import Worktree" by branch name solves this.

These features connect: after importing a worktree, users often want to move their current terminal to the new workspace.

## Scope

- **In scope**: Terminal tab cross-workspace move, Import Worktree by branch name, post-import terminal move prompt.
- **Out of scope**: Editor tabs, ACP chat tabs (future extension), drag-and-drop between workspace tab bars.

## Requirements

### Feature A: Move Terminal Tab to Workspace

**Core Move Operation**

- A1. A terminal tab can be moved from any workspace to any other open workspace within the same window.
- A2. The move removes the tab from the source workspace and adds it to the target workspace's center active pane.
- A3. The terminal process, session state, and all functional bindings (workspace/project entity references, event subscriptions) must be correctly re-established for the target workspace after the move.
- A4. After the move, the target workspace becomes the active workspace and the moved tab is activated.
- A5. The action must only be available when there are 2+ workspaces open. When only one workspace exists, the action should be hidden or disabled.

**Entry Points**

- A6. Command palette: a `MoveTerminalToWorkspace` action that opens a workspace picker.
- A7. Tab context menu: a "Move to Workspace" entry positioned below "Pin Tab" that opens the same workspace picker.
- A8. Both entry points show a list of other open workspaces (excluding the current one) and the user selects the target.

**Edge Cases**

- A9. If the source workspace has only one tab and that tab is moved, the source workspace remains open with an empty pane (consistent with existing close-tab behavior).
- A10. If the moved terminal tab is the active tab in the source, the source workspace activates the next available tab per existing tab-close ordering (or shows empty pane).

### Feature B: Import Worktree

**Core Import Operation**

- B1. Users can import a single git worktree by selecting its branch name, without importing all worktrees.
- B2. A picker shows available git worktree branch names (from `git worktree list`) that are not already open as workspaces, with search/filter support and direct text input.
- B3. On selection, the worktree is imported as a new managed workspace under the current project — same mechanism as `run_sync_project_worktrees` but for a single worktree.

**Entry Points**

- B4. Workspace sidebar: project right-click context menu, "Import Worktree" entry positioned above "Sync Worktrees".
- B5. Command palette: an `ImportWorktree` action that opens the same branch picker.
- B6. Only available for local projects (same guard as Sync Worktrees).

**Post-Import Terminal Move**

- B7. After a worktree is successfully imported, if there is an active terminal tab in the center pane, show a confirmation dialog asking whether to move it to the newly imported workspace.
- B8. If the user confirms, the terminal tab is moved using Feature A's mechanism and the new workspace is activated.
- B9. If the user declines, the new workspace is activated without moving the terminal.
