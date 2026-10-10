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
  - markdown preview rewrite #52008;
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

## Pending for the next sync

These apply to the fork but were not ported yet. All were triaged against fork HEAD on 2026-10-10.

- **Terminal and ACP:**
  - #59911 IME candidate window in TUI apps
  - #52162 + #54565 truecolor contrast
  - #46648 stable terminal size when the scrollbar appears
  - #62891 alt-F5 / ctrl-alt keys
  - #61939 + #52111 inline terminal clipping
  - #64544 ACP terminal exit status
  - #64708 stale ACP tool output
  - #53216 duplicated ACP prompts
  - #58367 tool-call locations outside the project
- **Workspace data loss and restore:**
  - #54224 recent-projects cleanup deletes scratch workspaces
  - #52035 removed workspace resurrected
  - #53362 one failed workspace aborts window restore
  - #60477 session lost on red-X quit
- **Workspace (clean):**
  - Clean cherry-picks not yet taken: #53659 rules files, #52692 format on first save, #52871, #53552 + #58435 auto-update status, #54243, #52671
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
