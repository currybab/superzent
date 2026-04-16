---
title: "Fix codex wrapper recursive fork between superzent instances"
type: fix
status: active
date: 2026-04-16
---

# Fix codex wrapper recursive fork between superzent instances

## Overview

`wrapper_resolver_content()` generates a `find_real_binary()` shell function that only skips its own `bin_dir` when searching PATH. When both superzent and superzent-dev are installed, each instance's wrapper finds the other as the "real binary", causing infinite mutual recursion that spawns 1000+ processes and accumulates megabytes of duplicate `-c notify=...` arguments.

## Problem Frame

Users running both superzent (release) and superzent-dev (dev build) simultaneously get a fork bomb when launching codex from either instance. The claude wrapper is unaffected because it uses `exec` (process replacement), but the codex wrapper uses a subshell invocation with a background watcher, so each recursive call creates new processes.

The root cause is in the Rust code that generates the shell wrapper, not in the shell scripts themselves.

## Scope Boundaries

- Fix `wrapper_resolver_content()` to detect and skip other Superzent wrapper scripts
- Both claude and codex wrappers share the same resolver, so both benefit from the fix
- No changes to the watcher subshell logic or overall wrapper structure

### Non-goals

- Changing codex wrapper to use `exec` (would break the watcher cleanup pattern)
- Adding environment-variable-based recursion guards (unnecessary if resolver is correct)

## Context & Research

### Relevant Code and Patterns

- `crates/superzent_agent/src/runtime.rs:526` — `wrapper_resolver_content()` generates the resolver
- `crates/superzent_agent/src/runtime.rs:28` — `WRAPPER_MARKER` constant: `"# Superzent agent wrapper v1"`
- `crates/superzent_agent/src/runtime.rs:556` — `claude_wrapper_content()` uses the resolver
- `crates/superzent_agent/src/runtime.rs:586` — `codex_wrapper_content()` uses the resolver
- All generated wrappers already include `WRAPPER_MARKER` on line 2 of the script

### Institutional Learnings

- `docs/solutions/integration-issues/managed-zsh-terminals-can-lose-codex-and-claude-wrapper-resolution-after-shell-startup-2026-04-07.md` — Documents the wrapper resolution architecture and previous PATH-related fixes

## Key Technical Decisions

- **Check file content for WRAPPER_MARKER instead of pattern-matching directory names**: Directory name matching (the current `case` statement) is brittle because it only knows about one specific path. Content-based detection works regardless of how many superzent instances exist or where they are installed.
- **Use `head -2` + `grep -qF` for detection**: The marker is always on line 2 of generated wrappers. Reading only 2 lines is fast and avoids scanning entire files. `grep -qF` does a fixed-string match (no regex overhead).

## Implementation Units

- [ ] **Unit 1: Add WRAPPER_MARKER skip to `wrapper_resolver_content()`**

**Goal:** Make `find_real_binary()` skip any candidate binary that contains the Superzent wrapper marker, preventing cross-instance recursion.

**Requirements:** Eliminate recursive fork between superzent/superzent-dev wrapper scripts.

**Dependencies:** None

**Files:**

- Modify: `crates/superzent_agent/src/runtime.rs`

**Approach:**
Inside the `for dir in $PATH` loop in the generated shell code, after checking `[ -x "$dir/$name" ]`, add a content check: read the first 2 lines of the candidate file and skip it if the `WRAPPER_MARKER` string is found. The marker string should be injected from the Rust `WRAPPER_MARKER` constant so it stays in sync.

**Patterns to follow:**

- The existing `case "$dir" in "{bin_dir}") continue ;;` pattern for skipping directories. The new check is an additional skip condition on the candidate file itself.

**Test scenarios:**

- Happy path: Generated `find_real_binary()` shell function contains a `head -2` + `grep -qF` check for the wrapper marker string
- Happy path: The marker string in the generated shell matches `WRAPPER_MARKER` constant value
- Edge case: Existing `bin_dir` skip logic is preserved (the content check is additive, not a replacement)

**Verification:**

- The generated wrapper scripts from both `claude_wrapper_content()` and `codex_wrapper_content()` contain the marker-skip logic
- Running two superzent instances no longer causes recursive process spawning

## System-Wide Impact

- **Interaction graph:** Both `claude_wrapper_content()` and `codex_wrapper_content()` call `wrapper_resolver_content()`, so both wrappers get the fix automatically
- **Error propagation:** If `head`/`grep` fail (e.g., binary file), the `2>/dev/null` and `||` guard ensure the candidate is not skipped — safe fallthrough
- **Unchanged invariants:** The `SUPERZENT_REAL_CLAUDE_BIN` / `SUPERZENT_REAL_CODEX_BIN` override env vars still bypass all PATH resolution, unaffected by this change

## Risks & Dependencies

| Risk                                                             | Mitigation                                                                             |
| ---------------------------------------------------------------- | -------------------------------------------------------------------------------------- |
| `head -2` on a large binary could be slow                        | `head` reads at most a few bytes from the file header and exits; negligible overhead   |
| Marker string changes in future without updating the shell check | Both use the same `WRAPPER_MARKER` Rust constant, so they stay in sync by construction |

## Sources & References

- Related solution: `docs/solutions/integration-issues/managed-zsh-terminals-can-lose-codex-and-claude-wrapper-resolution-after-shell-startup-2026-04-07.md`
