use std::time::Instant;

use acp_thread::{AcpThread, AcpThreadEvent, AgentThreadEntry, ThreadStatus, ToolCallStatus};
use agent_ui::{ThreadView, external_acp_tab_threads};
use gpui::{AnyWindowHandle, App, Context, Entity, EntityId, Subscription, Window, WindowHandle};
use ui::IconName;
use workspace::{MultiWorkspace, Workspace};

use superzent_model::WorkspaceAttentionStatus;

use crate::{
    TerminalLifecycleNotification, WorkspaceAttentionController,
    agent_list::{AgentListEntry, AgentListGroup, AgentListIcon, AgentListTarget},
    matched_workspace_id_for_candidate_locations, ordered_multi_workspace_windows,
    workspace_location_candidates,
};

// Matches the cap on prompts that agent CLIs report through hooks.
const PROMPT_TITLE_MAX_CHARS: usize = 120;

/// A top-level conversation in an ACP tab. Its status, title, and pending approvals are
/// read from the thread itself; only what the thread can't tell is kept here.
pub(crate) struct AcpSessionInfo {
    sequence: u64,
    working_since: Option<Instant>,
    needs_review: bool,
    // What the conversation last gave its workspace's attention, and which workspace.
    attention: WorkspaceAttentionStatus,
    workspace_id: Option<String>,
    _subscriptions: [Subscription; 3],
}

fn workspace_attention_for_group(group: AgentListGroup) -> WorkspaceAttentionStatus {
    match group {
        AgentListGroup::NeedsApproval => WorkspaceAttentionStatus::Permission,
        AgentListGroup::NeedsReview => WorkspaceAttentionStatus::Review,
        AgentListGroup::Working => WorkspaceAttentionStatus::Working,
        AgentListGroup::Idle => WorkspaceAttentionStatus::Idle,
    }
}

pub(crate) struct FocusedAcpThread {
    thread_id: EntityId,
    window: AnyWindowHandle,
}

pub(crate) fn acp_thread_group(
    awaits_approval: bool,
    needs_review: bool,
    generating: bool,
) -> AgentListGroup {
    if awaits_approval {
        AgentListGroup::NeedsApproval
    } else if needs_review {
        AgentListGroup::NeedsReview
    } else if generating {
        AgentListGroup::Working
    } else {
        AgentListGroup::Idle
    }
}

fn thread_awaits_approval(thread: &AcpThread) -> bool {
    thread.entries().iter().any(|entry| {
        matches!(
            entry,
            AgentThreadEntry::ToolCall(tool_call)
                if matches!(tool_call.status, ToolCallStatus::WaitingForConfirmation { .. })
        )
    })
}

// What a thread is called until its agent titles it.
const DEFAULT_THREAD_TITLE: &str = "New Thread";

/// The thread's title once its agent names it. Until then the thread carries the agent's
/// own name, which says nothing about the task.
fn acp_thread_title(title: &str, agent_name: &str) -> Option<String> {
    let title = title.trim();
    (!title.is_empty() && !title.eq_ignore_ascii_case(agent_name) && title != DEFAULT_THREAD_TITLE)
        .then(|| title.to_string())
}

/// The first line of the first prompt, which names the task until the agent titles the
/// thread.
fn first_prompt(thread: &AcpThread, cx: &App) -> Option<String> {
    thread.entries().iter().find_map(|entry| {
        let AgentThreadEntry::UserMessage(message) = entry else {
            return None;
        };
        let markdown = message.content.to_markdown(cx);
        let line = markdown
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())?;
        Some(line.chars().take(PROMPT_TITLE_MAX_CHARS).collect())
    })
}

impl WorkspaceAttentionController {
    pub(crate) fn track_acp_thread(
        &mut self,
        thread: Entity<AcpThread>,
        is_subagent: bool,
        cx: &mut Context<Self>,
    ) {
        if is_subagent {
            // A subagent's approval requests show on its parent conversation's row.
            let subscription =
                cx.subscribe(&thread, |controller, subagent, event, cx| match event {
                    AcpThreadEvent::ToolAuthorizationRequested(_) => {
                        if let Some(parent_id) = controller.acp_parent_thread_id(&subagent, cx) {
                            controller.notify_unless_viewed(
                                TerminalLifecycleNotification::PermissionRequest,
                                parent_id,
                                cx,
                            );
                            controller.sync_acp_attention(parent_id, cx);
                        }
                        cx.notify();
                    }
                    AcpThreadEvent::ToolAuthorizationReceived(_) => {
                        if let Some(parent_id) = controller.acp_parent_thread_id(&subagent, cx) {
                            controller.sync_acp_attention(parent_id, cx);
                        }
                        cx.notify();
                    }
                    _ => {}
                });
            let thread_id = thread.entity_id();
            self.acp_subagent_subscriptions
                .insert(thread_id, subscription);
            cx.observe_release(&thread, move |controller, _, _| {
                controller.acp_subagent_subscriptions.remove(&thread_id);
            })
            .detach();
            return;
        }

        let thread_id = thread.entity_id();
        let subscriptions = [
            cx.subscribe(&thread, Self::handle_acp_thread_event),
            cx.observe(&thread, Self::handle_acp_thread_changed),
            cx.observe_release(&thread, move |controller, _, cx| {
                controller.store.update(cx, |store, _| {
                    store.forget_unreviewed_terminal(&AgentListTarget::AcpThread(thread_id).key());
                });
                if let Some(workspace_id) = controller
                    .acp_sessions
                    .remove(&thread_id)
                    .and_then(|session| session.workspace_id)
                {
                    controller.recompute_workspace_attention(&workspace_id, cx);
                }
                cx.notify();
            }),
        ];
        let sequence = self.next_agent_sequence;
        self.next_agent_sequence += 1;
        self.acp_sessions.insert(
            thread_id,
            AcpSessionInfo {
                sequence,
                working_since: None,
                needs_review: false,
                attention: WorkspaceAttentionStatus::Idle,
                workspace_id: None,
                _subscriptions: subscriptions,
            },
        );
    }

    fn handle_acp_thread_event(
        &mut self,
        thread: Entity<AcpThread>,
        event: &AcpThreadEvent,
        cx: &mut Context<Self>,
    ) {
        let thread_id = thread.entity_id();
        if !self.acp_sessions.contains_key(&thread_id) {
            return;
        }
        let viewed = self.acp_thread_viewed(thread_id, cx);
        let mut changed = self.sync_acp_working(&thread, cx);
        match event {
            AcpThreadEvent::Stopped(_) | AcpThreadEvent::Error | AcpThreadEvent::Refusal => {
                if let Some(session) = self.acp_sessions.get_mut(&thread_id) {
                    session.working_since = None;
                    session.needs_review = !viewed;
                }
                self.notify_unless_viewed(TerminalLifecycleNotification::Completed, thread_id, cx);
                changed = true;
            }
            AcpThreadEvent::ToolAuthorizationRequested(_) => {
                self.notify_unless_viewed(
                    TerminalLifecycleNotification::PermissionRequest,
                    thread_id,
                    cx,
                );
                changed = true;
            }
            // The agent reports its commands and options once the session is set up, by
            // which time its tab shows the thread and can be listed.
            AcpThreadEvent::TitleUpdated
            | AcpThreadEvent::ToolAuthorizationReceived(_)
            | AcpThreadEvent::LoadError(_)
            | AcpThreadEvent::PromptCapabilitiesUpdated
            | AcpThreadEvent::AvailableCommandsUpdated(_)
            | AcpThreadEvent::ModeUpdated(_)
            | AcpThreadEvent::ConfigOptionsUpdated(_) => changed = true,
            // Entries stream in while the agent works, so only a change in status (caught
            // above) is worth rebuilding the list for.
            AcpThreadEvent::NewEntry
            | AcpThreadEvent::EntryUpdated(_)
            | AcpThreadEvent::EntriesRemoved(_)
            | AcpThreadEvent::TokenUsageUpdated
            | AcpThreadEvent::Retry(_)
            | AcpThreadEvent::SubagentSpawned(_) => {}
        }
        if changed {
            self.sync_acp_attention(thread_id, cx);
            cx.notify();
        }
    }

    /// Catches the thread starting or stopping work. A retried turn starts without adding
    /// an entry, so this runs on every event and every change of the thread.
    fn sync_acp_working(&mut self, thread: &Entity<AcpThread>, cx: &App) -> bool {
        let generating = thread.read(cx).status() == ThreadStatus::Generating;
        let Some(session) = self.acp_sessions.get_mut(&thread.entity_id()) else {
            return false;
        };
        if generating == session.working_since.is_some() {
            return false;
        }
        session.working_since = generating.then(Instant::now);
        if generating {
            session.needs_review = false;
        }
        true
    }

    fn handle_acp_thread_changed(&mut self, thread: Entity<AcpThread>, cx: &mut Context<Self>) {
        if self.sync_acp_working(&thread, cx) {
            self.sync_acp_attention(thread.entity_id(), cx);
            cx.notify();
        }
    }

    /// Carries the conversation's state over to its workspace's attention, as terminal
    /// agents' hooks do.
    fn sync_acp_attention(&mut self, thread_id: EntityId, cx: &mut Context<Self>) {
        let Some(thread) = self
            .acp_threads
            .iter()
            .filter_map(|thread| thread.upgrade())
            .find(|thread| thread.entity_id() == thread_id)
        else {
            return;
        };
        let Some(session) = self.acp_sessions.get(&thread_id) else {
            return;
        };
        let attention = workspace_attention_for_group(acp_thread_group(
            self.acp_thread_awaits_approval(thread.read(cx), cx),
            session.needs_review,
            thread.read(cx).status() == ThreadStatus::Generating,
        ));
        // Focus changes arrive while their window is being updated, when no tab in it can
        // be read; the conversation hasn't moved then.
        let workspace_id = self
            .acp_thread_workspace_id(thread_id, cx)
            .or_else(|| session.workspace_id.clone());
        // Activating the workspace clears its review unless something in it is still
        // unseen, so an unseen conversation is registered like an unseen terminal.
        let unseen_review_key = AgentListTarget::AcpThread(thread_id).key();
        let needs_review = session.needs_review;
        self.store.update(cx, |store, cx| match &workspace_id {
            Some(workspace_id) if needs_review => {
                store.mark_terminal_unreviewed(&unseen_review_key, workspace_id);
            }
            _ => {
                store.mark_terminal_reviewed(&unseen_review_key, cx);
            }
        });
        let Some(session) = self.acp_sessions.get_mut(&thread_id) else {
            return;
        };
        if session.attention == attention && session.workspace_id == workspace_id {
            return;
        }
        session.attention = attention;
        let previous_workspace_id = std::mem::replace(&mut session.workspace_id, workspace_id);
        let workspace_id = session.workspace_id.clone();
        if let Some(previous_workspace_id) = previous_workspace_id
            && Some(&previous_workspace_id) != workspace_id.as_ref()
        {
            self.recompute_workspace_attention(&previous_workspace_id, cx);
        }
        if let Some(workspace_id) = workspace_id {
            self.recompute_workspace_attention(&workspace_id, cx);
        }
    }

    /// The live attention ACP conversations give a workspace.
    pub(crate) fn acp_workspace_attention<'a>(
        &'a self,
        workspace_id: &'a str,
    ) -> impl Iterator<Item = WorkspaceAttentionStatus> + 'a {
        self.acp_sessions
            .values()
            .filter(move |session| session.workspace_id.as_deref() == Some(workspace_id))
            .map(|session| session.attention.clone())
    }

    fn notify_unless_viewed(
        &mut self,
        notification: TerminalLifecycleNotification,
        thread_id: EntityId,
        cx: &mut Context<Self>,
    ) {
        if !self.acp_thread_viewed(thread_id, cx) {
            self.maybe_show_acp_notification(notification, thread_id, cx);
        }
    }

    fn acp_parent_thread_id(&self, subagent: &Entity<AcpThread>, cx: &App) -> Option<EntityId> {
        let parent_session_id = subagent.read(cx).parent_session_id()?.clone();
        self.acp_threads
            .iter()
            .filter_map(|thread| thread.upgrade())
            .find(|thread| thread.read(cx).session_id() == &parent_session_id)
            .map(|thread| thread.entity_id())
            .filter(|thread_id| self.acp_sessions.contains_key(thread_id))
    }

    fn acp_thread_viewed(&self, thread_id: EntityId, cx: &App) -> bool {
        self.focused_acp_thread.as_ref().is_some_and(|focused| {
            focused.thread_id == thread_id && cx.active_window() == Some(focused.window)
        })
    }

    pub(crate) fn handle_acp_focus_in(
        &mut self,
        thread_id: EntityId,
        window: AnyWindowHandle,
        window_active: bool,
        cx: &mut Context<Self>,
    ) {
        self.focused_acp_thread = Some(FocusedAcpThread { thread_id, window });
        if window_active {
            if let Some(session) = self.acp_sessions.get_mut(&thread_id) {
                session.needs_review = false;
            }
            self.sync_acp_attention(thread_id, cx);
            if self
                .active_notification_target
                .as_ref()
                .is_some_and(|target| target.target == AgentListTarget::AcpThread(thread_id))
            {
                self.dismiss_notifications(cx);
            }
        }
        cx.notify();
    }

    pub(crate) fn handle_acp_focus_out(&mut self, thread_id: EntityId, cx: &mut Context<Self>) {
        if self
            .focused_acp_thread
            .as_ref()
            .is_some_and(|focused| focused.thread_id == thread_id)
        {
            self.focused_acp_thread = None;
            cx.notify();
        }
    }

    fn acp_thread_awaits_approval(&self, thread: &AcpThread, cx: &App) -> bool {
        thread_awaits_approval(thread)
            || self
                .acp_threads
                .iter()
                .filter_map(|subagent| subagent.upgrade())
                .any(|subagent| {
                    let subagent = subagent.read(cx);
                    subagent.parent_session_id() == Some(thread.session_id())
                        && thread_awaits_approval(subagent)
                })
    }

    pub(crate) fn acp_agent_list_entries(&self, cx: &App) -> Vec<AgentListEntry> {
        let workspace_entries = self.store.read(cx).workspaces();
        let mut entries = Vec::new();
        for window in ordered_multi_workspace_windows(cx) {
            let Ok(multi_workspace) = window.read(cx) else {
                continue;
            };
            for workspace in multi_workspace.workspaces() {
                let workspace_id = matched_workspace_id_for_candidate_locations(
                    &workspace_location_candidates(workspace, cx),
                    workspace_entries,
                    None,
                );
                for tab in external_acp_tab_threads(workspace.read(cx), cx) {
                    let thread_id = tab.thread.entity_id();
                    let Some(session) = self.acp_sessions.get(&thread_id) else {
                        continue;
                    };
                    let thread = tab.thread.read(cx);
                    entries.push(AgentListEntry {
                        target: AgentListTarget::AcpThread(thread_id),
                        group: acp_thread_group(
                            self.acp_thread_awaits_approval(thread, cx),
                            session.needs_review,
                            thread.status() == ThreadStatus::Generating,
                        ),
                        title: acp_thread_title(&thread.title(), &tab.display_name)
                            .or_else(|| first_prompt(thread, cx)),
                        name: tab.display_name.to_string(),
                        icon: tab
                            .icon_path
                            .map(AgentListIcon::External)
                            .unwrap_or(AgentListIcon::Named(IconName::Sparkle)),
                        workspace_id: workspace_id.clone(),
                        working_since: session.working_since,
                        sequence: session.sequence,
                        focused: self.acp_thread_viewed(thread_id, cx),
                    });
                }
            }
        }
        entries
    }

    pub(crate) fn acp_thread_workspace_id(&self, thread_id: EntityId, cx: &App) -> Option<String> {
        let (_, workspace, _) = find_acp_tab(thread_id, cx)?;
        matched_workspace_id_for_candidate_locations(
            &workspace_location_candidates(&workspace, cx),
            self.store.read(cx).workspaces(),
            None,
        )
    }

    pub(crate) fn open_acp_thread(&mut self, thread_id: EntityId, cx: &mut Context<Self>) {
        let Some((window, workspace, item_id)) = find_acp_tab(thread_id, cx) else {
            log::warn!("cannot open ACP thread {thread_id:?}: no tab shows it");
            return;
        };
        cx.activate(true);
        let activated = window.update(cx, |multi_workspace, window, cx| {
            window.activate_window();
            multi_workspace.activate(workspace.clone(), cx);
            workspace.update(cx, |workspace, cx| {
                let item = workspace.panes().iter().find_map(|pane| {
                    pane.read(cx)
                        .items()
                        .find(|item| item.item_id() == item_id)
                        .map(|item| item.boxed_clone())
                });
                if let Some(item) = item {
                    workspace.activate_item(item.as_ref(), true, true, window, cx);
                }
            });
        });
        if let Err(error) = activated {
            log::error!("failed to open ACP thread {thread_id:?}: {error:#}");
        }
    }
}

/// The window, workspace, and tab item showing an ACP conversation.
fn find_acp_tab(
    thread_id: EntityId,
    cx: &App,
) -> Option<(WindowHandle<MultiWorkspace>, Entity<Workspace>, EntityId)> {
    ordered_multi_workspace_windows(cx)
        .into_iter()
        .find_map(|window| {
            let multi_workspace = window.read(cx).ok()?;
            multi_workspace.workspaces().iter().find_map(|workspace| {
                external_acp_tab_threads(workspace.read(cx), cx)
                    .into_iter()
                    .find(|tab| tab.thread.entity_id() == thread_id)
                    .map(|tab| (window, workspace.clone(), tab.item_id))
            })
        })
}

/// Tracks focus on a top-level conversation the way terminal focus is tracked, so a turn
/// that finishes while its conversation is in front of the user needs no review.
pub(crate) fn observe_thread_view_focus(
    thread_view: &ThreadView,
    attention_controller: &Entity<WorkspaceAttentionController>,
    window: &mut Window,
    cx: &mut Context<ThreadView>,
) {
    if thread_view.parent_id.is_some() {
        return;
    }
    let thread_id = thread_view.thread.entity_id();
    let focus_handle = thread_view.focus_handle.clone();
    cx.on_focus_in(&focus_handle, window, {
        let attention_controller = attention_controller.clone();
        move |_, window, cx| {
            let window_handle = window.window_handle();
            let window_active = window.is_window_active();
            attention_controller.update(cx, |controller, cx| {
                controller.handle_acp_focus_in(thread_id, window_handle, window_active, cx);
            });
        }
    })
    .detach();
    cx.on_focus_out(&focus_handle, window, {
        let attention_controller = attention_controller.clone();
        move |_, _, _, cx| {
            attention_controller.update(cx, |controller, cx| {
                controller.handle_acp_focus_out(thread_id, cx);
            });
        }
    })
    .detach();
    // Returning to the app doesn't move focus, so a conversation that was already focused
    // only becomes "seen" through window activation.
    cx.observe_window_activation(window, {
        let attention_controller = attention_controller.clone();
        move |thread_view, window, cx| {
            if !window.is_window_active() || !thread_view.focus_handle.contains_focused(window, cx)
            {
                return;
            }
            let window_handle = window.window_handle();
            attention_controller.update(cx, |controller, cx| {
                controller.handle_acp_focus_in(thread_id, window_handle, true, cx);
            });
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn threads_named_after_their_agent_have_no_title_yet() {
        assert_eq!(
            acp_thread_title("[WIP] Fix the store", "Claude Code").as_deref(),
            Some("[WIP] Fix the store")
        );
        assert_eq!(acp_thread_title("Claude Code", "Claude Code"), None);
        assert_eq!(acp_thread_title("New Thread", "Codex"), None);
        assert_eq!(acp_thread_title("  ", "Codex"), None);
    }

    #[test]
    fn approval_outranks_review_which_outranks_work() {
        assert_eq!(
            acp_thread_group(true, true, true),
            AgentListGroup::NeedsApproval
        );
        assert_eq!(
            acp_thread_group(false, true, false),
            AgentListGroup::NeedsReview
        );
        assert_eq!(
            acp_thread_group(false, false, true),
            AgentListGroup::Working
        );
        assert_eq!(acp_thread_group(false, false, false), AgentListGroup::Idle);
    }
}
