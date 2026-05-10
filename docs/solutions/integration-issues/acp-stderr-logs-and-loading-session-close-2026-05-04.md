---
title: ACP lifecycle fixes must cover diagnostics and pending-session teardown
date: 2026-05-04
category: integration-issues
module: ACP agent integration
problem_type: integration_issue
component: assistant
symptoms:
  - ACP Logs showed transport stream messages but not process stderr emitted by the active agent
  - Closing a loading ACP tab could leave the remote session open because `close_session` only ran after the view reached Connected
root_cause: missing_workflow_step
resolution_type: code_fix
severity: medium
related_components:
  - tooling
  - testing_framework
tags:
  [acp, agent-ui, agent-servers, acp-tools, stderr, close-session, lifecycle]
---

# ACP lifecycle fixes must cover diagnostics and pending-session teardown

## Problem

ACP integration work crossed three surfaces: the server process wrapper, the ACP Logs tool, and the conversation view lifecycle. Two workflow steps were easy to miss during the sync: stderr emitted by an active ACP process was only written to the Rust log, and closing a tab while an existing session was still loading did not call `close_session`.

## Symptoms

- The ACP Logs panel could replay ACP stream messages from the registry backlog, but process stderr was invisible there.
- The stderr task in `crates/agent_servers/src/acp.rs` called `log::warn!` and then discarded the line.
- `ConversationView::on_release` closed sessions only after the view reached `Connected`.
- Loading an existing ACP session and immediately closing the tab could leave the provider-side session alive.

## What Didn't Work

- Relying on `log::warn!` for stderr was not enough. The ACP Logs tool subscribes to `AcpConnectionRegistry`, so anything that does not enter that registry is not replayed in the tool backlog.
- Capturing only `connection.subscribe()` traffic covered protocol stream messages, but stderr is process I/O and has to be explicitly converted into a log entry.
- Adding pending-session tracking inside `AcpConnection` did not protect the UI release path by itself. If `ConversationView::on_release` never calls `close_session` while the view is still `Loading`, the connection-level cleanup code is never reached.
- A broad session-history search surfaced upstream context that ACP transport and stderr were intended to be captured into a log ring, but the local branch still had a server-side stderr task that only warned and cleared each line (session history).

## Solution

Route stderr through the same active-connection registry used by ACP stream messages, and make the loading view retain the resolved connection long enough for `on_release` to close a pending session.

For diagnostics, model the registry backlog as log entries instead of only stream messages:

```rust
#[derive(Clone)]
enum AcpLogEntry {
    Stream(acp::StreamMessage),
    Stderr { line: SharedString },
}
```

Then record stderr only when it belongs to the currently active connection:

```rust
pub fn record_stderr_line(
    &self,
    connection: &Weak<acp::ClientSideConnection>,
    line: impl Into<SharedString>,
) {
    let is_active = self
        .active_connection
        .borrow()
        .as_ref()
        .is_some_and(|active_connection| {
            Weak::ptr_eq(&active_connection.connection, connection)
        });
    if !is_active {
        return;
    }

    self.record_stderr_entry(line);
}
```

The stderr task now runs on the foreground executor so it can update the GPUI entity, and it passes a weak connection identity into the registry:

```rust
let stderr_task = cx.spawn({
    let connection_registry = connection_registry.clone();
    let connection_weak = Rc::downgrade(&connection);
    async move |cx| {
        let mut stderr = BufReader::new(stderr);
        let mut line = String::new();
        while let Ok(n) = stderr.read_line(&mut line).await
            && n > 0
        {
            let stderr_line = line.trim().to_string();
            log::warn!("agent stderr: {}", stderr_line);
            connection_registry.update(cx, |registry, _| {
                registry.record_stderr_line(&connection_weak, stderr_line)
            });
            line.clear();
        }
        Ok(())
    }
});
```

For pending-session teardown, store the resolved connection in `LoadingView`:

```rust
struct LoadingView {
    session_id: Option<acp::SessionId>,
    connection: Rc<RefCell<Option<Rc<dyn AgentConnection>>>>,
    _load_task: Task<()>,
}
```

Then close a loading session from the release hook when the connection has resolved and the agent supports close:

```rust
} else if let ServerState::Loading(loading) = &this.server_state {
    loading.update(cx, |loading, cx| {
        if let Some(session_id) = loading.session_id.as_ref()
            && let Some(connection) = loading.connection.borrow().clone()
            && connection.supports_close_session()
        {
            connection
                .close_session(session_id, cx)
                .detach_and_log_err(cx);
        }
    });
}
```

## Why This Works

ACP Logs has one source of truth: `AcpConnectionRegistry`. By widening the registry backlog from `StreamMessage` to `AcpLogEntry`, stream traffic and stderr follow the same backlog and subscriber path. The active-connection guard prevents a stale stderr reader from appending lines after another agent connection becomes active.

The loading-session fix closes the missing UI lifecycle step. A session load starts only after the connection resolves, so the loading view stores that connection once it exists. If the user closes the tab before the view transitions to `Connected`, `on_release` can still call `close_session` for the pending `session_id`.

## Prevention

- When adding ACP diagnostics, test the data source that the ACP Logs UI actually subscribes to. `registry_backlog_includes_stderr_lines` guards that stderr is retained in the backlog, not merely written to process logs.
- When adding connection lifecycle cleanup, test the UI state that triggers cleanup. `test_loading_view_close_closes_pending_session` covers the case where `ConversationView` is released before the session load future completes.
- Treat provider-side cleanup as a two-part contract: the connection implementation can support `close_session`, but every UI release path that owns a session id still has to call it.
- For process I/O readers that outlive UI state changes, include an identity check before appending to global or shared diagnostic state.

## Related Issues

- Plan context: [Backport Zed 1.0 Next-Edit and ACP Polish](../../plans/2026-05-03-001-feat-zed-1-0-next-edit-acp-sync-plan.md) is the direct planning artifact for this sync.
- Broader sync context: [Backport Upstream Editor and Search Improvements](../../plans/2026-04-23-001-feat-upstream-editor-search-sync-plan.md) explains the selective upstream-sync constraints that make ACP changes require Superzent boundary review.
- Low-overlap related doc: [Restoring default-build next-edit requires separating it from hosted AI surfaces](./default-build-next-edit-surface-restoration-2026-04-05.md) mentions `acp_tabs`, but covers feature gating rather than ACP lifecycle cleanup.
- Session history surfaced upstream ACP debug/log work, including `73127da4b7 acp_tools: Always capture ACP transport and stderr into the log ring (#54536)` and `1c1b03c3 acp: Improve ACP debug view (#54769)`. These explain why local stderr and debug-view wiring should be treated as part of one diagnostics path.
