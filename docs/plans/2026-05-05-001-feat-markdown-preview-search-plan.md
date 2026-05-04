---
title: "Add markdown preview search support"
type: feat
status: active
date: 2026-05-05
origin: docs/brainstorms/2026-04-23-upstream-sync-editor-search-requirements.md
---

# Add Markdown Preview Search Support

## Overview

Backport the useful behavior from upstream Zed commit `fd4d8444cf` (`markdown_preview: Add search support to markdown preview (#52502)`) onto Superzent's current markdown preview renderer. The upstream patch targets Zed's newer `Markdown` entity rendering path; this branch still renders parsed markdown blocks through `crates/markdown_preview/src/markdown_renderer.rs`, so the implementation needs to preserve the current renderer and add only the thin search integration layer.

## Problem Frame

Markdown preview currently behaves like a focused item but not a searchable item: Cmd/Ctrl-F does not give users in-preview search over rendered markdown. The broader upstream sync plan deferred this commit because a direct cherry-pick would pull in a renderer refactor. This follow-up implements the same user-facing search behavior against the existing parsed-block/list renderer instead of changing the preview architecture.

## Requirements Trace

- R1. Cmd/Ctrl-F in `MarkdownPreview` opens the existing buffer search UI.
- R2. Markdown preview implements `SearchableItem` so the existing search bar can find, count, and navigate matches.
- R3. Search candidates include rendered text chunks and code block contents.
- R4. Search highlights render in inline markdown text and code blocks without corrupting syntax, link, or inline-code styling.
- R5. Next/previous search navigation scrolls the matching preview block into view and marks the active result distinctly.
- R6. Search uses existing `SearchQuery` semantics for plain text, case handling, whole-word matching, and regex matching.
- R7. The backport remains scoped to search/highlight/navigation and does not add markdown preview text selection or drag interactions.

## Scope Boundaries

- Do not refactor markdown preview to upstream's newer `Markdown` entity rendering model.
- Do not implement preview text drag selection, actual selection ranges, copy selection, or drag-and-drop interactions.
- Do not implement replacement in markdown preview.
- Do not make `SearchableItem::select_matches` behave like editor selection in this phase.
- Do not change general editor or project-search behavior beyond a reusable string-search helper.

## Context & Research

### Relevant Code and Patterns

- `crates/markdown_preview/src/markdown_preview_view.rs` owns the preview item, focus handle, parsed markdown contents, list state, selected block, and `Item` integration.
- `crates/markdown_preview/src/markdown_renderer.rs` renders parsed markdown blocks into GPUI elements and already combines syntax, link, and inline-code styles for `InteractiveText`.
- `crates/project/src/search.rs` owns `SearchQuery`, which already represents text and regex search options for buffer/project search.
- `assets/keymaps/default-macos.json`, `assets/keymaps/default-linux.json`, `assets/keymaps/default-windows.json`, and `assets/keymaps/vim.json` define the `MarkdownPreview` key context bindings.
- Existing `SearchableItem` implementations in `crates/terminal_view/src/terminal_view.rs`, `crates/language_tools/src/lsp_log_view.rs`, and `crates/debugger_tools/src/dap_log.rs` show the search-bar integration contract.

### Institutional Learnings

- `docs/plans/2026-04-23-001-feat-upstream-editor-search-sync-plan.md` deferred `fd4d8444cf` because the direct upstream patch assumed a different markdown rendering path.
- Repo guidance requires focused changes, no drive-by `.rules` edits, and `./script/clippy` instead of raw `cargo clippy`.

### External References

- Upstream commit: `fd4d8444cf markdown_preview: Add search support to markdown preview (#52502)`.

## Key Technical Decisions

- **Adapt the behavior, not the upstream renderer:** Preserve Superzent's current parsed markdown renderer and carry only the search contract, matching logic, keymaps, and highlighting behavior that fit the current code.
- **Search rendered segments rather than source markdown:** Extract searchable segments from headings, paragraphs, lists, block quotes, tables, and code blocks so users search what they see instead of markdown formatting syntax.
- **Track matches by block and source-backed text segment:** Store `block_index`, `text_source_range`, and in-segment `text_range` so navigation can reveal the right virtual list row and rendering can apply highlights to the correct `InteractiveText` or code block.
- **Keep selection explicitly out of scope:** `select_matches` remains a no-op and `supported_options().selection` remains false because preview selection requires separate state and mouse handling.
- **Centralize plain-string search in `SearchQuery`:** Add `SearchQuery::search_str` so markdown preview can reuse existing search semantics without creating a markdown-specific query engine.

## Implementation Units

- [ ] **U1: Add reusable `SearchQuery::search_str` helper**

  **Goal:** Let non-buffer surfaces search plain strings with the same query semantics as existing search UI options.

  **Requirements:** R2, R6

  **Files:**

  - Modify: `crates/project/src/search.rs`

  **Approach:**

  Add a synchronous helper that returns byte ranges in an input `&str` for both text and regex queries. Preserve whole-word filtering for text queries and line-by-line behavior for non-multiline regex queries.

  **Patterns to follow:**

  - Existing `SearchQuery` enum behavior in `crates/project/src/search.rs`.

  **Test scenarios:**

  - Text search finds multiple matches and excludes embedded whole-word false positives.
  - Regex search respects one-match-per-line behavior.
  - Empty queries return no ranges.

  **Verification:**

  - `cargo test -p project search_str`

- [ ] **U2: Make markdown preview searchable**

  **Goal:** Integrate `MarkdownPreviewView` with the workspace search bar and maintain match state for counts and navigation.

  **Requirements:** R1, R2, R3, R5, R7

  **Files:**

  - Modify: `crates/markdown_preview/Cargo.toml`
  - Modify: `crates/markdown_preview/src/markdown_preview_view.rs`
  - Modify: `Cargo.lock`

  **Approach:**

  Implement `SearchableItem` for `MarkdownPreviewView`, expose the item via `Item::as_searchable`, store matches on the view, invalidate matches after markdown reparses, and scroll the virtual list to the active match block during navigation.

  **Patterns to follow:**

  - Existing `SearchableItem` behavior in `crates/terminal_view/src/terminal_view.rs`.
  - Existing markdown preview `ListState` row navigation in `crates/markdown_preview/src/markdown_preview_view.rs`.

  **Test scenarios:**

  - Search segments include headings, paragraphs, list item text, block quote text, table text, and code block contents.
  - Images, horizontal rules, and mermaid diagrams do not produce searchable text segments.
  - `select_matches` and replacement remain no-ops.

  **Verification:**

  - `cargo test -p markdown_preview search_segments_include_rendered_text_and_code_blocks`

- [ ] **U3: Render search highlights in markdown text and code blocks**

  **Goal:** Show all matches and the active match with existing theme colors while preserving current rendered markdown styling.

  **Requirements:** R4, R5

  **Files:**

  - Modify: `crates/markdown_preview/src/markdown_renderer.rs`

  **Approach:**

  Pass search state through `RenderContext`, map each match to highlights for its source-backed text segment, and combine those highlights with syntax, inline-code, and link styling.

  **Patterns to follow:**

  - Current `gpui::combine_highlights` usage in markdown text rendering.
  - Theme search colors from `theme.colors().search_match_background` and `search_active_match_background`.

  **Test scenarios:**

  - Inline markdown text still renders syntax/link/inline-code styles with search backgrounds layered in.
  - Code blocks preserve syntax highlights while adding search backgrounds.
  - Active match uses a distinct background from inactive matches.

  **Verification:**

  - Focused compile/test of `markdown_preview`.
  - Manual preview smoke test if a local UI run is available.

- [ ] **U4: Wire markdown preview keymaps and finish verification**

  **Goal:** Ensure normal platform search shortcuts deploy the search bar while preview has focus.

  **Requirements:** R1

  **Files:**

  - Modify: `assets/keymaps/default-macos.json`
  - Modify: `assets/keymaps/default-linux.json`
  - Modify: `assets/keymaps/default-windows.json`
  - Modify: `assets/keymaps/vim.json`

  **Approach:**

  Add `buffer_search::Deploy` bindings under the existing `MarkdownPreview` key context for macOS, Linux, Windows, and Vim modes.

  **Patterns to follow:**

  - Existing `MarkdownPreview` scroll bindings in the same keymap sections.

  **Test scenarios:**

  - Cmd-F works in macOS preview context.
  - Ctrl-F/find works in Linux and Windows preview contexts.
  - Vim `/` deploys search while preview is focused.

  **Verification:**

  - Keymap JSON remains valid.
  - Final local checks include focused unit tests and `./script/clippy` if feasible.

## System-Wide Impact

- **Search UI:** Reuses the existing buffer search bar and search event model instead of adding a separate preview search UI.
- **Preview rendering:** Adds highlight state to rendering context but does not change markdown parsing or block layout.
- **Performance:** Search runs on extracted text segments in a background task. Large markdown files still avoid blocking the foreground thread during query execution.
- **Error handling:** Search helper returns empty results for empty queries and does not introduce fallible operations.

## Risks & Dependencies

| Risk | Mitigation |
| --- | --- |
| Direct upstream patch diverges from current renderer | Adapt only the behavior to current `ParsedMarkdown` and `RenderContext` structures |
| Highlight byte ranges drift from rendered text | Store ranges relative to each extracted text segment and render highlights only when the segment source range matches |
| Users expect text selection from search | Keep `selection: false`, document `select_matches` as out of scope, and do not present selection UI |
| Search helper subtly diverges from buffer search semantics | Keep tests around text whole-word and regex per-line behavior |

## Sources & References

- Origin requirements: `docs/brainstorms/2026-04-23-upstream-sync-editor-search-requirements.md`
- Prior plan deferral: `docs/plans/2026-04-23-001-feat-upstream-editor-search-sync-plan.md`
- Upstream source commit: `fd4d8444cf markdown_preview: Add search support to markdown preview (#52502)`
