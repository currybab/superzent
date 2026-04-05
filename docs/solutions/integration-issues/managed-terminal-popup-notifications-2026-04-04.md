---
title: Managed terminal popup notifications can fail before reaching the popup path
date: 2026-04-04
category: integration-issues
module: managed terminal notifications
problem_type: integration_issue
component: tooling
symptoms:
  - Managed Codex or Claude terminal sessions did not show popup notifications even when `terminal.agent_notifications` was set to `always`
  - `codex` or `claude` in a managed terminal could resolve to a user-installed binary instead of the Superzent wrapper
  - Claude sessions could execute the wrapper but still never invoke `notify.sh`
root_cause: config_error
resolution_type: code_fix
severity: high
related_components:
  - development_workflow
tags: [managed-terminal-notifications, claude-hooks, codex-wrapper, popup-notifications, debug-hooks]
---

# Managed terminal popup notifications can fail before reaching the popup path

## Problem

Managed terminal notifications looked like a popup-rendering bug, but the failure happened earlier in the lifecycle. The managed `codex` / `claude` terminal flow could fail before `superzent_ui` ever reached `maybe_show_terminal_notification`, so popup windows never had a chance to open.

## Symptoms

- `terminal.agent_notifications = always` was enabled, but managed terminal popup notifications still did not appear.
- Managed `codex` sessions sometimes resolved to the expected wrapper path only after checking `type codex` inside the terminal.
- Managed `claude` sessions executed the wrapper, but `/tmp` or `$TMPDIR` debug logs showed no `notify.sh` activity at all.
- App logs showed no `superzent notification hook received` lines for the failing session.

## What Didn't Work

- Treating the problem as only a popup-policy issue did not fix anything. Logs showed the UI policy gate was never reached in the failing path.
- Temporarily overriding shell aliases/functions inside the terminal made the launch UX more complex and, for zsh, produced parse errors when function definitions were injected incorrectly.
- Assuming `native` fallback was the problem was a dead end. Native notifications had already been removed intentionally; the real issue was upstream of popup rendering.

## Solution

Trace the managed notification pipeline in order and fix the earliest failing stage.

1. Add env-gated diagnostics so the hook path can be observed end to end:

```rust
fn debug_hooks_enabled() -> bool {
    std::env::var("SUPERZENT_DEBUG_HOOKS")
        .map(|value| !matches!(value.as_str(), "0" | "false" | "FALSE" | "False"))
        .unwrap_or(false)
}
```

2. Propagate `SUPERZENT_DEBUG_HOOKS` into managed terminal environments so `notify.sh` can write debug output from inside the terminal session:

```rust
if let Ok(debug_hooks) = std::env::var(AGENT_DEBUG_HOOKS_ENV_VAR) {
    environment.insert(AGENT_DEBUG_HOOKS_ENV_VAR.to_string(), debug_hooks);
}
```

3. Expand hook event aliases so newer lifecycle names still map to the existing `Start` / `Stop` / `PermissionRequest` model:

```rust
match event_type {
    "Start"
    | "UserPromptSubmit"
    | "PostToolUse"
    | "PostToolUseFailure"
    | "BeforeAgent"
    | "AfterTool"
    | "SessionStart"
    | "sessionStart"
    | "userPromptSubmitted"
    | "postToolUse" => Some(AgentHookEventType::Start),
    "PermissionRequest" | "preToolUse" | "Notification" => {
        Some(AgentHookEventType::PermissionRequest)
    }
    "Stop" | "AfterAgent" | "agent-turn-complete" | "sessionEnd" => {
        Some(AgentHookEventType::Stop)
    }
    _ => None,
}
```

4. Keep the old “plain terminal input” preset UX and let `PATH` precedence choose the wrapper for managed sessions. Do not force a separate process/task UI just to get completion tracking.

5. Fix Claude hook configuration so the hook command is shell-safe even when the path contains spaces, and add `SessionStart` alongside the older event names:

```rust
let notify_command = format!(
    "[ -x {path} ] && {path} || true",
    path = shell_single_quote(&notify_script_path)
);

let settings = serde_json::json!({
    "hooks": {
        "UserPromptSubmit": [{ "hooks": [{ "type": "command", "command": notify_command }] }],
        "SessionStart": [{ "hooks": [{ "type": "command", "command": notify_command }] }],
        "Stop": [{ "hooks": [{ "type": "command", "command": notify_command }] }],
        "PostToolUse": [{ "matcher": "*", "hooks": [{ "type": "command", "command": notify_command }] }],
        "PostToolUseFailure": [{ "matcher": "*", "hooks": [{ "type": "command", "command": notify_command }] }],
        "PermissionRequest": [{ "matcher": "*", "hooks": [{ "type": "command", "command": notify_command }] }],
    }
});
```

6. Add targeted debug logs in `superzent_ui` for:

- hook receipt
- workspace resolution
- notification policy decision
- popup open success/failure

This makes it obvious whether the breakage is in wrapper execution, hook ingestion, workspace matching, or popup creation.

## Why This Works

The popup renderer was not the core problem. The real failures happened earlier:

- `codex` or `claude` could bypass the wrapper entirely if the shell resolved another binary first.
- Claude hook commands could silently fail because the generated settings file embedded a path with spaces as a raw command string.
- Event names coming from the agent side were not guaranteed to match the exact names the app already knew how to map.

Once the managed terminal actually ran through the wrapper, `notify.sh` fired, the hook server accepted the event, `superzent_ui` resolved the workspace, and the popup path worked again. In other words, the fix restored the event pipeline, not the popup widget itself.

## Prevention

- When debugging managed terminal notifications, verify the pipeline in this order:
  - `type codex` / `type claude`
  - `${TMPDIR:-/tmp}/superzent-notify-debug.log`
  - app logs containing `superzent notification hook`, `superzent notification policy`, and `superzent popup`
- Keep generated hook commands shell-safe. If a path can contain spaces, never write it as a raw command string.
- Add tests for hook event alias mapping whenever new lifecycle names are introduced.
- Preserve the old terminal-input UX unless there is a strong reason to replace it. UX regressions can hide the actual notification bug.
- For managed terminal investigations, use:

```bash
RUST_LOG=superzent_agent=info,superzent_ui=info SUPERZENT_DEBUG_HOOKS=1 cargo run -p superzent
```

## Related Issues

- Related requirements: `docs/brainstorms/2026-04-03-managed-terminal-notifications-always-mode-requirements.md`
- Related plan: `docs/plans/2026-04-03-002-fix-managed-terminal-notifications-always-mode-plan.md`
