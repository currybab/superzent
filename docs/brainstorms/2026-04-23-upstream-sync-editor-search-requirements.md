---
date: 2026-04-23
topic: upstream-sync-editor-search
---

# Upstream Sync: Editor and Search

## Problem Frame

Superzent is about one month behind upstream Zed and should selectively backport improvements that make everyday editing, search, terminal, and picker usage better. A full upstream merge is out of scope because Superzent has product-specific behavior around single-window workspace management, managed workspaces, default-build next-edit, and avoiding the broader upstream hosted AI/chat surface in the default build.

The first sync phase should use cherry-picks, not a broad merge. It should prioritize user-visible editor/search quality while preserving Superzent's current product model.

## Requirements

**Selection Strategy**

- R1. Phase 1 must use upstream commit or PR-level cherry-picks rather than merging all of `upstream/main`.
- R2. Phase 1 must include both low-risk bug fixes and selected editor/search feature improvements.
- R3. Phase 1 must exclude broad upstream agent/sidebar/chat architecture rewrites.
- R4. Any selected commit that touches default-build AI, hosted model setup, ACP tabs, or chat surfaces must be reviewed against Superzent's existing next-edit and `full` feature split before inclusion.

**Recommended Cherry-Pick Waves**

- R5. Wave A should apply small terminal/search/editor correctness fixes first, because they provide value with low conflict risk.
- R6. Wave B should apply search and picker improvements that touch shared project/search infrastructure.
- R7. Wave C should evaluate larger feature backports separately: fuzzy picker matching, Bookmarks, Code Lens, and generic navigation overlays.
- R8. Wave C candidates may be deferred individually if they introduce persistence, settings, LSP, workspace, or build-feature conflicts.

**Candidate Coverage**

- R9. Search candidates should include replace/search correctness fixes, project search stale result fixes, symbol picker UTF-8 safety, markdown-preview search support, and non-ASCII replace-all hang fixes.
- R10. Editor candidates should include folding, sticky header, hover, completion undo, selection, formatting, block-comment, Bookmarks, Code Lens, and navigation overlay improvements where they can be backported cleanly.
- R11. Terminal candidates should include small terminal input, focus, cursor, path detection, process cleanup, and combining-mark fixes.
- R12. Picker candidates should include fuzzy matching improvements only after confirming the new `fuzzy_nucleo` crate and downstream picker changes fit Superzent's current workspace and file-finder behavior.

## Initial Candidate Shortlist

**Wave A: Small, High-Confidence Fixes**

- `e5dc2f06c9` search: Fix replace all being silently dropped (#50852)
- `4b1a2f3ad8` search: Fix focus replacement field when opening replace (#51061)
- `7d3ccce952` Don't auto-close in search (#52553)
- `6184b2457c` Fix project symbol picker UTF-8 highlight panic (#53485)
- `c97442029a` Fix hang in replace all with non-ASCII text and regex-special characters (#54422)
- `ce7512b115` Fix terminal rename from context menu on inactive tabs (#50031)
- `00c771af0a` terminal: Properly apply focus when switching via tabbing hotkey (#53127)
- `9c5f3b10fd` terminal_view: Reset cursor blink on send actions (#53171)
- `d5fd199719` Fix terminal path detection inside parentheses (#52222)
- `62f020312c` terminal: Send SIGTERM synchronously on terminal drop (#53107)
- `7d7ec655e7` terminal_view: Show hollow cursor when bar/underline is unfocused (#53713)
- `5c5727c90a` Replace terminal ctrl `SendText` keybinds with `SendKeystroke` (#51728)
- `71f5dbdf26` Fix ctrl-delete keybind in terminal (#51726)
- `2c49900c6a` terminal: Fix heredoc commands failing in agent shell (#49106)
- `debf4c9988` Fix terminal combining marks (#53176)

**Wave B: Medium Search/Editor Improvements**

- `b0e35b6599` Allow search/replace to span multiple lines (#50783)
- `0238d2d180` search: Fix deleted files persisting in project search results (#50551)
- `43867668f4` Add query and search options to `pane::DeploySearch` action (#47331)
- `fd4d8444cf` markdown_preview: Add search support to markdown preview (#52502)
- `c7870cb93d` editor: Add align selections action (#44769)
- `735eb4340d` editor: Fix folding for unindented multiline strings and comments (#50049)
- `d94aa26ac5` editor: Fix multi-line cursor expansion with multi-byte characters (#51780)
- `ed42b806b1` editor: Fix Accessibility Keyboard word completion corruption (#50676)
- `0640e550b8` editor: Merge additional completion edits into primary undo transaction (#52699)
- `b7f166ab40` Fix `FormatSelections` to only format selected ranges (#51593)
- `3a6faf2b4a` editor: Deduplicate sticky header rows (#52844)
- `525f10a133` editor: Add action to toggle block comments (#48752)
- `f4addb6a24` editor: Make multiline comment folding more robust (#54102)
- `aa14c4201b` terminal_view: Don't try `home_dir` when working locally (#53071)

**Wave C: Larger Feature Backports**

- `93438829c7` Add `fuzzy_nucleo` crate for order independent file finder search (#51164)
- `722f3089ed` fuzzy_nucleo: Optimize path matching with CharBag prefilter (#54112)
- `68541960a7` fuzzy_nucleo: Add strings module and route several pickers through it (#54123)
- `81b16f464c` fuzzy_nucleo: Fix out of range panic (#54371)
- `73126dcb81` editor: Introduce Bookmarks (#54174)
- `76883bb983` Support Code Lens in the editor (#54100)
- `0800c007c4` editor: Add generic navigation overlays (#52630)
- `497b6de85f` editor: Add configurable hover delay (#53504)

## Success Criteria

- Phase 1 improves editor/search/terminal usability without adopting unrelated upstream product direction.
- Cherry-picks are applied in waves so small correctness fixes can land even if larger feature backports are deferred.
- Superzent's default build still keeps next-edit separate from broader upstream AI/chat surfaces.
- Single-window and managed-workspace behavior remains intact.
- Each accepted wave builds and passes focused tests for the touched crates or features.

## Scope Boundaries

- Do not merge all upstream changes.
- Do not pull upstream agent/sidebar/chat rewrites into Phase 1.
- Do not change Superzent's default build to include upstream hosted AI/chat behavior.
- Do not treat Bookmarks, Code Lens, or fuzzy picker changes as mandatory if they conflict heavily.
- Do not modify `.rules` or project policy files as part of this sync phase.

## Key Decisions

- Cherry-pick over merge: this preserves Superzent's product-specific behavior and avoids absorbing unrelated upstream architecture churn.
- Work in waves: this reduces the risk that a large feature backport blocks smaller fixes.
- Keep chat/agent UI for Phase 2: upstream agent/sidebar changes are too coupled to include in the first sync phase.

## Dependencies / Assumptions

- Current merge-base with `upstream/main` is `be3a5e2c061823c5fa4de56e4eec58e68319a0ac` from 2026-03-18.
- `upstream/main` was fetched on 2026-04-23 before candidate selection.
- Larger feature candidates may require additional dependency commits not listed here; planning should verify each candidate by attempting cherry-picks in wave order.

## Outstanding Questions

### Deferred to Planning

- [Affects R5-R12][Technical] Which Wave A and Wave B commits cherry-pick cleanly onto current Superzent, and which require manual adaptation?
- [Affects R7-R8][Needs research] Do Bookmarks and Code Lens conflict with Superzent's workspace persistence, status bar/footer merge, or default feature split?
- [Affects R12][Needs research] Does `fuzzy_nucleo` change picker behavior in a way that affects Superzent's workspace switcher, import-worktree picker, or managed workspace flows?

## Next Steps

-> `/ce:plan` for structured implementation planning.
