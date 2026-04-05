---
title: Getting Started with superzent
description: Get started with superzent, the local-first workspace shell for coding agents.
---

# Getting Started

`superzent` is a local-first shell for working across repositories, worktrees, terminals, diffs, and editor panes in one native app window.

## Quick Start

### 1. Open the app and add a repository

The main window is built around multiple local workspaces. Use the welcome page or sidebar controls to open an existing repository, then create or switch worktrees from there.

### 2. Learn the core shortcuts

| Action                      | macOS          |
| --------------------------- | -------------- |
| Command palette             | `Cmd+Shift+P`  |
| Open file                   | `Cmd+P`        |
| Search symbols              | `Ctrl+Shift+T` |
| New terminal in center pane | `Cmd+T`        |
| Toggle terminal panel       | `` Ctrl+` ``   |
| Open settings               | `Cmd+,`        |

### 3. Work from terminals first

The default `superzent` workflow expects you to run external coding agents and local tooling from terminals.

- Use `Cmd+T` for a center-pane terminal tab
- Use `` Ctrl+` `` to show or hide the terminal panel
- Keep diffs, files, and terminals visible at the same time instead of constantly context switching

### 4. Tune the workspace shell

Good first settings:

- theme and font
- startup workspace behavior
- keybindings
- project-specific tasks

See [Running & Testing](./running-testing.md), [Terminal](./terminal.md), and [All Settings](./reference/all-settings.md).

## Current Product Scope

The default build is intentionally narrow:

- local repositories and worktrees
- native editor, panes, and diffs
- terminal-driven agent workflows
- center-pane footer next-edit in regular editor buffers via non-Zed-hosted providers

Not part of the current public shell surface:

- collaboration
- hosted AI surfaces
- Zed's own agent panel and text threads
- public Windows and Linux desktop releases

## Public Release Availability

The first public binary release is currently macOS Apple Silicon only. Other platforms are still source-build territory.
