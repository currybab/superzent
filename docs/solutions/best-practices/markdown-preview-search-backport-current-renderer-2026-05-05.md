---
title: "Backport markdown preview search against the current renderer"
date: "2026-05-05"
category: best-practices
module: markdown_preview
problem_type: best_practice
component: tooling
severity: medium
applies_when:
  - "Backporting an upstream feature whose implementation assumes a newer renderer or entity model"
  - "Adding search support to a non-editor GPUI item through SearchableItem"
  - "Searching rendered markdown without implementing preview text selection"
tags:
  - markdown-preview
  - search
  - upstream-backport
  - searchable-item
  - gpui
  - renderer-boundary
---

# Backport markdown preview search against the current renderer

## Context

Upstream Zed commit `fd4d8444cf` added markdown preview search, but its patch assumed the newer `Markdown` entity rendering path. Superzent's markdown preview still renders parsed markdown blocks through `MarkdownPreviewView` and `RenderContext`, so a direct cherry-pick would have turned a scoped search feature into a renderer migration.

Session history for the `markdown-preview-search` worktree confirmed this started from the upstream-sync plan's deferred markdown-preview-search item. The useful pattern was to preserve the current renderer and adapt only the search behavior: `SearchableItem` integration, rendered-text match extraction, highlight composition, active-match scrolling, and keymap wiring.

## Guidance

When backporting a feature from upstream into an older or intentionally different rendering path, separate the user-facing contract from the upstream internal architecture.

For markdown preview search, the contract was:

- Cmd/Ctrl-F deploys the existing buffer search bar while preview is focused.
- The preview implements `SearchableItem`.
- Search candidates are rendered text and code block contents, not raw markdown syntax.
- Matches are highlighted in rendered inline text and code blocks.
- Next/previous navigation scrolls the matching preview block into view.
- Replacement, text drag selection, and `select_matches` remain out of scope.

The implementation that fit Superzent's current renderer used three local boundaries:

- `crates/project/src/search.rs`: add `SearchQuery::search_str` so non-buffer surfaces can reuse existing query semantics.
- `crates/markdown_preview/src/markdown_preview_view.rs`: extract rendered search segments and implement `SearchableItem` on the preview item.
- `crates/markdown_preview/src/markdown_renderer.rs`: pass search matches through `RenderContext` and combine search backgrounds with existing syntax, link, and inline-code highlights.

The important detail is that matches are stored with both a preview block and a source-backed rendered segment:

```rust
pub struct MarkdownSearchMatch {
    pub(crate) block_index: usize,
    pub(crate) text_source_range: Range<usize>,
    pub(crate) text_range: Range<usize>,
}
```

That lets navigation reveal the right virtual-list row while rendering applies highlights only to the `InteractiveText` or code block whose source range produced the match.

## Why This Matters

Directly importing upstream code across a renderer boundary tends to pull in unrelated architecture. In this case, the upstream behavior was small, but the upstream implementation depended on a different markdown rendering model. Adapting the contract preserved the current Superzent renderer, kept the PR reviewable, and avoided accidental work on text selection or drag handling.

It also made the non-goals explicit. Markdown preview search can support highlighting and navigation without pretending it supports editor-like selection. Keeping `selection: false`, `replacement: false`, and a no-op `select_matches` prevents the search UI from advertising behavior the preview cannot perform safely yet.

## When to Apply

- An upstream PR is valuable but assumes a newer component architecture than the current branch has.
- A GPUI item should integrate with existing search UI without becoming an editor.
- The rendered surface differs from source text and search should match what users see.
- Implementing selection, copy, or drag behavior would require new state and input handling beyond the intended scope.

## Examples

Prefer rendered-segment extraction over searching raw markdown source:

```rust
fn search_segments_for_paragraph(
    paragraph: &MarkdownParagraph,
    block_index: usize,
    segments: &mut Vec<MarkdownSearchSegment>,
) {
    for chunk in paragraph {
        if let MarkdownParagraphChunk::Text(text) = chunk {
            segments.push(MarkdownSearchSegment {
                block_index,
                text_source_range: text.source_range.clone(),
                text: text.contents.to_string(),
            });
        }
    }
}
```

Prefer highlight composition over replacing existing text styling:

```rust
let highlights = gpui::combine_highlights(
    gpui::combine_highlights(syntax_highlights, region_highlights),
    search_highlights,
);
```

Cover the renderer boundary in tests. The search segment test should include text-bearing nested structures such as lists, block quotes, tables, and code blocks, and it should also assert that non-text blocks such as images, horizontal rules, and mermaid diagrams do not create search candidates.

## Related

- Plan: `docs/plans/2026-05-05-001-feat-markdown-preview-search-plan.md`
- Origin requirements: `docs/brainstorms/2026-04-23-upstream-sync-editor-search-requirements.md`
- Prior deferred item: `docs/plans/2026-04-23-001-feat-upstream-editor-search-sync-plan.md`
- Upstream commit: `fd4d8444cf markdown_preview: Add search support to markdown preview (#52502)`
