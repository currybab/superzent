---
title: Managed zsh terminals can lose Codex and Claude wrapper resolution after shell startup
date: 2026-04-07
category: integration-issues
module: managed terminal wrappers
problem_type: integration_issue
component: tooling
symptoms:
  - In Superzent-launched zsh terminals, `codex` and `claude` could resolve to user-installed binaries after shell startup instead of the Superzent wrapper
  - Wrapper precedence became unreliable after user dotfiles ran, so managed agent terminals could lose hook coverage
  - Claude sessions could drop user, project, or local settings because the wrapper replaced the settings file instead of merging hooks into the existing config
root_cause: logic_error
resolution_type: code_fix
severity: high
related_components:
  - development_workflow
tags:
  [
    zsh,
    zdotdir,
    shell-startup,
    zsh-bootstrap,
    codex-wrapper,
    claude-wrapper,
    agent-hooks,
    settings-merge,
  ]
---

# Managed zsh terminals can lose Codex and Claude wrapper resolution after shell startup

## Problem

Superzent was prepending its wrapper directory into the managed terminal environment, but zsh startup could still reshuffle `PATH` after that initial injection. As a result, Superzent-launched zsh terminals could drift back to the user's real `codex` or `claude` binary after shell startup, and the Claude wrapper was also replacing settings instead of layering hooks onto the user's existing config.

## Symptoms

- In Superzent-launched zsh terminals, `which codex` or `which claude` could point at user-installed binaries after startup instead of the Superzent wrapper path.
- Managed agent sessions could lose wrapper-only behavior because hook events no longer flowed through the wrapper first.
- Claude sessions risked ignoring user, project, or local settings because `--settings` pointed at a generated file that replaced rather than extended the existing configuration model.

## What Didn't Work

- Prepending `PATH` once in the injected terminal environment was not enough. zsh can still source `.zprofile`, `.zshrc`, and `.zlogin` afterwards, and user dotfiles can prepend other directories again.
- Writing a dedicated `claude-settings.json` file and passing it to `--settings` made Superzent own the full settings payload instead of treating hooks as additive configuration.
- Temporarily overriding shell functions in the live terminal session was avoided because that path had already proven brittle in earlier managed-terminal debugging.

## Solution

Superzent now bootstraps zsh through a dedicated override `ZDOTDIR` under `agent-hooks/shell/zsh`.

`crates/project/src/terminals.rs` now:

- detects zsh when the Superzent hook environment is present
- stores the original `ZDOTDIR` in `SUPERZENT_ORIGINAL_ZDOTDIR`
- points `ZDOTDIR` at generated bootstrap files under `agent-hooks/shell/zsh`
- restores the original `ZDOTDIR` while sourcing the user's `.zshenv`, `.zprofile`, `.zshrc`, and `.zlogin`
- prepends `SUPERZENT_AGENT_HOOK_BIN_DIR` before sourcing `.zprofile`
- re-prepends the wrapper path after `.zshrc` and `.zlogin`

`crates/superzent_agent/src/runtime.rs` now:

- exports `SUPERZENT_AGENT_HOOK_BIN_DIR` alongside the existing managed-terminal environment markers
- builds the Claude hook configuration as inline JSON
- passes that JSON directly to `claude --settings '<json>'` so the hooks are added at launch time instead of replacing user/project/local settings with a generated file

## Why This Works

The zsh override gives Superzent a deterministic place to participate in shell startup instead of hoping one early `PATH` prepend survives every user dotfile mutation. Restoring the original `ZDOTDIR` only for the `source` step preserves the user's normal shell semantics while still letting Superzent reassert wrapper precedence afterwards.

On the Claude side, using inline `--settings` JSON makes the hook configuration additive. Superzent still injects its hooks, but the rest of Claude's user/project/local settings model stays intact because the wrapper is no longer swapping in a standalone generated settings file.

## Prevention

- When debugging managed zsh terminals, verify both wrapper resolution and startup ordering:
  - `echo $ZDOTDIR`
  - `which codex`
  - `which claude`
  - run `zsh` again and repeat the checks
- Prefer startup bootstrap over one-time `PATH` mutation when zsh dotfiles can still run afterwards.
- If a wrapper needs to augment Claude behavior, treat `--settings` as an additive overlay rather than replacing the user's settings file.
- Keep tests around the bootstrap order and settings payload shape. The current coverage checks:
  - original `ZDOTDIR` is restored while sourcing user dotfiles
  - `.zprofile` prepends the wrapper path before user startup runs
  - `.zshrc` and `.zlogin` re-prepend the wrapper path after user startup runs
  - the inline Claude settings payload parses as valid JSON and still contains the expected hook structure

## Related Issues

- Related solution: `docs/solutions/integration-issues/managed-terminal-popup-notifications-2026-04-04.md`
- Related requirements: `docs/brainstorms/2026-04-03-managed-terminal-notifications-always-mode-requirements.md`
- Related plan: `docs/plans/2026-04-03-002-fix-managed-terminal-notifications-always-mode-plan.md`
