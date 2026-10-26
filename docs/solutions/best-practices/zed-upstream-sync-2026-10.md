---
title: "Zed upstream sync through 2026-10-09"
date: "2026-10-11"
category: best-practices
module: upstream-sync
problem_type: best_practice
component: tooling
severity: medium
applies_when:
  - "Starting the next zed-industries/zed upstream sync"
  - "Checking whether an upstream fix was already ported, skipped on purpose, or still pending"
tags:
  - upstream
  - zed
  - porting
---

# Zed upstream sync through 2026-10-09

## Where this sync stopped

- The fork's merge-base with upstream is still `be3a5e2c06` (2026-03-18). Fixes are ported by cherry-picking; upstream is never merged.
- This sync triaged every upstream commit up to **`f16f9652ec` (2026-10-09)**. Start the next sync from there: `git log f16f9652ec..upstream/main`.
- Ported in two PRs:
  - `port/security-deps` (#56) updated wasmtime 36.0.17, cap-std, async-tar 0.6, rustls, and others with published advisories.
  - `port/october-crash-fixes` added 137 upstream PRs: crashes, hangs, data loss, and editor/project/git correctness.
- Earlier ports are listed by PR number in `git log` (`git log --oneline be3a5e2c06..HEAD | grep -oE '#[0-9]{5}'`).

## How the triage was done

1. Exclude PR numbers already in the fork's history.
2. Bucket the remaining upstream commits by top crate, and skip remote/SSH/collab, Windows/Linux-only, vim, edit-prediction, and debugger changes.
3. For each candidate:
   - check that the pre-fix code exists in fork HEAD;
   - if it fixes a regression, run `git merge-base --is-ancestor <regression-sha> HEAD` to confirm the fork has the regression;
   - judge conflicts with `git merge-tree --write-tree --merge-base=<sha>^ HEAD <sha>`.
4. Cherry-pick with `-x`, oldest first. Commits adapted by hand keep the upstream author and get a `Superzent:` note in the message.
5. Before trusting a test failure, compare against `origin/main`. Some project, worktree, and workspace tests fail or hang on main too.

## Traps found during this sync

- **Upstream refactors the fork lacks:**

  - multibuffer excerpt rework #52364;
  - editor.rs split into `input.rs`, `navigation.rs`, `completions.rs`, `element/mouse.rs`, etc.;
  - markdown preview rewrite #52008 (ported on 2026-10-25, see below);
  - libgit2 removal #53453;
  - ACP v2;
  - project groups and threads sidebar.

  Fixes for code in these areas usually don't apply, or need porting into the fork's older layout.

- **Conflict hunks that pull in unrelated upstream code:** when resolving a test conflict by taking "theirs", extract only the tests the commit adds. Otherwise, tests for behavior the fork lacks come along and fail.
- **disable_ai gates:** the fork removes `disable_ai` gates from context servers. Upstream hunks that add them back must be dropped.
- **Missing APIs and different names:** new upstream tests often use helpers the fork doesn't have:

  - `DiagnosticEntry::new`, `HighlightId::new`, `SaveOptions::force_format`;
  - `flex_grow_1()`, `theme_settings`, `is_linked_worktree`.

  Port these tests to the fork's equivalents.

- **Session restore is only a fallback here:** Superzent's startup reopens the workspaces in its own store by path (`restore_superzent_startup_workspace` in `zed/src/main.rs`). Session-based restore (`restorable_workspaces`, `restore_multiworkspace`) runs only when that path fails. Upstream fixes for session bindings, session restore, or scratch (path-less) workspaces rarely change what users see, so trace the Superzent startup path before porting them.
- **A clean merge-tree can still fail to compile:** #52692 merged cleanly but used `SaveOptions::force_format`. Build every cherry-pick, not only the conflicted ones.
- **Property tests need more cases to fail:** #52871's `single_pane_navigation` passed without its fix at the default case count. Run it with `PROPTEST_CASES=3000` to confirm it fails without the fix.

## Skipped or dropped on purpose

- #59087 `346d3605cf`: fork default_branch has include_remote_name variant
- #59069 `5e514f4624`: diverged git_binary plumbing in remote branch check (perf only)
- #59044 `7726682898`: fork's compute_snapshot was reshaped when porting #57292 (perf only)
- #60205 `d1e8c0b50f`: needs upstream reinitialize_local_backend path (perf only)
- #52849 `b2db24e58a`: MCP multi-root fix entangled with upstream OAuth and ai_disabled state
- #61365 `ac44e9f5df`: depends on skipped #52849 MCP rework
- #62026 `b5764581d2`: depends on skipped #52849 MCP rework
- #62921 `907ed09c9f`: read-only autosave fix is a 7-file capability-event refactor; needs hand port
- #64659 `532532bb80`: out-of-bounds inlay hint filter conflicts in 3 files; fork's inlay hint pipeline differs
- #51244 `4d13f01c89`: MCP refresh on worktree changes recalculates agent tools and breaks 12 agent tests without the rest of the upstream MCP series
- #61562 `90b1549310`: per-chunk inlay hint replacement; fork cancels the in-flight fetch on refresh, so the upstream race (and its test) can't be reproduced
- #52158 `6d09eded79`: middle-click bracketed paste only runs on Linux/FreeBSD
- #56715 `df37766060` (and its follow-up #61325 `37b9fbf22b`): fixes jitter introduced by #47195, which the fork doesn't have
- #60960 `3f57e8d17d`: already in the fork as `ea0676ed3c`
- #54224 `1ea6eb4d42`: the scratch-workspace deletion it fixes does happen in the fork, but Superzent never restores path-less workspaces, so the deleted rows were never shown again. The port touches 8 files.
- #53362: written against the project-group restore (#53032, #53217), which the fork lacks. The fork's startup path already falls back per workspace.
- #60477 `99b0ed6b55` (and its follow-up #63213 `279da63882`): the red-X session loss only affects session-based restore, which Superzent's startup doesn't use.
- #52671 `f83313d0e0` and its replacement #65546 `8231a43e45`: fix tab-switcher selection changing on mouse hover. The fork's picker doesn't select on hover, since #50827 isn't in the fork.

## Ported after this record

- #60067 `f5c975162c`: Cmd-click opens terminal links while a TUI (e.g. Claude Code) has mouse reporting on. The triage missed it; a user hit it on 2026-10-12. Ported on `port/terminal-mouse-mode-links`.
- On `port/terminal-batch` (2026-10-14):
  - From the pending list: #52111, #52162, #54565, #59911, #61939, #62891.
  - #63707 `620efaa5c7`: follow-up to #62891, which otherwise swallows Windows AltGr input as ctrl-alt meta sequences.
  - #61079 `5079b33d65`: follow-up to #59911, whose removed `composing` guard otherwise lets Fcitx5 under KWin/Wayland loop on the same cursor rectangle and grow memory without bound.
  - #52351 `4c3179f2e2`: fixes the `test_mouse_to_cell_test` flake that #52111 introduces.
  - Missed by the triage, found by re-sweeping terminal commits: #61036 (3-line fix only), #63097.
- On `port/workspace-batch` (2026-10-15): #52035, #53552, #53659, #52871, #52692, #58435, #54243. #52035 only matters on the session-restore fallback path, but it is small and its new test fails without the fix.

- On `superzent/markdown-preview-rewrite` (2026-10-25): the markdown preview rewrite #52008 and the markdown and markdown preview commits after it, up to `60e8c18598` (#65736). Each commit's `Superzent:` note says how it was adapted. The recurring adaptations:
  - `theme_settings::` becomes `theme::`, and changes to `theme_settings` go into the fork's `theme` crate (including `theme::init` observers).
  - New settings UI items drop `organization_override` and the third `write` argument.
  - The preview DB is a `db::static_connection!` static; tests insert the workspace row into the preview's own test database.
  - Agent commits #56427, #57303 and #57376 are ported only for their `markdown` crate side, because later markdown commits build on it.
  - Tests that need gpui test APIs the fork lacks, or behavior from skipped commits, are left out or trimmed.
- Skipped from that range on purpose:

  - #61797 gpui div scrolling (needs `gestures.rs` and the `element/mouse.rs` split), so tables and code blocks scroll without `restrict_scroll_to_axis`.
  - #61773 table auto-sizing (needs taffy 0.13).
  - #63138, #65409, #65398 parse-time highlight caching, its allocation follow-up, and a crash in that cache (needs `language_core`).
  - #60864, #61784 opening links in the preview through `workspace::path_link`, which pulls in the terminal path-link rework (#57303, #52537, #61036, #61257).
  - #54514 buffer search active match change, #62490 remote images, #64978 code block scrollbar corners (needs the scrollbar rework). #56695 Mermaid UI font was ported later with `merman`.

- On `superzent/merman-toolchain` (2026-10-26):
  - The Rust toolchain moves to 1.98.1 through upstream's bumps #51086, #55104, #62395 and #63010. Files that conflicted kept the fork's version, so a follow-up commit fixes the new lints in fork code, and `crates/eval_cli/Dockerfile` (fork-only) moves to 1.98.1 too.
  - Mermaid switches to `merman`: #56430 (markdown and gpui side only), #56695, #57644, #58325, #58341, #57967, #58419, #58473, #59140, #59360, #59704, #61260, #62115 (markdown and gpui side only), #62236, #62257, #62931, #63430. #56468 was already in the fork.
  - The agent panel still doesn't render Mermaid, so the `mermaid_font_family` setting text only mentions the markdown preview.
  - `Cargo.lock` pins the `merman` crates to upstream's `0.8.0-alpha.5`. A fresh resolve picks `merman-core` 0.8.0, which the alpha API doesn't build against.

## Local changes on top of ported code

These fork-only follow-ups sit on top of upstream code, so expect small conflicts when upstream later touches the same lines:

- `git_ui/src/branch_picker.rs` `update_matches`: the query is normalized before the empty and duplicate checks for the new-branch entry (upstream still checks the raw query).
- `agent_ui/src/message_editor.rs` `confirm_command`: `.log_err()` instead of upstream's `let _ =`.
- `markdown_preview_view.rs`: previews that own their source editor (automatic previews and restored ones) forward its dirty, conflict and deleted-file state, and `open_preview_url` only percent-decodes local paths. Upstream still reports these previews as clean and decodes every link.
- `workspace.rs` `open_url_or_file`: `file:` URLs go through `Url::to_file_path()` instead of `Url::path()`.
- `gpui/src/svg_renderer.rs`: the non-emoji fallback excludes macOS's LastResort font, and `markdown/src/mermaid.rs` drops U+FE0F from the `merman` SVG before parsing it. Emoji that need U+FE0F for color (🕸️, ⚙️, ❤️) render as monochrome glyphs instead of boxes.
- `markdown/src/mermaid.rs`: the supported-diagram check skips YAML frontmatter, `%%{init}%%` directives and `%%` comments before reading the diagram type, and open previews re-render diagrams when `mermaid_font_family` (or the UI font it falls back to) changes. Upstream rejects diagrams with a preamble and only re-renders on theme changes.
- `markdown/src/parser.rs` `extract_code_block_content_range`: a closing fence may be indented by its list item's indentation plus three spaces. Upstream's #62257 allows only three spaces, so fenced blocks in nested lists show the closing fence as code and their Mermaid diagrams are skipped.
- `gpui/src/svg_renderer.rs` `render_parsed`: when the 8192px cap shrinks a raster, the image's `scale_factor` is lowered to match, so `RenderImage::render_size` (now public) stays at the requested logical size. `mermaid_base_size` uses it, so zoomed large diagrams keep their size. Upstream still shrinks them once the capped raster arrives.

## Pending for the next sync

These apply to the fork but were not ported yet. All were triaged against fork HEAD on 2026-10-10.

- **Terminal:**
  - #46648 stable terminal size when the scrollbar appears. It rewrites `ui/src/components/scrollbar.rs` on top of gpui's `Anchor` API from #47154, which the fork lacks, so it needs a hand port of the shared scrollbar.
  - #54884 `b41505358f` terminal hyperlinks shown correctly while content changes. Hand port into the fork's inline `Terminal::make_content`, since upstream split out `alacritty.rs` (#57483).
- **Buffer search:**
  - #54451 `ce08d965bc` + #56681 `3a8d012d1a` Cmd-E seeds the search query on macOS. Port together; the fork's extra `SearchableItem` impl in `agent_ui/src/text_thread_editor.rs` needs the new signature.
- **Panels:**
  - #59439 `94c6647995` zoomed panels closing when focus moves within them. Large hand port, because the fork's agent panel still uses `ActiveView`.
- **ACP:**
  - #64544 ACP terminal exit status
  - #64708 stale ACP tool output
  - #53216 duplicated ACP prompts
  - #58367 tool-call locations outside the project
- **Workspace:**
  - #62921 read-only autosave (needs a hand port)
- **gpui:**
  - #58803 image texture leak
  - #58134 stuck tooltips
  - #53378 list scrollbar drag
  - #55124 anchored menus with negative coordinates
  - #58245 traffic-light padding
  - #52641 SVG BGRA
  - #64537 standalone modifier keystrokes
- **AI:**
  - #53884 action_log race
  - #54863 disabled MCP tools still callable
  - #54793 MCP zombie processes
  - small provider fixes
- **Editor (hand port):**
  - #55863 reference highlight order
  - #58221 newline outdent
  - #53165 sticky-header autoscroll
  - #61677 project search keeps closed buffers
  - #61275 hot-path perf
  - #52907 + #60963 diagnostics re-enabling themselves
- **Project and git (hand port):**
  - #63863 FSEvents fd usage (needs the notify bump chain)
  - #57895, #58624, #57763, #58959
  - #53560 npx registry prefix
  - #51605 code_actions_on_format
  - #60798 npm 12 output
  - #61492 outer repo excludes in nested repos
  - #61750 npm cache growth
  - #59087, #59069, #59044, #60205 git process perf (skipped above)
- **Ported in part:**
  - #64525: only the `file_types` crash; invalid scan-exclusion patterns still drop all exclusions
  - #63164: only the tree view; the flat view needs collapsible sections
  - #53459: only the gpui side; no GIF animation in the old markdown preview
  - #54352: only the panic site in `split.rs`
