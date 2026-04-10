#[cfg(feature = "acp_tabs")]
mod acp_tabs;
#[cfg(feature = "acp_tabs")]
pub use acp_tabs::{FocusAcpTab, NewAcpTab, OpenAcpHistory};

#[cfg(feature = "acp_tabs")]
use crate::acp_tabs::{CLAUDE_AGENT_NAME, CODEX_NAME, GEMINI_NAME};
#[cfg(feature = "acp_tabs")]
use agent_ui::{
    AgentNotification, AgentNotificationEvent, open_external_acp_tab, pane_has_external_acp_item,
};
use anyhow::Result;
use chrono::Utc;
#[cfg(target_os = "macos")]
use cocoa::base::{id, nil};
use editor::{Editor, EditorEvent, actions::SelectAll};
use git::repository::validate_worktree_directory;
use git_ui::git_panel::GitPanel;
use gpui::{
    Action, Animation, AnimationExt, App, AsyncWindowContext, ClickEvent, DismissEvent, Entity,
    EntityId, EventEmitter, FocusHandle, Focusable, MouseButton, MouseDownEvent, PathPromptOptions,
    Point, PromptLevel, ScrollHandle, SharedString, Subscription, Task, WeakEntity, WindowHandle,
    actions, anchored, deferred, px,
};
use menu;
use notifications::status_toast::{StatusToast, ToastIcon};
#[cfg(target_os = "macos")]
use objc::{
    class,
    declare::ClassDecl,
    msg_send,
    runtime::{BOOL, Class, Object, Sel, YES},
    sel, sel_impl,
};
#[cfg(feature = "acp_tabs")]
use project::agent_server_store::{AllAgentServersSettings, CustomAgentServerSettings};
use project::git_store::{GitStoreEvent, Repository, RepositoryEvent, pending_op};
use project::project_settings::ProjectSettings;
#[cfg(feature = "acp_tabs")]
use project::{AgentId, AgentRegistryStore};
use project_panel::ProjectPanel;
use recent_projects::open_remote_project;
use remote::{RemoteConnectionOptions, SshConnectionOptions};
use settings::Settings;
use smol::channel::{Receiver as SmolReceiver, Sender as SmolSender};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
#[cfg(target_os = "macos")]
use std::{
    ffi::{CStr, c_char},
    sync::{Mutex, OnceLock},
};
use superzent_agent::{
    AGENT_TERMINAL_ID_ENV_VAR, AGENT_WORKSPACE_ID_ENV_VAR, AgentHookEvent, AgentHookEventType,
};
use superzent_model::{
    AgentPreset, GitChangeSummary, PresetLaunchMode, ProjectEntry, ProjectLocation,
    StoredSshConnection, StoredSshPortForward, SuperzentStore, TaskStatus,
    WorkspaceAttentionStatus, WorkspaceEntry, WorkspaceGitStatus, WorkspaceKind, WorkspaceLocation,
    aggregate_workspace_attention_status,
};
use task::{Shell, ShellKind};
use terminal::{
    Event as TerminalEvent,
    terminal_settings::{TerminalAgentNotificationMode, TerminalSettings},
};
use terminal_view::{TerminalView, terminal_panel::TerminalPanel};
#[cfg(feature = "acp_tabs")]
use ui::ContextMenuEntry;
use ui::{
    ButtonLike, Checkbox, Chip, CommonAnimationExt, ContextMenu, CopyButton, Disclosure,
    DropdownMenu, DropdownStyle, ElevationIndex, Icon, Indicator, ListItem, Modal, ModalFooter,
    ModalHeader, Section, Tab, ToggleState, Tooltip, prelude::*,
};
use uuid::Uuid;
use workspace::{
    AppState as WorkspaceAppState, ModalView, MultiWorkspace, MultiWorkspaceEvent,
    NextWorkspaceInWindow, OpenOptions, Pane, PreviousWorkspaceInWindow,
    SerializedWorkspaceLocation, Sidebar as WorkspaceSidebar, SidebarEvent, Toast, Workspace,
    dock::{DockPosition, Panel, PanelEvent},
    notifications::NotificationId,
    workspace_windows_for_location,
};
#[cfg(feature = "acp_tabs")]
use zed_actions::AcpRegistry;
use zed_actions::{OpenRecent, OpenRemote, OpenSettingsAt};

actions!(
    superzent,
    [
        AddProject,
        NewWorkspace,
        RevealChanges,
        CloseFromRightSidebar,
        OpenWorkspaceInNewWindow,
        CloseWorkspace,
        DeleteWorkspace,
        CollapseWorkspaceSection,
        ExpandWorkspaceSection
    ]
);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RightSidebarTab {
    Changes,
    Files,
    Panel(EntityId),
}

fn show_superzent_right_sidebar(
    workspace: &mut Workspace,
    tab: Option<RightSidebarTab>,
    focus: bool,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    workspace.open_panel::<SuperzentRightSidebar>(window, cx);
    if focus {
        workspace.focus_panel::<SuperzentRightSidebar>(window, cx);
    }

    if let Some(panel) = workspace.panel::<SuperzentRightSidebar>(cx) {
        panel.update(cx, |panel, cx| {
            if let Some(tab) = tab {
                panel.set_active_tab(tab, cx);
            } else {
                cx.notify();
            }
        });
    }
}

pub fn show_superzent_files_sidebar(
    workspace: &mut Workspace,
    focus: bool,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    show_superzent_right_sidebar(workspace, Some(RightSidebarTab::Files), focus, window, cx);
}

pub fn show_superzent_changes_sidebar(
    workspace: &mut Workspace,
    focus: bool,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    show_superzent_right_sidebar(workspace, Some(RightSidebarTab::Changes), focus, window, cx);
}

pub fn toggle_superzent_files_sidebar(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    toggle_superzent_right_sidebar(workspace, RightSidebarTab::Files, window, cx);
}

pub fn toggle_superzent_changes_sidebar(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    toggle_superzent_right_sidebar(workspace, RightSidebarTab::Changes, window, cx);
}

fn toggle_superzent_right_sidebar(
    workspace: &mut Workspace,
    tab: RightSidebarTab,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let should_close = workspace.right_dock().read(cx).is_open()
        && workspace
            .right_dock()
            .read(cx)
            .active_panel()
            .is_some_and(|panel| panel.panel_key() == SuperzentRightSidebar::panel_key())
        && workspace
            .panel::<SuperzentRightSidebar>(cx)
            .is_some_and(|panel| panel.read(cx).is_tab_active(tab));

    if should_close {
        workspace
            .right_dock()
            .update(cx, |dock, cx| dock.set_open(false, window, cx));
        return;
    }

    show_superzent_right_sidebar(workspace, Some(tab), true, window, cx);
}

#[derive(Clone)]
struct LiveTerminalAttention {
    workspace_id: String,
    status: WorkspaceAttentionStatus,
}

struct WorkspaceAttentionController {
    store: Entity<SuperzentStore>,
    terminal_ids_by_entity: BTreeMap<EntityId, String>,
    workspace_ids_by_terminal: BTreeMap<String, String>,
    live_terminal_attention: BTreeMap<String, LiveTerminalAttention>,
    #[cfg(feature = "acp_tabs")]
    notifications: Vec<WindowHandle<AgentNotification>>,
    #[cfg(feature = "acp_tabs")]
    notification_subscriptions: Vec<Subscription>,
    _hook_task: Task<Result<()>>,
    _notification_activation_task: Task<Result<()>>,
}

fn debug_terminal_notifications_enabled() -> bool {
    std::env::var("SUPERZENT_DEBUG_HOOKS")
        .map(|value| !matches!(value.as_str(), "0" | "false" | "FALSE" | "False"))
        .unwrap_or(false)
}

impl WorkspaceAttentionController {
    fn new(cx: &mut Context<Self>) -> Self {
        let store = SuperzentStore::global(cx);
        let hook_task = match superzent_agent::subscribe() {
            Ok(receiver) => cx.spawn(async move |this, cx| {
                while let Ok(event) = receiver.recv().await {
                    this.update(cx, |this, cx| {
                        this.handle_hook_event(event, cx);
                    })?;
                }
                Ok(())
            }),
            Err(error) => {
                log::error!("failed to subscribe to Superzent agent hooks: {error:#}");
                Task::ready(Ok(()))
            }
        };
        let notification_activation_task =
            match take_native_terminal_notification_activation_receiver() {
                Some(receiver) => cx.spawn(async move |this, cx| {
                    while let Ok(workspace_id) = receiver.recv().await {
                        this.update(cx, |this, cx| {
                            this.handle_native_notification_activation(&workspace_id, cx);
                        })?;
                    }
                    Ok(())
                }),
                None => Task::ready(Ok(())),
            };

        Self {
            store,
            terminal_ids_by_entity: BTreeMap::new(),
            workspace_ids_by_terminal: BTreeMap::new(),
            live_terminal_attention: BTreeMap::new(),
            #[cfg(feature = "acp_tabs")]
            notifications: Vec::new(),
            #[cfg(feature = "acp_tabs")]
            notification_subscriptions: Vec::new(),
            _hook_task: hook_task,
            _notification_activation_task: notification_activation_task,
        }
    }

    fn register_terminal<T>(
        &mut self,
        terminal: Entity<T>,
        terminal_id: String,
        workspace_id: Option<String>,
        cx: &mut Context<Self>,
    ) where
        T: 'static,
    {
        let entity_id = terminal.entity_id();
        self.terminal_ids_by_entity
            .insert(entity_id, terminal_id.clone());
        if let Some(workspace_id) = workspace_id {
            self.workspace_ids_by_terminal
                .insert(terminal_id.clone(), workspace_id);
        }

        cx.observe_release(&terminal, move |this, _, cx| {
            this.unregister_terminal(&terminal_id, entity_id, cx);
        })
        .detach();
    }

    fn unregister_terminal(
        &mut self,
        terminal_id: &str,
        entity_id: EntityId,
        cx: &mut Context<Self>,
    ) {
        self.terminal_ids_by_entity.remove(&entity_id);
        let tracked_workspace_id = self.workspace_ids_by_terminal.remove(terminal_id);
        let workspace_id = workspace_id_for_terminal_unregister(
            self.live_terminal_attention.remove(terminal_id).as_ref(),
            tracked_workspace_id.as_deref(),
        );
        if let Some(workspace_id) = workspace_id {
            self.recompute_workspace_attention(&workspace_id, cx);
        }
    }

    fn handle_terminal_input(
        &mut self,
        terminal_id: &str,
        workspace_id: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(next_status) = next_terminal_input_attention_status(
            self.live_terminal_attention
                .get(terminal_id)
                .map(|attention| &attention.status),
        ) else {
            return;
        };

        // Reused agent terminals can take a fresh prompt before the next start hook lands.
        // Treat outbound input as "work resumed", but never downgrade a pending permission.
        self.live_terminal_attention.insert(
            terminal_id.to_string(),
            LiveTerminalAttention {
                workspace_id: workspace_id.to_string(),
                status: next_status,
            },
        );
        self.store.update(cx, |store, cx| {
            store.set_workspace_attention(
                workspace_id,
                WorkspaceAttentionStatus::Idle,
                false,
                None,
                cx,
            );
        });
        self.recompute_workspace_attention(workspace_id, cx);
    }

    fn handle_hook_event(&mut self, event: AgentHookEvent, cx: &mut Context<Self>) {
        if debug_terminal_notifications_enabled() {
            log::info!(
                "superzent notification hook received: type={:?} terminal_id={} workspace_id={:?} session_id={:?} cwd={:?}",
                event.event_type,
                event.terminal_id,
                event.workspace_id,
                event.session_id,
                event.cwd,
            );
        }
        let Some((workspace_id, workspace_name)) = self
            .resolve_workspace_for_event(&event, cx)
            .map(|workspace| {
                (
                    workspace.id.clone(),
                    workspace_notification_title(&workspace),
                )
            })
        else {
            if debug_terminal_notifications_enabled() {
                log::warn!(
                    "superzent notification hook could not resolve workspace: type={:?} terminal_id={} workspace_id={:?} session_id={:?} cwd={:?}",
                    event.event_type,
                    event.terminal_id,
                    event.workspace_id,
                    event.session_id,
                    event.cwd,
                );
            } else {
                log::debug!("ignoring agent hook event without a matching workspace");
            }
            return;
        };

        if debug_terminal_notifications_enabled() {
            log::info!(
                "superzent notification hook resolved workspace: event={:?} workspace_id={} workspace_name={}",
                event.event_type,
                workspace_id,
                workspace_name,
            );
        }

        match event.event_type {
            AgentHookEventType::Start => {
                self.live_terminal_attention.insert(
                    event.terminal_id,
                    LiveTerminalAttention {
                        workspace_id: workspace_id.clone(),
                        status: WorkspaceAttentionStatus::Working,
                    },
                );
                self.store.update(cx, |store, cx| {
                    store.set_workspace_attention(
                        &workspace_id,
                        WorkspaceAttentionStatus::Idle,
                        false,
                        None,
                        cx,
                    );
                });
                self.recompute_workspace_attention(&workspace_id, cx);
            }
            AgentHookEventType::PermissionRequest => {
                self.live_terminal_attention.insert(
                    event.terminal_id.clone(),
                    LiveTerminalAttention {
                        workspace_id: workspace_id.clone(),
                        status: WorkspaceAttentionStatus::Permission,
                    },
                );
                self.store.update(cx, |store, cx| {
                    store.set_workspace_attention(
                        &workspace_id,
                        WorkspaceAttentionStatus::Idle,
                        false,
                        None,
                        cx,
                    );
                });
                self.recompute_workspace_attention(&workspace_id, cx);
                self.maybe_show_terminal_notification(
                    TerminalLifecycleNotification::PermissionRequest,
                    &workspace_id,
                    &workspace_name,
                    cx,
                );
            }
            AgentHookEventType::Stop => {
                self.live_terminal_attention.remove(&event.terminal_id);
                let (attention_status, review_pending) =
                    workspace_attention_for_terminal_status(&TaskStatus::Completed)
                        .expect("completed terminal status should map to attention");
                self.store.update(cx, |store, cx| {
                    store.set_workspace_attention(
                        &workspace_id,
                        attention_status,
                        review_pending,
                        workspace_attention_reason_for_terminal_status(
                            &TaskStatus::Completed,
                            None,
                        ),
                        cx,
                    );
                });
                self.recompute_workspace_attention(&workspace_id, cx);
                self.maybe_show_terminal_notification(
                    TerminalLifecycleNotification::Completed,
                    &workspace_id,
                    &workspace_name,
                    cx,
                );
            }
        }
    }

    fn resolve_workspace_for_event(
        &self,
        event: &AgentHookEvent,
        cx: &App,
    ) -> Option<WorkspaceEntry> {
        let store = self.store.read(cx);
        if let Some(workspace_id) = event.workspace_id.as_deref() {
            return store.workspace(workspace_id).cloned();
        }

        event
            .cwd
            .as_deref()
            .and_then(|cwd| store.workspace_for_path_or_ancestor(cwd).cloned())
    }

    fn recompute_workspace_attention(&mut self, workspace_id: &str, cx: &mut Context<Self>) {
        let live_attention_status = self
            .live_terminal_attention
            .values()
            .filter(|attention| attention.workspace_id == workspace_id)
            .map(|attention| attention.status.clone())
            .max_by_key(attention_priority);
        let review_pending = self
            .store
            .read(cx)
            .workspace(workspace_id)
            .map(|workspace| workspace.review_pending)
            .unwrap_or(false);
        let attention_status =
            aggregate_workspace_attention_status(live_attention_status, review_pending);

        self.store.update(cx, |store, cx| {
            let reason = if attention_status == WorkspaceAttentionStatus::Review {
                Some("Agent task completed".to_string())
            } else {
                None
            };
            store.set_workspace_attention(
                workspace_id,
                attention_status,
                review_pending,
                reason,
                cx,
            );
        });
    }

    fn handle_native_notification_activation(
        &mut self,
        workspace_id: &str,
        cx: &mut Context<Self>,
    ) {
        self.dismiss_notifications(cx);

        let Some(workspace_entry) = self.store.read(cx).workspace(workspace_id).cloned() else {
            return;
        };

        cx.activate(true);

        let Some(app_state) = WorkspaceAppState::try_global(cx).and_then(|state| state.upgrade())
        else {
            log::error!("failed to open workspace from notification: missing app state");
            return;
        };
        let Some(target_window) = fallback_notification_window(cx) else {
            log::error!("failed to open workspace from notification: no workspace window found");
            return;
        };

        let activated_existing_workspace =
            match target_window.update(cx, |multi_workspace, window, cx| {
                let live_workspace = multi_workspace
                    .workspaces()
                    .iter()
                    .find(|workspace| workspace_matches_entry(workspace, &workspace_entry, cx))
                    .cloned();
                window.activate_window();
                if let Some(live_workspace) = live_workspace {
                    multi_workspace.activate(live_workspace, cx);
                    true
                } else {
                    false
                }
            }) {
                Ok(activated_existing_workspace) => activated_existing_workspace,
                Err(error) => {
                    log::error!("failed to activate workspace from notification: {error:#}");
                    false
                }
            };
        if activated_existing_workspace {
            return;
        }

        let open_task = match target_window.update(cx, |_, window, cx| {
            window.activate_window();
            open_workspace_entry(workspace_entry.clone(), app_state.clone(), window, cx)
        }) {
            Ok(open_task) => open_task,
            Err(error) => {
                log::error!("failed to dispatch workspace open from notification: {error:#}");
                return;
            }
        };
        open_task.detach_and_log_err(cx);
    }

    fn maybe_show_terminal_notification(
        &mut self,
        notification: TerminalLifecycleNotification,
        workspace_id: &str,
        workspace_name: &str,
        cx: &mut Context<Self>,
    ) {
        let mode = TerminalSettings::get_global(cx).agent_notifications;
        let should_show = should_show_terminal_notification(mode, workspace_id, &self.store, cx);
        if debug_terminal_notifications_enabled() {
            log::info!(
                "superzent notification policy: event={:?} mode={:?} workspace_id={} active_window={} active_workspace_id={:?} should_show={}",
                notification,
                mode,
                workspace_id,
                cx.active_window().is_some(),
                self.store.read(cx).active_workspace_id(),
                should_show,
            );
        }
        if !should_show {
            return;
        }

        self.show_popup_notification(notification, workspace_id, workspace_name, cx);
    }

    #[cfg(feature = "acp_tabs")]
    fn show_popup_notification(
        &mut self,
        notification: TerminalLifecycleNotification,
        workspace_id: &str,
        workspace_name: &str,
        cx: &mut Context<Self>,
    ) {
        if debug_terminal_notifications_enabled() {
            log::info!(
                "superzent popup opening: event={:?} workspace_id={} workspace_name={}",
                notification,
                workspace_id,
                workspace_name,
            );
        }
        self.dismiss_notifications(cx);

        let Some(screen) = cx
            .primary_display()
            .or_else(|| cx.displays().into_iter().next())
        else {
            if debug_terminal_notifications_enabled() {
                log::warn!("superzent popup aborted: no display available");
            }
            return;
        };

        let title = SharedString::from(notification.title());
        let caption = SharedString::from(notification.caption());
        let workspace_name = SharedString::from(workspace_name.to_string());
        let icon = notification.icon();
        let options = AgentNotification::window_options(screen, cx);

        let screen_window = match cx.open_window(options, {
            move |_window, cx| {
                cx.new(|_cx| {
                    AgentNotification::new(
                        title.clone(),
                        caption.clone(),
                        icon,
                        Some(workspace_name.clone()),
                    )
                    .with_action_label("Open Workspace")
                })
            }
        }) {
            Ok(screen_window) => screen_window,
            Err(error) => {
                log::error!("failed to open terminal agent notification window: {error:#}");
                return;
            }
        };

        let pop_up = match screen_window.entity(cx) {
            Ok(pop_up) => pop_up,
            Err(error) => {
                log::error!("failed to access terminal agent notification window: {error:#}");
                let _ = screen_window.update(cx, |_, window, _| {
                    window.remove_window();
                });
                return;
            }
        };

        if debug_terminal_notifications_enabled() {
            log::info!(
                "superzent popup opened successfully: event={:?} workspace_id={}",
                notification,
                workspace_id,
            );
        }

        let workspace_id = workspace_id.to_string();
        self.notification_subscriptions
            .push(
                cx.subscribe(&pop_up, move |this, _, event, cx| match event {
                    AgentNotificationEvent::Accepted => {
                        this.handle_native_notification_activation(&workspace_id, cx);
                    }
                    AgentNotificationEvent::Dismissed => {
                        this.dismiss_notifications(cx);
                    }
                }),
            );
        self.notifications.push(screen_window);
    }

    #[cfg(not(feature = "acp_tabs"))]
    fn show_popup_notification(
        &mut self,
        notification: TerminalLifecycleNotification,
        workspace_id: &str,
        _workspace_name: &str,
        _cx: &mut Context<Self>,
    ) {
        static DID_LOG: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        if DID_LOG.set(()).is_ok() {
            log::warn!(
                "terminal popup notifications require acp_tabs; dropping {:?} for workspace {workspace_id}",
                notification
            );
        }
    }

    #[cfg(feature = "acp_tabs")]
    fn dismiss_notifications(&mut self, cx: &mut Context<Self>) {
        for window in self.notifications.drain(..) {
            window
                .update(cx, |_, window, _| {
                    window.remove_window();
                })
                .ok();
        }

        self.notification_subscriptions.clear();
    }

    #[cfg(not(feature = "acp_tabs"))]
    fn dismiss_notifications(&mut self, _cx: &mut Context<Self>) {}
}

#[derive(Clone, Copy, Debug)]
enum TerminalLifecycleNotification {
    Completed,
    PermissionRequest,
}

impl TerminalLifecycleNotification {
    #[cfg(feature = "acp_tabs")]
    fn title(self) -> &'static str {
        match self {
            Self::Completed => "Agent task finished",
            Self::PermissionRequest => "Agent needs approval",
        }
    }

    #[cfg(feature = "acp_tabs")]
    fn caption(self) -> &'static str {
        match self {
            Self::Completed => "Managed terminal task completed",
            Self::PermissionRequest => "Approval is required to continue",
        }
    }

    #[cfg(feature = "acp_tabs")]
    fn icon(self) -> IconName {
        match self {
            Self::Completed => IconName::Check,
            Self::PermissionRequest => IconName::Warning,
        }
    }
}

pub fn init(cx: &mut App) {
    if SuperzentStore::try_global(cx).is_none() {
        return;
    }

    #[cfg(feature = "acp_tabs")]
    acp_tabs::init(cx);

    let attention_controller = cx.new(WorkspaceAttentionController::new);

    cx.observe_new(
        move |terminal_view: &mut TerminalView, _window, cx: &mut Context<TerminalView>| {
            let terminal = terminal_view.terminal().clone();
            let (terminal_id, workspace_id) = {
                let terminal_snapshot = terminal.read(cx);
                let Some(terminal_id) = terminal_snapshot
                    .env_var(AGENT_TERMINAL_ID_ENV_VAR)
                    .map(str::to_string)
                else {
                    return;
                };
                let workspace_id = terminal_snapshot
                    .env_var(AGENT_WORKSPACE_ID_ENV_VAR)
                    .map(str::to_string);
                (terminal_id, workspace_id)
            };

            attention_controller.update(cx, |controller, cx| {
                controller.register_terminal(
                    terminal,
                    terminal_id.clone(),
                    workspace_id.clone(),
                    cx,
                );
            });

            let Some(workspace_id) = workspace_id else {
                return;
            };

            let terminal_id = terminal_id.clone();
            let attention_controller = attention_controller.clone();
            cx.subscribe(&cx.entity(), move |_, _, event: &TerminalEvent, cx| {
                if event != &TerminalEvent::Input {
                    return;
                }

                attention_controller.update(cx, |controller, cx| {
                    controller.handle_terminal_input(&terminal_id, &workspace_id, cx);
                });
            })
            .detach();
        },
    )
    .detach();

    cx.observe_new(|pane: &mut Pane, _window, cx: &mut Context<Pane>| {
        let pane_handle = cx.entity();
        let pane_id = pane_handle.entity_id();
        let empty_state =
            cx.new(|cx| SuperzentEmptyPaneView::new(pane_handle.downgrade(), pane_id, cx));
        pane.set_empty_state_view(empty_state.into(), cx);
    })
    .detach();

    cx.observe_new(
        |workspace: &mut Workspace, _window, _: &mut Context<Workspace>| {
            workspace
                .register_action(|workspace, _: &AddProject, window, cx| {
                    run_add_project(workspace, window, cx);
                })
                .register_action(|workspace, _: &NewWorkspace, window, cx| {
                    run_new_workspace(workspace, window, cx);
                })
                .register_action(|workspace, _: &RevealChanges, window, cx| {
                    run_reveal_changes(workspace, window, cx);
                })
                .register_action(|workspace, _: &OpenWorkspaceInNewWindow, window, cx| {
                    run_open_workspace_in_new_window(workspace, window, cx);
                })
                .register_action(|workspace, _: &workspace::CloseWindow, window, cx| {
                    let right_dock = workspace.right_dock().clone();
                    if !right_dock.read(cx).is_open() {
                        // When there are no tabs and the right dock is closed,
                        // absorb Cmd+W so it does nothing instead of closing the window.
                        if workspace.active_pane().read(cx).items_len() == 0 {
                            return;
                        }
                        cx.propagate();
                        return;
                    }

                    let Some(active_panel) = right_dock.read(cx).active_panel().cloned() else {
                        cx.propagate();
                        return;
                    };
                    if active_panel.panel_key() != SuperzentRightSidebar::panel_key() {
                        cx.propagate();
                        return;
                    }

                    let Some(panel) = workspace.panel::<SuperzentRightSidebar>(cx) else {
                        cx.propagate();
                        return;
                    };

                    match panel.read(cx).tab {
                        RightSidebarTab::Changes | RightSidebarTab::Files => {
                            if workspace.active_pane().read(cx).items_len() > 0 {
                                window.dispatch_action(Box::new(CloseFromRightSidebar), cx);
                            }
                        }
                        RightSidebarTab::Panel(_) => {
                            window.dispatch_action(Box::new(workspace::CloseActiveDock), cx);
                        }
                    }
                })
                .register_action(|workspace, _: &CloseWorkspace, window, cx| {
                    run_close_workspace(workspace, window, cx);
                })
                .register_action(|workspace, _: &CloseFromRightSidebar, window, cx| {
                    let Some(panel) = workspace.panel::<SuperzentRightSidebar>(cx) else {
                        return;
                    };

                    match panel.read(cx).tab {
                        RightSidebarTab::Changes | RightSidebarTab::Files => {
                            let active_pane = workspace.active_pane().clone();
                            if active_pane.read(cx).items_len() > 0 {
                                active_pane.update(cx, |pane, cx| {
                                    pane.close_active_item(
                                        &workspace::CloseActiveItem::default(),
                                        window,
                                        cx,
                                    )
                                    .detach_and_log_err(cx);
                                });
                            }
                        }
                        RightSidebarTab::Panel(_) => {
                            window.dispatch_action(Box::new(workspace::CloseActiveDock), cx);
                        }
                    }
                })
                .register_action(|workspace, _: &DeleteWorkspace, window, cx| {
                    run_delete_workspace(workspace, window, cx);
                })
                .register_action(|_, _: &NextWorkspaceInWindow, window, cx| {
                    cycle_workspace_in_window_from_window(
                        WorkspaceSwitchDirection::Forward,
                        window,
                        cx,
                    );
                })
                .register_action(|_, _: &PreviousWorkspaceInWindow, window, cx| {
                    cycle_workspace_in_window_from_window(
                        WorkspaceSwitchDirection::Backward,
                        window,
                        cx,
                    );
                });
        },
    )
    .detach();
}

pub fn install_pane_accessory(pane: &Entity<Pane>, cx: &mut Context<Workspace>) {
    let Some(store) = SuperzentStore::try_global(cx) else {
        return;
    };
    let pane_handle = pane.clone();
    cx.observe(&store, move |_, _, cx| {
        let pane_handle = pane_handle.clone();
        pane_handle.update(cx, |_, cx| cx.notify());
    })
    .detach();

    pane.update(cx, |pane, cx| {
        pane.set_render_tab_bar_accessory(cx, render_terminal_preset_bar);
    });
}

fn render_terminal_preset_bar(
    pane: &mut Pane,
    window: &mut Window,
    cx: &mut Context<Pane>,
) -> Option<AnyElement> {
    let workspace_handle = pane.workspace()?;
    let store = SuperzentStore::try_global(cx)?;
    let (workspace_entry, presets) = {
        let store = store.read(cx);
        let workspace_entry = store.active_workspace().cloned()?;
        (workspace_entry, store.presets().to_vec())
    };

    let active_acp_history_button = render_active_acp_history_button(pane, &workspace_entry.id, cx);
    let (visible_presets, hidden_presets) = split_presets_for_width(
        &presets,
        available_preset_bar_width(window),
        active_acp_history_button.is_some(),
    );
    let hidden_dropdown = (!hidden_presets.is_empty()).then(|| {
        render_hidden_preset_dropdown(
            workspace_handle.clone(),
            workspace_entry.clone(),
            hidden_presets.clone(),
            window,
            cx,
        )
    });
    Some(
        h_flex()
            .id(format!("superzent-preset-bar-{}", workspace_entry.id))
            .w_full()
            .items_center()
            .justify_between()
            .gap_2()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(cx.theme().colors().border_variant)
            .bg(cx.theme().colors().editor_background)
            .child(h_flex().min_w_0().items_center().gap_1().children(
                visible_presets.into_iter().map(|preset| {
                    render_workspace_preset_button(
                        workspace_handle.clone(),
                        workspace_entry.clone(),
                        preset,
                        window,
                        cx,
                    )
                }),
            ))
            .child(
                h_flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap_1()
                    .children(hidden_dropdown)
                    .children(active_acp_history_button)
                    .child(render_preset_actions_dropdown(
                        &workspace_entry.id,
                        window,
                        cx,
                    )),
            )
            .into_any_element(),
    )
}

fn render_workspace_preset_button(
    workspace_handle: Entity<Workspace>,
    workspace_entry: WorkspaceEntry,
    preset: AgentPreset,
    _window: &mut Window,
    _cx: &mut Context<Pane>,
) -> AnyElement {
    Button::new(
        format!(
            "superzent-preset-button-{}-{}",
            workspace_entry.id, preset.id
        ),
        preset.label.clone(),
    )
    .label_size(LabelSize::Small)
    .style(ButtonStyle::Subtle)
    .on_click(move |_, window, cx| {
        launch_workspace_preset(
            workspace_handle.clone(),
            workspace_entry.clone(),
            preset.id.clone(),
            None,
            window,
            cx,
        );
    })
    .into_any_element()
}

fn render_hidden_preset_dropdown(
    workspace_handle: Entity<Workspace>,
    workspace_entry: WorkspaceEntry,
    hidden_presets: Vec<AgentPreset>,
    window: &mut Window,
    cx: &mut Context<Pane>,
) -> AnyElement {
    let workspace_id = workspace_entry.id.clone();
    let workspace_entry_for_menu = workspace_entry;
    let menu = ContextMenu::build(window, cx, move |mut menu, _, _| {
        for preset in &hidden_presets {
            let workspace_handle = workspace_handle.clone();
            let workspace_entry = workspace_entry_for_menu.clone();
            let preset_id = preset.id.clone();
            let label = preset.label.clone();
            menu = menu.entry(label, None, move |window, cx| {
                launch_workspace_preset(
                    workspace_handle.clone(),
                    workspace_entry.clone(),
                    preset_id.clone(),
                    None,
                    window,
                    cx,
                );
            });
        }

        menu
    });

    DropdownMenu::new(
        format!("superzent-preset-overflow-{workspace_id}"),
        "More",
        menu,
    )
    .style(DropdownStyle::Ghost)
    .into_any_element()
}

fn available_preset_bar_width(window: &Window) -> Pixels {
    window.viewport_size().width
}

fn split_presets_for_width(
    presets: &[AgentPreset],
    available_width: Pixels,
    reserve_history_button: bool,
) -> (Vec<AgentPreset>, Vec<AgentPreset>) {
    let mut visible_presets =
        select_presets_for_width(presets, available_width, false, reserve_history_button);
    let mut hidden_presets = presets[visible_presets.len()..].to_vec();

    if !hidden_presets.is_empty() {
        visible_presets =
            select_presets_for_width(presets, available_width, true, reserve_history_button);
        hidden_presets = presets[visible_presets.len()..].to_vec();
    }

    (visible_presets, hidden_presets)
}

fn select_presets_for_width(
    presets: &[AgentPreset],
    available_width: Pixels,
    reserve_overflow: bool,
    reserve_history_button: bool,
) -> Vec<AgentPreset> {
    let reserved_width = if reserve_overflow { 132.0 } else { 88.0 }
        + if reserve_history_button { 84.0 } else { 0.0 };
    let available_button_width = (f32::from(available_width) - reserved_width).max(0.0);
    let mut used_width = 0.0;
    let mut visible_presets = Vec::new();

    for preset in presets {
        let button_width = estimated_preset_button_width(preset);
        if used_width + button_width <= available_button_width {
            visible_presets.push(preset.clone());
            used_width += button_width;
        } else {
            break;
        }
    }

    visible_presets
}

fn estimated_preset_button_width(preset: &AgentPreset) -> f32 {
    ((preset.label.chars().count() as f32) * 7.5 + 48.0).max(84.0)
}

fn open_agent_presets_settings(window: &mut Window, cx: &mut App) {
    window.dispatch_action(
        OpenSettingsAt {
            path: "terminal.agent_presets".to_string(),
        }
        .boxed_clone(),
        cx,
    );
}

#[cfg(feature = "acp_tabs")]
fn render_active_acp_history_button(
    pane: &Pane,
    workspace_id: &str,
    _cx: &mut Context<Pane>,
) -> Option<AnyElement> {
    if !pane_has_external_acp_item(pane) {
        return None;
    }
    Some(
        Button::new(format!("superzent-acp-history-{workspace_id}"), "History")
            .label_size(LabelSize::Small)
            .style(ButtonStyle::Subtle)
            .on_click(move |_, window, cx| open_acp_history(None, window, cx))
            .into_any_element(),
    )
}

#[cfg(not(feature = "acp_tabs"))]
fn render_active_acp_history_button(
    _pane: &Pane,
    _workspace_id: &str,
    _cx: &mut Context<Pane>,
) -> Option<AnyElement> {
    None
}

fn render_preset_actions_dropdown(
    workspace_id: &str,
    window: &mut Window,
    cx: &mut Context<Pane>,
) -> AnyElement {
    let menu = ContextMenu::build(window, cx, move |mut menu, _, _| {
        menu = menu.entry("Agent Presets", None, |window, cx| {
            open_agent_presets_settings(window, cx);
        });

        #[cfg(feature = "acp_tabs")]
        {
            menu = menu.item(
                ContextMenuEntry::new("ACP Registry")
                    .icon(IconName::Flask)
                    .icon_position(IconPosition::End)
                    .handler(|window, cx| {
                        open_acp_registry(window, cx);
                    }),
            );
        }

        menu
    });

    DropdownMenu::new_with_element(
        format!("superzent-preset-actions-{workspace_id}"),
        Icon::new(IconName::Settings).into_any_element(),
        menu,
    )
    .style(DropdownStyle::Ghost)
    .trigger_tooltip(|window, cx| Tooltip::text("Preset and ACP options")(window, cx))
    .no_chevron()
    .into_any_element()
}

#[cfg(feature = "acp_tabs")]
fn open_acp_registry(window: &mut Window, cx: &mut App) {
    window.dispatch_action(Box::new(AcpRegistry), cx);
}

#[cfg(feature = "acp_tabs")]
fn open_acp_history(agent_name: Option<String>, window: &mut Window, cx: &mut App) {
    window.dispatch_action(OpenAcpHistory { agent_name }.boxed_clone(), cx);
}

fn launch_workspace_preset(
    workspace_handle: Entity<Workspace>,
    workspace_entry: WorkspaceEntry,
    preset_id: String,
    task_prompt: Option<String>,
    window: &mut Window,
    cx: &mut App,
) {
    let store = SuperzentStore::global(cx);
    let Some(preset) = store.read(cx).preset(&preset_id).cloned() else {
        show_workspace_toast(
            &workspace_handle,
            format!("Preset `{preset_id}` is missing."),
            cx,
        );
        return;
    };

    store.update(cx, |store, cx| {
        store.set_workspace_agent_preset(&workspace_entry.id, &preset.id, cx);
    });
    let task_prompt = task_prompt.filter(|task_prompt| !task_prompt.trim().is_empty());
    match preset.launch_mode {
        PresetLaunchMode::Terminal => {
            if let Some(task_prompt) = task_prompt {
                launch_workspace_preset_task(
                    workspace_handle,
                    workspace_entry,
                    preset,
                    Some(task_prompt),
                    window,
                    cx,
                );
            } else {
                launch_workspace_preset_in_terminal(
                    workspace_handle,
                    workspace_entry,
                    preset,
                    window,
                    cx,
                );
            }
        }
        PresetLaunchMode::Acp => {
            launch_workspace_preset_as_acp(workspace_handle, preset, task_prompt, window, cx);
        }
    }
}

#[cfg(feature = "acp_tabs")]
fn launch_workspace_preset_as_acp(
    workspace_handle: Entity<Workspace>,
    preset: AgentPreset,
    task_prompt: Option<String>,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(agent_name) = preset.resolved_acp_agent_name() else {
        show_workspace_toast(
            &workspace_handle,
            format!("{} is missing an ACP agent name.", preset.label),
            cx,
        );
        return;
    };

    acp_tabs::ensure_promoted_agent_enabled(&agent_name, cx);
    refresh_registry_agent_if_needed(&agent_name, cx);
    let workspace_for_launch = workspace_handle.clone();
    window
        .spawn(cx, async move |cx| {
            match wait_for_acp_agent_registration(&workspace_handle, agent_name.as_ref(), cx).await
            {
                AcpAgentRegistrationWaitResult::Registered => {}
                AcpAgentRegistrationWaitResult::RegistryFetchFailed(error) => {
                    show_workspace_toast_async(
                        &workspace_handle,
                        format!("Failed to load ACP Registry for `{agent_name}`: {error}"),
                        cx,
                    );
                    return Ok::<(), anyhow::Error>(());
                }
                AcpAgentRegistrationWaitResult::TimedOut => {
                    show_workspace_toast_async(
                        &workspace_handle,
                        acp_agent_loading_message(&agent_name),
                        cx,
                    );
                    return Ok::<(), anyhow::Error>(());
                }
            }

            let _ = workspace_for_launch.update_in(cx, |workspace, window, cx| {
                open_external_acp_tab(
                    workspace,
                    Some(agent_name.clone()),
                    task_prompt.clone(),
                    window,
                    cx,
                );
            });
            Ok::<(), anyhow::Error>(())
        })
        .detach();
}

#[cfg(feature = "acp_tabs")]
fn refresh_registry_agent_if_needed(agent_name: &str, cx: &mut App) {
    let uses_registry = matches!(agent_name, CLAUDE_AGENT_NAME | CODEX_NAME | GEMINI_NAME)
        || cx
            .global::<settings::SettingsStore>()
            .get::<AllAgentServersSettings>(None)
            .get(agent_name)
            .is_some_and(|settings| matches!(settings, CustomAgentServerSettings::Registry { .. }));

    if !uses_registry {
        return;
    }

    if let Some(registry_store) = AgentRegistryStore::try_global(cx) {
        let should_refresh = {
            let registry_store = registry_store.read(cx);
            let agent_id = AgentId::new(agent_name.to_string());
            registry_store.agent(&agent_id).is_none() || registry_store.fetch_error().is_some()
        };
        if should_refresh {
            registry_store.update(cx, |registry_store, cx| registry_store.refresh(cx));
        }
    }
}

#[cfg(feature = "acp_tabs")]
fn acp_agent_loading_message(agent_name: &str) -> String {
    if matches!(agent_name, CLAUDE_AGENT_NAME | CODEX_NAME | GEMINI_NAME) {
        format!(
            "ACP agent `{agent_name}` is still loading from the ACP Registry. Open ACP Registry or try again in a moment."
        )
    } else {
        format!("ACP agent `{agent_name}` is still loading. Try again in a moment.")
    }
}

#[cfg(not(feature = "acp_tabs"))]
fn launch_workspace_preset_as_acp(
    workspace_handle: Entity<Workspace>,
    preset: AgentPreset,
    _task_prompt: Option<String>,
    _window: &mut Window,
    cx: &mut App,
) {
    show_workspace_toast(
        &workspace_handle,
        format!("{} requires ACP support in this build.", preset.label),
        cx,
    );
}

fn launch_workspace_preset_in_terminal(
    workspace_handle: Entity<Workspace>,
    workspace_entry: WorkspaceEntry,
    preset: AgentPreset,
    window: &mut Window,
    cx: &mut App,
) {
    let store = SuperzentStore::global(cx);
    let launch = match superzent_agent::prepare_workspace_launch(&workspace_entry, &preset) {
        Ok(launch) => launch,
        Err(error) => {
            show_workspace_toast(
                &workspace_handle,
                format!("Failed to prepare {}: {error}", preset.label),
                cx,
            );
            return;
        }
    };

    let workspace_path = workspace_entry.cwd_path();
    let (command_line, open_terminal_task) = workspace_handle.update(cx, |workspace, cx| {
        let shell_kind = preset_shell_kind(workspace, &workspace_path, cx);
        let command_line = render_preset_command_line(&launch.command, &launch.args, shell_kind);
        let environment = launch.environment.clone();
        let working_directory = Some(workspace_path.clone());
        let preset_label = preset.label.clone();
        let open_terminal_task =
            TerminalPanel::add_center_terminal(workspace, window, cx, move |project, cx| {
                project.create_terminal_shell_with_environment_and_title(
                    working_directory,
                    environment,
                    preset_label,
                    cx,
                )
            });
        (command_line, open_terminal_task)
    });

    window
        .spawn(cx, async move |cx| {
            let terminal = match open_terminal_task.await {
                Ok(terminal) => terminal,
                Err(error) => {
                    show_workspace_toast_async(
                        &workspace_handle,
                        format!("Failed to open {} terminal: {error}", preset.label),
                        cx,
                    );
                    return Ok::<(), anyhow::Error>(());
                }
            };

            if let Err(error) = terminal.update_in(cx, |terminal, _, _| {
                let mut command_bytes = command_line.into_bytes();
                command_bytes.push(b'\r');
                terminal.input(command_bytes);
            }) {
                show_workspace_toast_async(
                    &workspace_handle,
                    format!("Failed to launch {} in terminal: {error}", preset.label),
                    cx,
                );
                return Ok::<(), anyhow::Error>(());
            }

            let _ = update_store_async(&store, cx, |store, cx| {
                store.set_workspace_attention(
                    &workspace_entry.id,
                    WorkspaceAttentionStatus::Working,
                    false,
                    None,
                    cx,
                );
            });

            Ok::<(), anyhow::Error>(())
        })
        .detach();
}

#[cfg(feature = "acp_tabs")]
enum AcpAgentRegistrationWaitResult {
    Registered,
    RegistryFetchFailed(String),
    TimedOut,
}

#[cfg(feature = "acp_tabs")]
async fn wait_for_acp_agent_registration(
    workspace_handle: &Entity<Workspace>,
    agent_name: &str,
    cx: &mut AsyncWindowContext,
) -> AcpAgentRegistrationWaitResult {
    for _ in 0..150 {
        let state = workspace_handle.read_with(cx, |workspace, cx| {
            let agent_server_store = workspace.project().read(cx).agent_server_store().clone();
            if agent_server_store
                .read(cx)
                .external_agents()
                .any(|registered_name| registered_name.0.as_ref() == agent_name)
            {
                return AcpAgentRegistrationWaitResult::Registered;
            }

            let Some(registry_store) = AgentRegistryStore::try_global(cx) else {
                return AcpAgentRegistrationWaitResult::TimedOut;
            };
            let registry_store = registry_store.read(cx);
            let agent_id = AgentId::new(agent_name.to_string());
            if let Some(error) = registry_store.fetch_error()
                && !registry_store.is_fetching()
                && registry_store.agent(&agent_id).is_none()
            {
                return AcpAgentRegistrationWaitResult::RegistryFetchFailed(error.to_string());
            }

            AcpAgentRegistrationWaitResult::TimedOut
        });
        match state {
            AcpAgentRegistrationWaitResult::Registered => {
                return AcpAgentRegistrationWaitResult::Registered;
            }
            AcpAgentRegistrationWaitResult::RegistryFetchFailed(error) => {
                return AcpAgentRegistrationWaitResult::RegistryFetchFailed(error);
            }
            AcpAgentRegistrationWaitResult::TimedOut => {}
        }

        cx.background_executor()
            .timer(Duration::from_millis(200))
            .await;
    }

    AcpAgentRegistrationWaitResult::TimedOut
}

fn launch_workspace_preset_task(
    workspace_handle: Entity<Workspace>,
    workspace_entry: WorkspaceEntry,
    preset: AgentPreset,
    task_prompt: Option<String>,
    window: &mut Window,
    cx: &mut App,
) {
    let store = SuperzentStore::global(cx);
    let session = store.update(cx, |store, cx| {
        store.start_session(
            &workspace_entry.id,
            &preset,
            session_label_for_prompt(&preset, task_prompt.as_deref()),
            cx,
        )
    });

    let Some(terminal_panel) = workspace_handle.read(cx).panel::<TerminalPanel>(cx) else {
        let reason = "Terminal panel is unavailable.".to_string();
        store.update(cx, |store, cx| {
            store.update_session_status(&session.id, TaskStatus::Failed, Some(reason.clone()), cx);
        });
        show_workspace_toast(&workspace_handle, reason, cx);
        return;
    };

    let spawn_in_terminal =
        match superzent_agent::spawn_for_workspace(&workspace_entry, &session, &preset) {
            Ok(spawn_in_terminal) => spawn_in_terminal,
            Err(error) => {
                let reason = format!("Failed to prepare {}: {error}", preset.label);
                store.update(cx, |store, cx| {
                    store.update_session_status(
                        &session.id,
                        TaskStatus::Failed,
                        Some(reason.clone()),
                        cx,
                    );
                });
                show_workspace_toast(&workspace_handle, reason, cx);
                return;
            }
        };
    let spawn_task = terminal_panel.update(cx, |terminal_panel, cx| {
        terminal_panel.spawn_task(&spawn_in_terminal, window, cx)
    });

    window
        .spawn(cx, async move |cx| {
            let terminal = match spawn_task.await {
                Ok(terminal) => {
                    if update_store_async(&store, cx, |store, cx| {
                        store.update_session_status(&session.id, TaskStatus::Running, None, cx);
                        if let Some((attention_status, review_pending)) =
                            workspace_attention_for_terminal_status(&TaskStatus::Running)
                        {
                            store.set_workspace_attention(
                                &workspace_entry.id,
                                attention_status,
                                review_pending,
                                None,
                                cx,
                            );
                        }
                    })
                    .is_none()
                    {
                        return Ok::<(), anyhow::Error>(());
                    }
                    terminal
                }
                Err(error) => {
                    let reason = format!("Failed to launch {}: {error}", preset.label);
                    if update_store_async(&store, cx, |store, cx| {
                        store.update_session_status(
                            &session.id,
                            TaskStatus::Failed,
                            Some(reason.clone()),
                            cx,
                        );
                    })
                    .is_none()
                    {
                        return Ok::<(), anyhow::Error>(());
                    }
                    show_workspace_toast_async(&workspace_handle, reason, cx);
                    return Ok::<(), anyhow::Error>(());
                }
            };

            if let Some(task_prompt) = task_prompt.clone() {
                if let Err(error) = terminal.update_in(cx, |terminal, _, _| {
                    let prompt = format!("{task_prompt}\n");
                    terminal.input(prompt.into_bytes());
                }) {
                    let reason = format!("Failed to send initial prompt: {error}");
                    if update_store_async(&store, cx, |store, cx| {
                        store.update_session_status(
                            &session.id,
                            TaskStatus::Failed,
                            Some(reason.clone()),
                            cx,
                        );
                        if let Some((attention_status, review_pending)) =
                            workspace_attention_for_terminal_status(&TaskStatus::Failed)
                        {
                            store.set_workspace_attention(
                                &workspace_entry.id,
                                attention_status,
                                review_pending,
                                workspace_attention_reason_for_terminal_status(
                                    &TaskStatus::Failed,
                                    Some(reason.clone()),
                                ),
                                cx,
                            );
                        }
                    })
                    .is_none()
                    {
                        return Ok::<(), anyhow::Error>(());
                    }
                    show_workspace_toast_async(&workspace_handle, reason, cx);
                    return Ok::<(), anyhow::Error>(());
                }
            }

            let exit_status = match terminal
                .update_in(cx, |terminal, _, cx| terminal.wait_for_completed_task(cx))
            {
                Ok(wait_task) => wait_task.await,
                Err(error) => {
                    let reason = format!("Terminal closed before the session started: {error}");
                    if update_store_async(&store, cx, |store, cx| {
                        store.update_session_status(
                            &session.id,
                            TaskStatus::Failed,
                            Some(reason.clone()),
                            cx,
                        );
                    })
                    .is_none()
                    {
                        return Ok::<(), anyhow::Error>(());
                    }
                    show_workspace_toast_async(&workspace_handle, reason, cx);
                    return Ok::<(), anyhow::Error>(());
                }
            };

            let (status, reason) = match exit_status {
                Some(exit_status) if exit_status.success() => (TaskStatus::Completed, None),
                Some(exit_status) => (
                    TaskStatus::Failed,
                    Some(
                        exit_status
                            .code()
                            .map(|code| format!("{} exited with code {code}.", preset.label))
                            .unwrap_or_else(|| {
                                format!("{} exited with an unknown error.", preset.label)
                            }),
                    ),
                ),
                None => (
                    TaskStatus::Failed,
                    Some(format!(
                        "{} terminated without an exit status.",
                        preset.label
                    )),
                ),
            };

            if update_store_async(&store, cx, |store, cx| {
                store.update_session_status(&session.id, status.clone(), reason.clone(), cx);
                if let Some((attention_status, review_pending)) =
                    workspace_attention_for_terminal_status(&status)
                {
                    store.set_workspace_attention(
                        &workspace_entry.id,
                        attention_status,
                        review_pending,
                        workspace_attention_reason_for_terminal_status(&status, reason.clone()),
                        cx,
                    );
                }
            })
            .is_none()
            {
                return Ok::<(), anyhow::Error>(());
            }
            if let Some(reason) = reason {
                show_workspace_toast_async(&workspace_handle, reason, cx);
            }

            Ok::<(), anyhow::Error>(())
        })
        .detach();
}

fn session_label_for_prompt(preset: &AgentPreset, task_prompt: Option<&str>) -> String {
    let Some(task_prompt) = task_prompt
        .map(str::trim)
        .filter(|task_prompt| !task_prompt.is_empty())
    else {
        return preset.label.clone();
    };

    let preview = task_prompt.lines().next().unwrap_or(task_prompt);
    let preview = if preview.chars().count() > 48 {
        let truncated = preview.chars().take(45).collect::<String>();
        format!("{truncated}...")
    } else {
        preview.to_string()
    };

    format!("{}: {}", preset.label, preview)
}

fn preset_shell_kind(workspace: &Workspace, workspace_path: &PathBuf, cx: &App) -> ShellKind {
    let project = workspace.project();
    let project = project.read(cx);
    let shell = project
        .remote_client()
        .and_then(|remote_client| remote_client.read(cx).shell().map(Shell::Program))
        .unwrap_or_else(|| {
            project
                .terminal_settings(&Some(workspace_path.clone()), cx)
                .shell
                .clone()
        });

    shell.shell_kind(project.path_style(cx).is_windows())
}

fn render_preset_command_line(command: &str, args: &[String], shell_kind: ShellKind) -> String {
    if args.is_empty() {
        return command.to_string();
    }

    let mut parts = Vec::with_capacity(args.len() + 1);
    parts.push(command.to_string());
    parts.extend(args.iter().map(|argument| {
        shell_kind
            .try_quote(argument)
            .map(|value| value.into_owned())
            .unwrap_or_else(|| argument.clone())
    }));
    parts.join(" ")
}

fn show_workspace_toast(
    workspace_handle: &Entity<Workspace>,
    message: impl Into<SharedString>,
    cx: &mut App,
) {
    let message = message.into().to_string();
    workspace_handle.update(cx, |workspace, cx| {
        workspace.show_toast(
            Toast::new(NotificationId::unique::<SuperzentSidebar>(), message),
            cx,
        );
    });
}

fn show_workspace_toast_async(
    workspace_handle: &Entity<Workspace>,
    message: impl Into<SharedString>,
    cx: &mut AsyncWindowContext,
) {
    let message: SharedString = message.into();
    if let Err(error) = cx.update(|_, cx| {
        show_workspace_toast(workspace_handle, message.clone(), cx);
    }) {
        log::error!("failed to show workspace toast: {error:#}");
    }
}

fn show_workspace_status_toast(
    workspace_handle: &Entity<Workspace>,
    message: impl Into<SharedString>,
    icon: ToastIcon,
    cx: &mut App,
) {
    let message: SharedString = message.into();
    let status_toast = StatusToast::new(message, cx, move |this: StatusToast, _cx| {
        this.icon(icon).dismiss_button(true)
    });
    workspace_handle.update(cx, |workspace, cx| {
        workspace.toggle_status_toast(status_toast, cx);
    });
}

fn clear_workspace_setup_attention(
    store: &mut SuperzentStore,
    workspace_id: &str,
    cx: &mut Context<SuperzentStore>,
) {
    let should_clear = store
        .workspace(workspace_id)
        .and_then(|workspace| workspace.last_attention_reason.as_deref())
        == Some(WORKSPACE_SETUP_IN_PROGRESS_REASON);

    if should_clear {
        store.set_workspace_attention(
            workspace_id,
            WorkspaceAttentionStatus::Idle,
            false,
            None,
            cx,
        );
    }
}

fn can_mark_workspace_setup_attention(workspace: &WorkspaceEntry) -> bool {
    workspace.attention_status == WorkspaceAttentionStatus::Idle && !workspace.review_pending
}

fn mark_workspace_setup_attention(
    store: &mut SuperzentStore,
    workspace_id: &str,
    cx: &mut Context<SuperzentStore>,
) {
    let should_mark = store
        .workspace(workspace_id)
        .is_some_and(can_mark_workspace_setup_attention);

    if should_mark {
        store.set_workspace_attention(
            workspace_id,
            WorkspaceAttentionStatus::Working,
            false,
            Some(WORKSPACE_SETUP_IN_PROGRESS_REASON.to_string()),
            cx,
        );
    }
}

fn show_current_window_status_toast(
    message: impl Into<SharedString>,
    icon: ToastIcon,
    cx: &mut AsyncWindowContext,
) {
    if let Err(error) = cx.update(|window, cx| {
        let Some(workspace) = workspace_from_window(window, cx) else {
            return;
        };
        show_workspace_status_toast(&workspace, message, icon, cx);
    }) {
        log::error!("failed to show current window status toast: {error:#}");
    }
}

fn update_store_async<R>(
    store: &Entity<SuperzentStore>,
    cx: &mut AsyncWindowContext,
    update: impl FnOnce(&mut SuperzentStore, &mut Context<SuperzentStore>) -> R,
) -> Option<R> {
    match cx.update(|_, cx| store.update(cx, update)) {
        Ok(result) => Some(result),
        Err(error) => {
            log::error!("failed to update Superzent store: {error:#}");
            None
        }
    }
}

struct WorkspaceCreationResult {
    workspace: WorkspaceEntry,
    notice: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct NewWorkspaceModalBootstrap {
    base_branch: Option<String>,
    base_branch_notice: Option<String>,
    base_branch_error: Option<String>,
    setup_script: Option<String>,
    teardown_script: Option<String>,
}

fn new_workspace_create_options(
    branch_name: String,
    base_branch_override: Option<String>,
    base_workspace_path: Option<PathBuf>,
    setup_script: Option<String>,
    teardown_script: Option<String>,
    save_teardown_script_as_repo_default: bool,
    allow_dirty: bool,
) -> superzent_git::CreateWorkspaceOptions {
    superzent_git::CreateWorkspaceOptions {
        branch_name,
        base_branch_override,
        base_workspace_path,
        setup_script,
        teardown_script,
        save_teardown_script_as_repo_default,
        allow_dirty,
    }
}

fn new_workspace_modal_bootstrap(
    base_branch_resolution: Result<superzent_git::WorkspaceBaseBranchResolution>,
    lifecycle_defaults: superzent_git::WorkspaceLifecycleDefaults,
) -> NewWorkspaceModalBootstrap {
    let mut bootstrap = NewWorkspaceModalBootstrap {
        setup_script: lifecycle_defaults.setup_script,
        teardown_script: lifecycle_defaults.teardown_script,
        ..Default::default()
    };

    match base_branch_resolution {
        Ok(resolution) => {
            bootstrap.base_branch = Some(resolution.effective_base_branch);
            bootstrap.base_branch_notice = resolution.notice;
        }
        Err(error) => {
            bootstrap.base_branch_error = Some(error.to_string());
        }
    }

    bootstrap
}

fn build_new_workspace_modal_bootstrap(
    project: &ProjectEntry,
    initial_base_workspace_path: Option<&Path>,
) -> NewWorkspaceModalBootstrap {
    if !matches!(project.location, ProjectLocation::Local { .. }) {
        return NewWorkspaceModalBootstrap::default();
    }

    let lifecycle_defaults =
        superzent_git::workspace_lifecycle_defaults(project).unwrap_or_default();
    let base_branch_resolution = superzent_git::resolve_workspace_base_branch_from_workspace(
        project,
        initial_base_workspace_path,
        None,
    );

    new_workspace_modal_bootstrap(base_branch_resolution, lifecycle_defaults)
}

fn workspace_lifecycle_failure_prompt_detail(
    workspace_entry: &WorkspaceEntry,
    failure: &superzent_git::WorkspaceLifecycleFailure,
    notice: Option<&str>,
    force_delete: bool,
) -> String {
    let mut sections = vec![format!(
        "Workspace `{}` at {} hit a {} failure.",
        workspace_entry.name,
        workspace_entry.display_path(),
        failure.phase.label().to_lowercase()
    )];

    if let Some(notice) = notice.filter(|notice| !notice.trim().is_empty()) {
        sections.push(notice.trim().to_string());
    }

    if force_delete {
        sections.push(
            "Choose `Delete Anyway` to retry deletion without running teardown again.".to_string(),
        );
    }

    sections.push(failure.details());
    sections.join("\n\n")
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DeleteWorkspacePromptDetails {
    title: &'static str,
    confirm_label: &'static str,
    confirm_decision: DeleteWorkspaceModalDecision,
    prompt_message: String,
    teardown_message: String,
    teardown_script: Option<String>,
    failure_details: Option<String>,
}

fn delete_workspace_prompt_details(
    workspace_entry: &WorkspaceEntry,
    delete_resolution: &superzent_git::WorkspaceDeleteResolution,
) -> DeleteWorkspacePromptDetails {
    let prompt_message = format!(
        "Delete `{}` and remove its worktree at {}?",
        workspace_entry.name,
        workspace_entry.display_path()
    );

    match delete_resolution {
        superzent_git::WorkspaceDeleteResolution::RunTeardownScript { script } => {
            DeleteWorkspacePromptDetails {
                title: "Delete workspace?",
                confirm_label: "Delete",
                confirm_decision: DeleteWorkspaceModalDecision::Delete,
                prompt_message,
                teardown_message: "This teardown script will run before deletion.".to_string(),
                teardown_script: Some(script.clone()),
                failure_details: None,
            }
        }
        superzent_git::WorkspaceDeleteResolution::SkipTeardown => DeleteWorkspacePromptDetails {
            title: "Delete workspace?",
            confirm_label: "Delete",
            confirm_decision: DeleteWorkspaceModalDecision::Delete,
            prompt_message,
            teardown_message: "No teardown script will run before deletion.".to_string(),
            teardown_script: None,
            failure_details: None,
        },
        superzent_git::WorkspaceDeleteResolution::BlockedByConfig(failure) => {
            DeleteWorkspacePromptDetails {
                title: "Delete blocked",
                confirm_label: "Delete Anyway",
                confirm_decision: DeleteWorkspaceModalDecision::DeleteAnyway,
                prompt_message,
                teardown_message:
                    "Superzent could not read `.superzent/config.json`, so normal delete is blocked. If you continue, teardown will be skipped."
                        .to_string(),
                teardown_script: None,
                failure_details: Some(failure.details()),
            }
        }
    }
}

const WORKSPACE_SETUP_IN_PROGRESS_REASON: &str = "Setting up workspace…";
fn workspace_setup_row_state(workspace: &WorkspaceEntry) -> Option<WorkspaceSetupRowState> {
    match workspace.last_attention_reason.as_deref() {
        Some(WORKSPACE_SETUP_IN_PROGRESS_REASON) => Some(WorkspaceSetupRowState::InProgress),
        _ => None,
    }
}

async fn resolve_remote_project_workspace(
    project: &ProjectEntry,
    store: &Entity<SuperzentStore>,
    app_state: Arc<WorkspaceAppState>,
    require_primary_workspace: bool,
    cx: &mut AsyncWindowContext,
) -> anyhow::Result<(Entity<Workspace>, WorkspaceEntry)> {
    let primary_workspace = cx
        .update(|_, cx| {
            store
                .read(cx)
                .primary_workspace_for_project(&project.id)
                .cloned()
        })?
        .ok_or_else(|| anyhow::anyhow!("missing primary workspace for remote project"))?;

    let primary_live_workspace = cx.update(|window, cx| {
        workspace_for_entry_in_window(window, cx, &primary_workspace)
            .or_else(|| workspace_for_entry_in_any_window(&primary_workspace, cx))
    })?;

    let project_live_workspace = cx.update(|window, cx| {
        let store = store.read(cx);
        store
            .workspaces_for_project(&project.id)
            .into_iter()
            .find_map(|workspace_entry| workspace_for_entry_in_window(window, cx, workspace_entry))
            .or_else(|| workspace_for_project_in_any_window(&project.id, &store, cx))
    })?;

    if let Some(live_workspace) = primary_live_workspace.or_else(|| {
        (!require_primary_workspace)
            .then_some(project_live_workspace)
            .flatten()
    }) {
        return Ok((live_workspace, primary_workspace));
    }

    let open_task = cx.update(|window, cx| {
        open_workspace_entry(primary_workspace.clone(), app_state, window, cx)
    })?;
    open_task.await?;

    let live_workspace = cx
        .update(|window, cx| workspace_for_entry_in_window(window, cx, &primary_workspace))?
        .ok_or_else(|| anyhow::anyhow!("failed to resolve remote project workspace"))?;

    Ok((live_workspace, primary_workspace))
}

async fn create_remote_workspace(
    project: ProjectEntry,
    branch_name: String,
    preset_id: String,
    store: Entity<SuperzentStore>,
    app_state: Arc<WorkspaceAppState>,
    cx: &mut AsyncWindowContext,
) -> anyhow::Result<WorkspaceCreationResult> {
    let connection = match &project.location {
        ProjectLocation::Ssh { connection, .. } => connection.clone(),
        ProjectLocation::Local { .. } => {
            return Err(anyhow::anyhow!(
                "remote workspace creation requires an SSH project"
            ));
        }
    };

    let (project_workspace, _) =
        resolve_remote_project_workspace(&project, &store, app_state, false, cx).await?;

    let repository = cx.update(|_, cx| {
        active_repository_for_workspace(&project_workspace, cx)
            .ok_or_else(|| anyhow::anyhow!("no active repository found"))
    })??;

    let (receiver, worktree_path) = repository.update(cx, |repository, cx| {
        let worktree_directory_setting = ProjectSettings::get_global(cx)
            .git
            .worktree_directory
            .clone();
        let directory = validate_worktree_directory(
            &repository.original_repo_abs_path,
            &worktree_directory_setting,
        )?;
        let worktree_path = directory.join(&branch_name);
        let receiver = repository.create_worktree(branch_name.clone(), directory, None);
        anyhow::Ok((receiver, worktree_path))
    })?;
    receiver.await??;

    let workspace_location = WorkspaceLocation::Ssh {
        connection,
        worktree_path: worktree_path.to_string_lossy().into_owned(),
    };

    let workspace = cx.update(|_, cx| {
        let existing_workspace = store
            .read(cx)
            .workspace_for_location(&workspace_location)
            .cloned();
        let now = Utc::now();

        WorkspaceEntry {
            id: existing_workspace
                .as_ref()
                .map(|workspace| workspace.id.clone())
                .unwrap_or_else(|| Uuid::new_v4().to_string()),
            project_id: project.id.clone(),
            kind: WorkspaceKind::Worktree,
            name: branch_name.clone(),
            display_name: existing_workspace
                .as_ref()
                .and_then(|workspace| workspace.display_name.clone()),
            branch: branch_name.clone(),
            location: workspace_location,
            agent_preset_id: existing_workspace
                .as_ref()
                .map(|workspace| workspace.agent_preset_id.clone())
                .unwrap_or(preset_id),
            managed: true,
            git_status: WorkspaceGitStatus::Available,
            git_summary: existing_workspace
                .as_ref()
                .and_then(|workspace| workspace.git_summary.clone()),
            attention_status: existing_workspace
                .as_ref()
                .map(|workspace| workspace.attention_status.clone())
                .unwrap_or(WorkspaceAttentionStatus::Idle),
            review_pending: existing_workspace
                .as_ref()
                .is_some_and(|workspace| workspace.review_pending),
            last_attention_reason: existing_workspace
                .as_ref()
                .and_then(|workspace| workspace.last_attention_reason.clone()),
            teardown_script_override: existing_workspace
                .as_ref()
                .and_then(|workspace| workspace.teardown_script_override.clone()),
            created_at: existing_workspace
                .as_ref()
                .map(|workspace| workspace.created_at)
                .unwrap_or(now),
            last_opened_at: now,
        }
    })?;

    Ok(WorkspaceCreationResult {
        workspace,
        notice: None,
    })
}

async fn resolve_opened_workspace(
    workspace_entry: &WorkspaceEntry,
    _window_handle: gpui::AnyWindowHandle,
    cx: &mut AsyncWindowContext,
) -> Option<Entity<Workspace>> {
    for _ in 0..20 {
        let resolved = cx
            .update(|window, cx| workspace_for_entry_in_window(window, cx, workspace_entry))
            .ok()
            .flatten();

        if resolved.is_some() {
            return resolved;
        }

        cx.background_executor()
            .timer(Duration::from_millis(100))
            .await;
    }

    cx.update(|window, cx| workspace_from_window(window, cx))
        .ok()
        .flatten()
}

async fn create_local_workspace(
    project: ProjectEntry,
    preset_id: String,
    create_options: superzent_git::CreateWorkspaceOptions,
    cx: &mut AsyncWindowContext,
) -> anyhow::Result<WorkspaceCreationResult> {
    cx.background_spawn(async move {
        superzent_git::create_workspace_without_setup(&project, &preset_id, create_options).map(
            |outcome| WorkspaceCreationResult {
                workspace: outcome.workspace,
                notice: outcome.notice,
            },
        )
    })
    .await
}

fn local_base_workspace_path_for_create_request(
    workspace_handle: &Entity<Workspace>,
    project: &ProjectEntry,
    store: &SuperzentStore,
    cx: &App,
) -> Option<PathBuf> {
    single_workspace_location_snapshot(workspace_handle, cx)
        .and_then(|location| match location {
            WorkspaceLocation::Local { worktree_path } => Some(worktree_path),
            WorkspaceLocation::Ssh { .. } => None,
        })
        .or_else(|| {
            store
                .active_workspace()
                .filter(|workspace_entry| workspace_entry.project_id == project.id)
                .and_then(|workspace_entry| workspace_entry.local_worktree_path())
                .map(Path::to_path_buf)
        })
        .or_else(|| {
            workspace_handle
                .read(cx)
                .project()
                .read(cx)
                .visible_worktrees(cx)
                .next()
                .and_then(|worktree| {
                    worktree
                        .read(cx)
                        .as_local()
                        .map(|local| local.abs_path().to_path_buf())
                })
        })
}

fn move_changes_source_path(
    base_workspace_path: Option<&Path>,
    project: &ProjectEntry,
) -> Option<PathBuf> {
    base_workspace_path
        .map(Path::to_path_buf)
        .or_else(|| project.local_repo_root().map(Path::to_path_buf))
}

fn is_dirty_workspace_create_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .to_string()
            .contains("commit or stash local changes before creating a managed workspace")
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DirtyWorkspaceCreateChoice {
    CreateOnly,
    CreateAndMoveChanges,
}

fn spawn_new_workspace_request(
    workspace_handle: Entity<Workspace>,
    app_state: Arc<WorkspaceAppState>,
    project: ProjectEntry,
    branch_name: String,
    base_branch_override: Option<String>,
    setup_script: Option<String>,
    teardown_script: Option<String>,
    save_teardown_script_as_repo_default: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let store = SuperzentStore::global(cx);
    let preset_id = store.read(cx).default_preset().id.clone();
    let base_workspace_path = {
        let store = store.read(cx);
        local_base_workspace_path_for_create_request(&workspace_handle, &project, &store, cx)
    };
    window
        .spawn(cx, async move |cx| {
            let mut move_changes_failure: Option<String> = None;
            let outcome = match &project.location {
                ProjectLocation::Local { .. } => {
                    match create_local_workspace(
                        project.clone(),
                        preset_id.clone(),
                        new_workspace_create_options(
                            branch_name.clone(),
                            base_branch_override.clone(),
                            base_workspace_path.clone(),
                            setup_script.clone(),
                            teardown_script.clone(),
                            save_teardown_script_as_repo_default,
                            false,
                        ),
                        cx,
                    )
                    .await
                    {
                        Ok(outcome) => Ok(outcome),
                        Err(error) if is_dirty_workspace_create_error(&error) => {
                            let prompt = cx.update(|window, cx| {
                                window.prompt(
                                    PromptLevel::Warning,
                                    "Create workspace with local changes?",
                                    Some(
                                        "The current workspace has uncommitted changes.\n\n`Create Only` makes the new workspace from the selected base branch and leaves your current changes where they are.\n\n`Create And Move Changes` makes the new workspace and then moves your tracked and untracked local changes into it.",
                                    ),
                                    &["Cancel", "Create Only", "Create And Move Changes"],
                                    cx,
                                )
                            })?;

                            let dirty_choice = match prompt.await {
                                Ok(1) => Some(DirtyWorkspaceCreateChoice::CreateOnly),
                                Ok(2) => Some(DirtyWorkspaceCreateChoice::CreateAndMoveChanges),
                                _ => None,
                            };

                            let Some(dirty_choice) = dirty_choice else {
                                return Ok::<(), anyhow::Error>(());
                            };

                            let outcome = create_local_workspace(
                                project.clone(),
                                preset_id.clone(),
                                new_workspace_create_options(
                                    branch_name.clone(),
                                    base_branch_override.clone(),
                                    base_workspace_path.clone(),
                                    setup_script.clone(),
                                    teardown_script.clone(),
                                    save_teardown_script_as_repo_default,
                                    true,
                                ),
                                cx,
                            )
                            .await?;

                            if dirty_choice == DirtyWorkspaceCreateChoice::CreateAndMoveChanges {
                                let source_workspace_path = move_changes_source_path(
                                    base_workspace_path.as_deref(),
                                    &project,
                                );
                                let target_worktree_path = outcome
                                    .workspace
                                    .local_worktree_path()
                                    .map(Path::to_path_buf);

                                if let (Some(source_workspace_path), Some(target_worktree_path)) =
                                    (source_workspace_path, target_worktree_path)
                                {
                                    if let Err(error) = cx
                                        .background_spawn(async move {
                                            superzent_git::move_changes_to_workspace(
                                                &source_workspace_path,
                                                &target_worktree_path,
                                            )
                                        })
                                        .await
                                    {
                                        move_changes_failure = Some(error.to_string());
                                    }
                                }
                            }

                            Ok(outcome)
                        }
                        Err(error) => Err(error),
                    }
                }
                ProjectLocation::Ssh { .. } => {
                    create_remote_workspace(
                        project.clone(),
                        branch_name.clone(),
                        preset_id.clone(),
                        store.clone(),
                        app_state.clone(),
                        cx,
                    )
                    .await
                }
            };

            let outcome = match outcome {
                Ok(outcome) => outcome,
                Err(error) => {
                    show_workspace_toast_async(
                        &workspace_handle,
                        format!("Failed to create workspace: {error}"),
                        cx,
                    );
                    return Ok::<(), anyhow::Error>(());
                }
            };

            let workspace_entry = outcome.workspace.clone();

            if update_store_async(&store, cx, |store, cx| {
                store.upsert_workspace(workspace_entry.clone(), cx);
                store.record_workspace_opened(&workspace_entry.id, cx);
            })
            .is_none()
            {
                return Ok::<(), anyhow::Error>(());
            }

            let open_task = cx.update(|window, cx| {
                open_workspace_entry(workspace_entry.clone(), app_state.clone(), window, cx)
            })?;
            if let Err(error) = open_task.await {
                show_workspace_toast_async(
                    &workspace_handle,
                    format!("Failed to open workspace: {error}"),
                    cx,
                );
                return Ok::<(), anyhow::Error>(());
            }

            let current_window_handle = cx.update(|window, _| window.window_handle())?;
            let visible_workspace =
                resolve_opened_workspace(&workspace_entry, current_window_handle, cx).await;

            if let (Some(visible_workspace), Some(notice)) =
                (visible_workspace.as_ref(), outcome.notice.clone())
            {
                cx.update(|_, cx| {
                    show_workspace_toast(&visible_workspace, notice, cx);
                })?;
            }

            let should_launch_preset = if matches!(project.location, ProjectLocation::Local { .. }) {
                let project = project.clone();
                let workspace_entry_for_setup = workspace_entry.clone();
                let base_workspace_path = base_workspace_path.clone();
                let setup_script = setup_script.clone();
                if setup_script
                    .as_ref()
                    .map(|script| !script.trim().is_empty())
                    .unwrap_or(false)
                {
                    let _ = update_store_async(&store, cx, |store, cx| {
                        mark_workspace_setup_attention(store, &workspace_entry.id, cx);
                    });
                    let setup_result = cx
                        .background_spawn(async move {
                            superzent_git::run_workspace_setup(
                                &project,
                                &workspace_entry_for_setup,
                                base_workspace_path.as_deref(),
                                setup_script.as_deref(),
                            )
                        })
                        .await;
                    // Always resolve the currently *active* workspace at completion
                    // time: the handle captured before setup may now point to a
                    // hidden tab if the user navigated away, and toasts attached
                    // to a hidden tab are never drawn. When nothing is active for
                    // this entry we fall back to the current window so the user
                    // still sees the result.
                    //
                    // The current window must be checked via `workspace_from_window`
                    // (using the `Window` arg) because it's already on the stack
                    // inside `cx.update`, so any `read_window`-based lookup
                    // (including `WindowHandle::read_with`) would panic. The
                    // iteration helper uses `WindowHandle::read` which skips the
                    // stack-locked current window gracefully.
                    let setup_visible_workspace = cx
                        .update(|window, cx| {
                            workspace_from_window(window, cx)
                                .filter(|active| {
                                    workspace_matches_entry(active, &workspace_entry, cx)
                                })
                                .or_else(|| {
                                    currently_active_workspace_for_entry(&workspace_entry, cx)
                                })
                        })
                        .ok()
                        .flatten();

                    match setup_result {
                        Ok(()) => {
                            let _ = update_store_async(&store, cx, |store, cx| {
                                clear_workspace_setup_attention(store, &workspace_entry.id, cx);
                            });
                            if let Some(visible_workspace) = setup_visible_workspace.as_ref() {
                                if let Err(error) = cx.update(|_, cx| {
                                    show_workspace_status_toast(
                                        visible_workspace,
                                        format!("Setup finished for `{}`.", workspace_entry.name),
                                        ToastIcon::new(IconName::Check).color(Color::Success),
                                        cx,
                                    );
                                }) {
                                    log::error!("failed to show workspace setup success toast: {error:#}");
                                }
                            } else {
                                show_current_window_status_toast(
                                    format!("Setup finished for `{}`.", workspace_entry.name),
                                    ToastIcon::new(IconName::Check).color(Color::Success),
                                    cx,
                                );
                            }
                            true
                        }
                        Err(setup_failure) => {
                            let _ = update_store_async(&store, cx, |store, cx| {
                                clear_workspace_setup_attention(store, &workspace_entry.id, cx);
                            });
                            let prompt_detail = workspace_lifecycle_failure_prompt_detail(
                                &workspace_entry,
                                &setup_failure,
                                outcome.notice.as_deref(),
                                false,
                            );
                            let setup_prompt = cx.update(|window, cx| {
                                // Prefer toasting the workspace where setup ran
                                // when it's still the active tab; otherwise pop
                                // the notice on the current window so the user
                                // sees the failure regardless of navigation.
                                if let Some(visible_workspace) =
                                    setup_visible_workspace.as_ref()
                                {
                                    show_workspace_toast(
                                        visible_workspace,
                                        setup_failure.summary(),
                                        cx,
                                    );
                                } else if let Some(current_workspace) =
                                    workspace_from_window(window, cx)
                                {
                                    show_workspace_toast(
                                        &current_workspace,
                                        setup_failure.summary(),
                                        cx,
                                    );
                                } else {
                                    log::warn!(
                                        "workspace setup failed and no window is available to show the error: {}",
                                        setup_failure.summary()
                                    );
                                }
                                window.prompt(
                                    PromptLevel::Warning,
                                    "Workspace created, but setup failed",
                                    Some(&prompt_detail),
                                    &["OK"],
                                    cx,
                                )
                            })?;
                            let _ = setup_prompt.await;
                            false
                        }
                    }
                } else {
                    true
                }
            } else {
                true
            };

            if should_launch_preset {
                let target_workspace = if let Some(visible_workspace) = visible_workspace.clone() {
                    Some(visible_workspace)
                } else {
                    resolve_opened_workspace(&workspace_entry, current_window_handle, cx).await
                };

                if let Some(target_workspace) = target_workspace {
                    cx.update(|window, cx| {
                        launch_workspace_preset(
                            target_workspace,
                            workspace_entry.clone(),
                            preset_id.clone(),
                            None,
                            window,
                            cx,
                        );
                    })?;
                } else {
                    log::warn!(
                        "workspace opened, but the new workspace view could not be resolved for preset launch"
                    );
                }
            }

            if let Some(move_changes_failure) = move_changes_failure.clone() {
                if let Some(visible_workspace) = visible_workspace.as_ref() {
                    let move_prompt = cx.update(|window, cx| {
                        show_workspace_toast(
                            visible_workspace,
                            "Workspace created, but moving changes failed.",
                            cx,
                        );
                        window.prompt(
                            PromptLevel::Warning,
                            "Workspace created, but moving changes failed",
                            Some(&move_changes_failure),
                            &["OK"],
                            cx,
                        )
                    })?;
                    let _ = move_prompt.await;
                } else {
                    log::warn!(
                        "workspace change move failed before the new workspace view could be resolved: {move_changes_failure}"
                    );
                }
            }

            Ok::<(), anyhow::Error>(())
        })
        .detach();
}

struct AddProjectChooserModal {
    workspace: WeakEntity<Workspace>,
    focus_handle: FocusHandle,
}

impl EventEmitter<DismissEvent> for AddProjectChooserModal {}
impl ModalView for AddProjectChooserModal {}

impl Focusable for AddProjectChooserModal {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.workspace
            .upgrade()
            .map(|workspace| workspace.read(cx).focus_handle(cx))
            .unwrap_or_else(|| self.focus_handle.clone())
    }
}

impl AddProjectChooserModal {
    fn new(workspace: WeakEntity<Workspace>, cx: &mut Context<Self>) -> Self {
        Self {
            workspace,
            focus_handle: cx.focus_handle(),
        }
    }

    fn open_local(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let workspace_handle = self.workspace.clone();
        window.defer(cx, move |window, cx| {
            let Some(workspace_handle) = workspace_handle.upgrade() else {
                return;
            };

            workspace_handle.update(cx, |workspace, workspace_cx| {
                workspace.hide_modal(window, workspace_cx);
                run_add_local_project(workspace, window, workspace_cx);
            });
        });
    }

    fn open_remote(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let workspace_handle = self.workspace.clone();
        window.defer(cx, move |window, cx| {
            let Some(workspace_handle) = workspace_handle.upgrade() else {
                return;
            };

            workspace_handle.update(cx, |workspace, workspace_cx| {
                workspace.hide_modal(window, workspace_cx);
                run_add_remote_project(workspace, window, workspace_cx);
            });
        });
    }
}

impl Render for AddProjectChooserModal {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .key_context("SuperzentAddProjectChooserModal")
            .elevation_3(cx)
            .w(px(420.))
            .overflow_hidden()
            .bg(cx.theme().colors().editor_background)
            .border_1()
            .border_color(cx.theme().colors().border)
            .rounded_lg()
            .child(
                v_flex()
                    .gap_3()
                    .p_4()
                    .child(
                        v_flex()
                            .gap_1()
                            .child(Label::new("Add Project").size(LabelSize::Large))
                            .child(
                                Label::new(
                                    "Choose whether to add a local folder or connect to a remote SSH project.",
                                )
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                            ),
                    )
                    .child(
                        v_flex()
                            .gap_2()
                            .child(
                                Button::new("superzent-add-local-project", "Local Project")
                                    .full_width()
                                    .style(ButtonStyle::Filled)
                                    .start_icon(
                                        Icon::new(IconName::FolderOpen).size(IconSize::Small),
                                    )
                                    .on_click(cx.listener(
                                        |this, _: &ClickEvent, window, cx| {
                                            this.open_local(window, cx);
                                        },
                                    )),
                            )
                            .child(
                                ButtonLike::new("superzent-add-remote-project")
                                    .full_width()
                                    .style(ButtonStyle::Subtle)
                                    .on_click(cx.listener(
                                        |this, _: &ClickEvent, window, cx| {
                                            this.open_remote(window, cx);
                                        },
                                    ))
                                    .child(
                                        div()
                                            .relative()
                                            .w_full()
                                            .child(
                                                h_flex()
                                                    .w_full()
                                                    .justify_center()
                                                    .items_center()
                                                    .gap(DynamicSpacing::Base04.rems(cx))
                                                    .child(
                                                        Icon::new(IconName::Server)
                                                            .size(IconSize::default()),
                                                    )
                                                    .child(Label::new("Remote Project (SSH)")),
                                            )
                                            .child(
                                                h_flex()
                                                    .absolute()
                                                    .right(DynamicSpacing::Base06.rems(cx))
                                                    .top_0()
                                                    .h_full()
                                                    .items_center()
                                                    .child(
                                                        Chip::new("experimental")
                                                            .label_color(Color::Accent),
                                                    ),
                                            ),
                                    ),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .justify_end()
                    .px_4()
                    .pb_4()
                    .child(
                        Button::new("superzent-add-project-cancel", "Cancel")
                            .style(ButtonStyle::Subtle)
                            .on_click(cx.listener(|_, _: &ClickEvent, _, cx| {
                                cx.emit(DismissEvent);
                            })),
                    ),
            )
    }
}

struct NewWorkspaceModal {
    workspace: WeakEntity<Workspace>,
    project: ProjectEntry,
    branch_name_editor: Entity<Editor>,
    base_branch_editor: Entity<Editor>,
    setup_script_editor: Entity<Editor>,
    teardown_script_editor: Entity<Editor>,
    show_more_options: bool,
    save_teardown_script_as_repo_default: bool,
    active_script_target: Option<ScriptEditorTarget>,
    scroll_handle: ScrollHandle,
    base_branch_notice: Option<SharedString>,
    base_branch_error: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
    last_error: Option<SharedString>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ScriptEditorTarget {
    Setup,
    Teardown,
}

impl EventEmitter<DismissEvent> for NewWorkspaceModal {}
impl ModalView for NewWorkspaceModal {}

impl Focusable for NewWorkspaceModal {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.branch_name_editor.focus_handle(cx)
    }
}

impl NewWorkspaceModal {
    fn new(
        workspace: WeakEntity<Workspace>,
        project: ProjectEntry,
        bootstrap: NewWorkspaceModalBootstrap,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let branch_name_editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("feature/my-branch", window, cx);
            editor
        });
        let base_branch_editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("main", window, cx);
            editor
        });
        let setup_script_editor = cx.new(|cx| {
            let mut editor = Editor::auto_height(2, 6, window, cx);
            editor.set_placeholder_text("cp \"$SUPERZENT_BASE_PATH\"/.env .env", window, cx);
            editor
        });
        let teardown_script_editor = cx.new(|cx| {
            let mut editor = Editor::auto_height(2, 6, window, cx);
            editor.set_placeholder_text("cargo clean", window, cx);
            editor
        });

        let mut subscriptions = Vec::new();
        subscriptions.push(cx.subscribe(
            &branch_name_editor,
            |this, _, event: &EditorEvent, cx| {
                let EditorEvent::Edited { .. } = event else {
                    return;
                };
                if this.last_error.take().is_some() {
                    cx.notify();
                }
            },
        ));
        subscriptions.push(cx.subscribe(
            &base_branch_editor,
            |this, _, event: &EditorEvent, cx| {
                let EditorEvent::Edited { .. } = event else {
                    return;
                };
                if this.last_error.take().is_some() {
                    cx.notify();
                }
                if this.base_branch_notice.take().is_some()
                    || this.base_branch_error.take().is_some()
                {
                    cx.notify();
                }
            },
        ));
        subscriptions.push(cx.subscribe(
            &setup_script_editor,
            |this, _, event: &EditorEvent, cx| match event {
                EditorEvent::Edited { .. } => {
                    if this.last_error.take().is_some() {
                        cx.notify();
                    }
                }
                EditorEvent::Focused | EditorEvent::FocusedIn => {
                    this.active_script_target = Some(ScriptEditorTarget::Setup);
                    cx.notify();
                }
                EditorEvent::Blurred => {
                    if this.active_script_target == Some(ScriptEditorTarget::Setup) {
                        this.active_script_target = None;
                        cx.notify();
                    }
                }
                _ => {}
            },
        ));
        subscriptions.push(cx.subscribe(
            &teardown_script_editor,
            |this, _, event: &EditorEvent, cx| match event {
                EditorEvent::Edited { .. } => {
                    if this.last_error.take().is_some() {
                        cx.notify();
                    }
                }
                EditorEvent::Focused | EditorEvent::FocusedIn => {
                    this.active_script_target = Some(ScriptEditorTarget::Teardown);
                    cx.notify();
                }
                EditorEvent::Blurred => {
                    if this.active_script_target == Some(ScriptEditorTarget::Teardown) {
                        this.active_script_target = None;
                        cx.notify();
                    }
                }
                _ => {}
            },
        ));

        if let Some(base_branch) = bootstrap.base_branch.as_deref() {
            base_branch_editor.update(cx, |editor, cx| {
                editor.set_text(base_branch, window, cx);
            });
        }
        if let Some(setup_script) = bootstrap.setup_script.as_deref() {
            setup_script_editor.update(cx, |editor, cx| {
                editor.set_text(setup_script, window, cx);
            });
        }
        if let Some(teardown_script) = bootstrap.teardown_script.as_deref() {
            teardown_script_editor.update(cx, |editor, cx| {
                editor.set_text(teardown_script, window, cx);
            });
        }

        Self {
            workspace,
            project,
            branch_name_editor,
            base_branch_editor,
            setup_script_editor,
            teardown_script_editor,
            show_more_options: false,
            save_teardown_script_as_repo_default: true,
            active_script_target: None,
            scroll_handle: ScrollHandle::new(),
            base_branch_notice: bootstrap.base_branch_notice.map(Into::into),
            base_branch_error: bootstrap.base_branch_error.map(Into::into),
            _subscriptions: subscriptions,
            last_error: None,
        }
    }

    fn base_branch(&self, cx: &App) -> Option<String> {
        let base_branch = self.base_branch_editor.read(cx).text(cx);
        let base_branch = base_branch.trim();
        if base_branch.is_empty() {
            None
        } else {
            Some(base_branch.to_string())
        }
    }

    fn setup_script(&self, cx: &App) -> Option<String> {
        let setup_script = self.setup_script_editor.read(cx).text(cx);
        let setup_script = setup_script.trim();
        if setup_script.is_empty() {
            None
        } else {
            Some(setup_script.to_string())
        }
    }

    fn teardown_script(&self, cx: &App) -> Option<String> {
        let teardown_script = self.teardown_script_editor.read(cx).text(cx);
        let teardown_script = teardown_script.trim();
        if teardown_script.is_empty() {
            None
        } else {
            Some(teardown_script.to_string())
        }
    }

    fn insert_variable(&mut self, variable: &str, window: &mut Window, cx: &mut Context<Self>) {
        let target = self
            .active_script_target
            .unwrap_or(ScriptEditorTarget::Setup);
        let editor = match target {
            ScriptEditorTarget::Setup => self.setup_script_editor.clone(),
            ScriptEditorTarget::Teardown => self.teardown_script_editor.clone(),
        };

        editor.update(cx, |editor, cx| {
            let existing_text = editor.text(cx);
            let separator = if existing_text.is_empty()
                || existing_text.ends_with(' ')
                || existing_text.ends_with('\n')
            {
                ""
            } else {
                " "
            };
            let next_text = format!("{existing_text}{separator}{variable}");
            editor.set_text(next_text.as_str(), window, cx);
            editor.focus_handle(cx).focus(window, cx);
        });
    }

    fn render_variable_helper(
        &self,
        id: &'static str,
        variable: &'static str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        h_flex()
            .gap_1()
            .items_center()
            .child(
                ButtonLike::new(format!("{id}-insert"))
                    .style(ButtonStyle::Subtle)
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.insert_variable(variable, window, cx);
                    }))
                    .child(Chip::new(variable).label_color(Color::Accent)),
            )
            .child(
                CopyButton::new(format!("{id}-copy"), variable)
                    .tooltip_label("Copy variable")
                    .icon_size(IconSize::XSmall),
            )
    }

    fn cancel(&mut self, _: &menu::Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }

    fn confirm(&mut self, _: &menu::Confirm, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace_handle) = self.workspace.upgrade() else {
            self.last_error = Some("The workspace is no longer available.".into());
            cx.notify();
            return;
        };

        let branch_name = self.branch_name_editor.read(cx).text(cx);
        let branch_name = branch_name.trim().to_string();
        if branch_name.is_empty() {
            self.last_error = Some("Enter a branch name.".into());
            cx.notify();
            return;
        }

        if let Some(base_branch_error) = self.base_branch_error.clone() {
            self.last_error = Some(base_branch_error);
            cx.notify();
            return;
        }

        let app_state = workspace_handle.read(cx).app_state().clone();

        spawn_new_workspace_request(
            workspace_handle,
            app_state,
            self.project.clone(),
            branch_name,
            self.base_branch(cx),
            self.setup_script(cx),
            self.teardown_script(cx),
            self.save_teardown_script_as_repo_default,
            window,
            cx,
        );

        cx.emit(DismissEvent);
    }
}

impl Render for NewWorkspaceModal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .key_context("SuperzentNewWorkspaceModal")
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::confirm))
            .elevation_3(cx)
            .w(px(480.))
            .max_h(vh(0.8, window))
            .overflow_hidden()
            .child(
                Modal::new("superzent-new-workspace-modal", Some(self.scroll_handle.clone()))
                    .header(
                        ModalHeader::new()
                            .headline("Create Workspace")
                            .description(format!(
                                "Create a managed workspace for {}.",
                                self.project.name
                            )),
                    )
                    .section(
                        Section::new().child(
                            v_flex()
                                .gap_3()
                                .child(
                                    v_flex()
                                        .gap_1()
                                        .child(Label::new("Branch Name").size(LabelSize::Small))
                                        .child(self.branch_name_editor.clone()),
                                )
                                .when(
                                    matches!(self.project.location, ProjectLocation::Local { .. }),
                                    |this| {
                                        this.child(
                                            v_flex()
                                                .gap_1()
                                                .child(Label::new("Base Branch").size(LabelSize::Small))
                                                .child(self.base_branch_editor.clone())
                                                .when_some(self.base_branch_notice.clone(), |this, notice| {
                                                    this.child(
                                                        Label::new(notice)
                                                            .size(LabelSize::Small)
                                                            .color(Color::Warning),
                                                    )
                                                })
                                                .when_some(self.base_branch_error.clone(), |this, error| {
                                                    this.child(
                                                        Label::new(error)
                                                            .size(LabelSize::Small)
                                                            .color(Color::Error),
                                                    )
                                                })
                                                .child(
                                                    div()
                                                        .w_full()
                                                        .my_2()
                                                        .border_t_1()
                                                        .border_color(
                                                            cx.theme().colors().border_variant,
                                                        ),
                                                )
                                                .child(
                                                    h_flex()
                                                        .gap_1()
                                                        .items_center()
                                                        .child(
                                                            Disclosure::new(
                                                                "superzent-new-workspace-more",
                                                                self.show_more_options,
                                                            )
                                                            .on_click(cx.listener(
                                                                |this, _: &ClickEvent, _, cx| {
                                                                    this.show_more_options =
                                                                        !this.show_more_options;
                                                                    cx.notify();
                                                                },
                                                            )),
                                                        )
                                                        .child(
                                                            Label::new("More")
                                                                .size(LabelSize::Small)
                                                                .color(Color::Muted),
                                                        ),
                                                )
                                                .when(self.show_more_options, |this| {
                                                    this.child(
                                                        v_flex()
                                                            .gap_2()
                                                            .pt_2()
                                                            .child(
                                                                v_flex()
                                                                    .gap_1()
                                                                    .child(
                                                                        Label::new("Setup Script")
                                                                            .size(LabelSize::Small),
                                                                    )
                                                                    .child(
                                                                        Label::new(
                                                                            "Runs once after creation for this workspace. Changes here are never saved as repo defaults.",
                                                                        )
                                                                        .size(LabelSize::Small)
                                                                        .color(Color::Muted),
                                                                    )
                                                                    .child(self.setup_script_editor.clone()),
                                                            )
                                                            .child(
                                                                v_flex()
                                                                    .gap_1()
                                                                    .child(
                                                                        Label::new("Teardown Script")
                                                                            .size(LabelSize::Small),
                                                                    )
                                                                    .child(self.teardown_script_editor.clone()),
                                                            )
                                                            .child(
                                                                v_flex()
                                                                    .gap_1()
                                                                    .child(
                                                                        Label::new("Available Variables")
                                                                            .size(LabelSize::Small)
                                                                            .color(Color::Muted),
                                                                    )
                                                                    .child(
                                                                        h_flex()
                                                                            .gap_2()
                                                                            .flex_wrap()
                                                                            .child(self.render_variable_helper(
                                                                                "superzent-base-path",
                                                                                "$SUPERZENT_BASE_PATH",
                                                                                cx,
                                                                            ))
                                                                            .child(self.render_variable_helper(
                                                                                "superzent-worktree-path",
                                                                                "$SUPERZENT_WORKTREE_PATH",
                                                                                cx,
                                                                            ))
                                                                            .child(self.render_variable_helper(
                                                                                "superzent-root-path",
                                                                                "$SUPERZENT_ROOT_PATH",
                                                                                cx,
                                                                            )),
                                                                    ),
                                                            )
                                                            .child(
                                                                Checkbox::new(
                                                                    "superzent-new-workspace-save-lifecycle",
                                                                    self.save_teardown_script_as_repo_default.into(),
                                                                )
                                                                .label(
                                                                    "Save teardown script as the default for this repository",
                                                                )
                                                                .fill()
                                                                .elevation(ElevationIndex::Surface)
                                                                .label_size(LabelSize::Small)
                                                                .on_click(cx.listener(
                                                                    |this, selection, _, cx| {
                                                                        this.save_teardown_script_as_repo_default =
                                                                            *selection == ToggleState::Selected;
                                                                        cx.notify();
                                                                    },
                                                                )),
                                                            ),
                                                    )
                                                }),
                                        )
                                    },
                                )
                                .when_some(self.last_error.clone(), |this, error| {
                                    this.child(
                                        Label::new(error)
                                            .size(LabelSize::Small)
                                            .color(Color::Error),
                                    )
                                }),
                        ),
                    )
                    .footer(
                        ModalFooter::new().end_slot(
                            h_flex()
                                .gap_2()
                                .child(
                                    Button::new("superzent-new-workspace-cancel", "Cancel")
                                        .style(ButtonStyle::Subtle)
                                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                            this.cancel(&menu::Cancel, window, cx);
                                        })),
                                )
                                .child(
                                    Button::new("superzent-new-workspace-create", "Create")
                                        .style(ButtonStyle::Filled)
                                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                            this.confirm(&menu::Confirm, window, cx);
                                        })),
                                ),
                        ),
                    ),
            )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DeleteWorkspaceModalDecision {
    Cancel,
    Delete,
    DeleteAnyway,
}

struct DeleteWorkspaceModal {
    prompt_details: DeleteWorkspacePromptDetails,
    sender: SmolSender<DeleteWorkspaceModalDecision>,
    scroll_handle: ScrollHandle,
    focus_handle: FocusHandle,
}

impl EventEmitter<DismissEvent> for DeleteWorkspaceModal {}
impl ModalView for DeleteWorkspaceModal {}

impl Focusable for DeleteWorkspaceModal {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl DeleteWorkspaceModal {
    fn new(
        prompt_details: DeleteWorkspacePromptDetails,
        sender: SmolSender<DeleteWorkspaceModalDecision>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            prompt_details,
            sender,
            scroll_handle: ScrollHandle::new(),
            focus_handle: cx.focus_handle(),
        }
    }

    fn send_decision(&mut self, decision: DeleteWorkspaceModalDecision, cx: &mut Context<Self>) {
        let _ = self.sender.try_send(decision);
        cx.emit(DismissEvent);
    }

    fn cancel(&mut self, _: &menu::Cancel, _: &mut Window, cx: &mut Context<Self>) {
        self.send_decision(DeleteWorkspaceModalDecision::Cancel, cx);
    }

    fn confirm(&mut self, _: &menu::Confirm, _: &mut Window, cx: &mut Context<Self>) {
        self.send_decision(self.prompt_details.confirm_decision, cx);
    }

    fn render_code_block(
        &self,
        id: &'static str,
        label: &'static str,
        text: SharedString,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        v_flex()
            .gap_1()
            .child(Label::new(label).size(LabelSize::Small).color(Color::Muted))
            .child(
                div()
                    .id(id)
                    .max_h(px(180.))
                    .overflow_y_scroll()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().colors().border_variant)
                    .bg(cx.theme().colors().editor_background)
                    .p_2()
                    .child(
                        Label::new(text)
                            .size(LabelSize::Small)
                            .buffer_font(cx)
                            .color(Color::Default),
                    ),
            )
    }
}

impl Render for DeleteWorkspaceModal {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .key_context("SuperzentDeleteWorkspaceModal")
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::confirm))
            .elevation_3(cx)
            .w(px(560.))
            .max_h(px(576.))
            .overflow_hidden()
            .child(
                Modal::new(
                    "superzent-delete-workspace-modal",
                    Some(self.scroll_handle.clone()),
                )
                .header(ModalHeader::new().headline(self.prompt_details.title))
                .section(
                    Section::new().child(
                        v_flex()
                            .gap_3()
                            .child(
                                Label::new(self.prompt_details.prompt_message.clone())
                                    .size(LabelSize::Small)
                                    .color(Color::Muted),
                            )
                            .child(
                                Label::new(self.prompt_details.teardown_message.clone())
                                    .size(LabelSize::Small),
                            )
                            .when_some(
                                self.prompt_details.teardown_script.clone(),
                                |this, teardown_script| {
                                    this.child(self.render_code_block(
                                        "superzent-delete-workspace-teardown-preview",
                                        "Teardown Script",
                                        teardown_script.into(),
                                        cx,
                                    ))
                                },
                            )
                            .when_some(
                                self.prompt_details.failure_details.clone(),
                                |this, failure_details| {
                                    this.child(self.render_code_block(
                                        "superzent-delete-workspace-blocked-detail",
                                        "Config Read Failure",
                                        failure_details.into(),
                                        cx,
                                    ))
                                },
                            ),
                    ),
                )
                .footer(
                    ModalFooter::new().end_slot(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("superzent-delete-workspace-cancel", "Cancel")
                                    .style(ButtonStyle::Subtle)
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                        this.cancel(&menu::Cancel, window, cx);
                                    })),
                            )
                            .child(
                                Button::new(
                                    "superzent-delete-workspace-confirm",
                                    self.prompt_details.confirm_label,
                                )
                                .style(ButtonStyle::Filled)
                                .on_click(cx.listener(
                                    |this, _: &ClickEvent, window, cx| {
                                        this.confirm(&menu::Confirm, window, cx);
                                    },
                                )),
                            ),
                    ),
                ),
            )
    }
}

pub fn add_project_from_window(window: &mut gpui::Window, cx: &mut App) {
    if let Some(workspace_handle) = workspace_from_window(window, cx) {
        run_add_project_from_store(workspace_handle, window, cx);
    }
}

pub fn new_workspace_from_window(window: &mut gpui::Window, cx: &mut App) {
    if let Some(workspace_handle) = workspace_from_window(window, cx) {
        run_new_workspace_from_store(workspace_handle, window, cx);
    }
}

pub fn reveal_changes_from_window(window: &mut gpui::Window, cx: &mut App) {
    if let Some(workspace_handle) = workspace_from_window(window, cx) {
        run_reveal_changes_from_store(workspace_handle, window, cx);
    }
}

pub fn open_workspace_in_new_window_from_window(window: &mut gpui::Window, cx: &mut App) {
    if let Some(workspace_handle) = workspace_from_window(window, cx) {
        run_open_workspace_in_new_window_from_store(workspace_handle, window, cx);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WorkspaceSwitchDirection {
    Forward,
    Backward,
}

fn cycle_workspace_in_window_from_window<T: 'static>(
    direction: WorkspaceSwitchDirection,
    window: &mut Window,
    cx: &mut Context<T>,
) {
    window.defer(cx, move |window, cx| {
        let Some(multi_workspace) = window.root::<MultiWorkspace>().flatten() else {
            return;
        };

        multi_workspace.update(cx, |multi_workspace, cx| match direction {
            WorkspaceSwitchDirection::Forward => {
                multi_workspace.activate_next_recent_workspace(window, cx);
            }
            WorkspaceSwitchDirection::Backward => {
                multi_workspace.activate_previous_recent_workspace(window, cx);
            }
        });
    });
}

#[derive(Clone, Debug)]
struct DraggedWorkspaceRow {
    workspace_id: String,
    project_id: String,
    label: String,
}

#[derive(Clone, Debug)]
struct DraggedProjectRow {
    project_id: String,
    label: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum SidebarRenameTarget {
    Project(String),
    Workspace(String),
}

struct DraggedRowPreview {
    label: String,
}

impl Render for DraggedRowPreview {
    fn render(&mut self, _window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .px_2()
            .py_0p5()
            .gap_1()
            .items_center()
            .rounded_md()
            .bg(cx.theme().colors().elevated_surface_background)
            .border_1()
            .border_color(cx.theme().colors().border)
            .child(Icon::new(IconName::MenuAlt).color(Color::Muted))
            .child(Label::new(self.label.clone()).size(LabelSize::Small))
    }
}

pub struct SuperzentSidebar {
    store: Entity<SuperzentStore>,
    multi_workspace: WeakEntity<MultiWorkspace>,
    focus_handle: FocusHandle,
    width: Option<Pixels>,
    deleting_workspace_ids: BTreeSet<String>,
    context_menu: Option<(Entity<ContextMenu>, Point<Pixels>, Subscription)>,
    rename_target: Option<SidebarRenameTarget>,
    rename_editor: Option<Entity<Editor>>,
    rename_editor_subscription: Option<Subscription>,
    _git_store_subscriptions: Vec<Subscription>,
    _subscriptions: Vec<Subscription>,
}

impl SuperzentSidebar {
    pub fn new(
        multi_workspace: Entity<MultiWorkspace>,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let store = SuperzentStore::global(cx);
        let weak_multi_workspace = multi_workspace.downgrade();
        let mut subscriptions = vec![cx.observe(&store, |_, _, cx| cx.notify())];
        subscriptions.push(
            cx.subscribe_in(
                &multi_workspace,
                window,
                |this, _, event, _, cx| match event {
                    MultiWorkspaceEvent::ActiveWorkspaceChanged => {
                        this.sync_active_workspace(cx);
                        if let Some(current_workspace) = this.current_workspace_entity(cx) {
                            this.refresh_live_workspace_metadata(&current_workspace, false, cx);
                        }
                        cx.notify();
                    }
                    MultiWorkspaceEvent::WorkspaceAdded(_)
                    | MultiWorkspaceEvent::WorkspaceRemoved(_) => {
                        this.sync_active_workspace(cx);
                        this.sync_live_workspace_git_subscriptions(cx);
                        cx.notify();
                    }
                },
            ),
        );

        let mut this = Self {
            store,
            multi_workspace: weak_multi_workspace,
            focus_handle: cx.focus_handle(),
            width: None,
            deleting_workspace_ids: BTreeSet::new(),
            context_menu: None,
            rename_target: None,
            rename_editor: None,
            rename_editor_subscription: None,
            _git_store_subscriptions: Vec::new(),
            _subscriptions: subscriptions,
        };
        this.sync_active_workspace(cx);
        this.sync_live_workspace_git_subscriptions(cx);
        this
    }

    fn sync_active_workspace(&mut self, cx: &mut Context<Self>) {
        let Some(current_workspace) = self.current_workspace_entity(cx) else {
            return;
        };
        let (candidate_count, existing_workspace_id, inferred_project_id) = {
            let store = self.store.read(cx);
            let candidate_locations = workspace_location_candidates(&current_workspace, cx);
            let existing_workspace_id = matched_workspace_id_for_candidate_locations(
                &candidate_locations,
                store.workspaces(),
                store.active_workspace_id(),
            );
            let inferred_project_id =
                if existing_workspace_id.is_none() && candidate_locations.len() > 1 {
                    inferred_project_id_for_live_workspace(&current_workspace, &store, cx)
                } else {
                    None
                };
            (
                candidate_locations.len(),
                existing_workspace_id,
                inferred_project_id,
            )
        };
        self.store.update(cx, |store, cx| {
            if let Some(workspace_id) = existing_workspace_id.as_deref() {
                store.record_workspace_opened(workspace_id, cx);
                return;
            }

            if candidate_count == 1 {
                let workspace_bundle = build_local_workspace_bundle(&current_workspace, store, cx)
                    .or_else(|| build_remote_workspace_bundle(&current_workspace, store, cx));
                if let Some((project_entry, workspace_entry)) = workspace_bundle {
                    let workspace_id = workspace_entry.id.clone();
                    store.upsert_project_bundle(project_entry, workspace_entry, cx);
                    store.record_workspace_opened(&workspace_id, cx);
                    return;
                }
            }

            let workspace_id = inferred_project_id
                .as_deref()
                .and_then(|project_id| store.primary_workspace_for_project(project_id))
                .or_else(|| {
                    inferred_project_id.as_deref().and_then(|project_id| {
                        store.workspaces_for_project(project_id).into_iter().next()
                    })
                })
                .map(|workspace| workspace.id.clone());
            store.set_active_workspace(workspace_id, cx);
        });
    }

    fn sync_live_workspace_git_subscriptions(&mut self, cx: &mut Context<Self>) {
        self._git_store_subscriptions.clear();

        let Some(multi_workspace) = self.multi_workspace.upgrade() else {
            return;
        };

        let workspaces = multi_workspace.read(cx).workspaces().to_vec();
        for workspace in &workspaces {
            self.refresh_live_workspace_metadata(workspace, false, cx);
        }

        self._git_store_subscriptions = workspaces
            .into_iter()
            .map(|workspace| {
                let git_store = workspace.read(cx).project().read(cx).git_store().clone();

                cx.subscribe(&git_store, move |this, _, event, cx| {
                    let persist = match event {
                        GitStoreEvent::RepositoryUpdated(
                            _,
                            RepositoryEvent::StatusesChanged,
                            _,
                        ) => false,
                        GitStoreEvent::RepositoryUpdated(_, RepositoryEvent::BranchChanged, _) => {
                            true
                        }
                        GitStoreEvent::ActiveRepositoryChanged(_)
                        | GitStoreEvent::RepositoryAdded
                        | GitStoreEvent::RepositoryRemoved(_) => false,
                        GitStoreEvent::RepositoryUpdated(_, _, _)
                        | GitStoreEvent::IndexWriteError(_)
                        | GitStoreEvent::JobsUpdated
                        | GitStoreEvent::ConflictsUpdated => return,
                    };

                    this.refresh_live_workspace_metadata(&workspace, persist, cx);
                })
            })
            .collect();
    }

    fn refresh_live_workspace_metadata(
        &self,
        workspace: &Entity<Workspace>,
        persist: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace_id) = ({
            let store = self.store.read(cx);
            store_workspace_id_for_live_workspace(
                workspace,
                &store,
                store.active_workspace_id(),
                cx,
            )
        }) else {
            return;
        };
        let (branch, git_status, git_summary) =
            if let Some(repository) = active_repository_for_workspace(workspace, cx) {
                let repository = repository.read(cx);
                (
                    repository_branch_display_name(&repository),
                    WorkspaceGitStatus::Available,
                    Some(git_change_summary_from_repository(&repository)),
                )
            } else {
                (
                    superzent_git::NO_GIT_BRANCH_LABEL.to_string(),
                    WorkspaceGitStatus::Unavailable,
                    None,
                )
            };

        self.store.update(cx, |store, cx| {
            store.refresh_workspace_metadata(
                &workspace_id,
                Some(branch),
                git_status,
                git_summary,
                persist,
                cx,
            );
        });
    }

    fn is_workspace_open_in_current_window(
        &self,
        workspace_entry: &WorkspaceEntry,
        cx: &App,
    ) -> bool {
        let Some(multi_workspace) = self.multi_workspace.upgrade() else {
            return false;
        };

        multi_workspace
            .read(cx)
            .workspaces()
            .iter()
            .any(|workspace| workspace_matches_entry(workspace, workspace_entry, cx))
    }

    fn mark_workspace_deleting(
        &mut self,
        workspace_id: &str,
        is_deleting: bool,
        cx: &mut Context<Self>,
    ) {
        let changed = if is_deleting {
            self.deleting_workspace_ids.insert(workspace_id.to_string())
        } else {
            self.deleting_workspace_ids.remove(workspace_id)
        };

        if changed {
            cx.notify();
        }
    }

    fn workspace_is_deleting(&self, workspace_id: &str) -> bool {
        self.deleting_workspace_ids.contains(workspace_id)
    }

    fn current_workspace_entity(&self, cx: &App) -> Option<Entity<Workspace>> {
        self.multi_workspace
            .upgrade()
            .map(|multi_workspace| multi_workspace.read(cx).workspace().clone())
    }

    fn is_renaming_project(&self, project_id: &str) -> bool {
        self.rename_target.as_ref() == Some(&SidebarRenameTarget::Project(project_id.to_string()))
    }

    fn is_renaming_workspace(&self, workspace_id: &str) -> bool {
        self.rename_target.as_ref()
            == Some(&SidebarRenameTarget::Workspace(workspace_id.to_string()))
    }

    fn begin_project_rename(
        &mut self,
        project_id: &str,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_renaming_project(project_id) {
            return;
        }

        let Some(current_label) = self
            .store
            .read(cx)
            .project(project_id)
            .map(|project| project.name.clone())
        else {
            return;
        };

        self.begin_rename(
            SidebarRenameTarget::Project(project_id.to_string()),
            current_label,
            window,
            cx,
        );
    }

    fn begin_workspace_rename(
        &mut self,
        workspace_id: &str,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_renaming_workspace(workspace_id) {
            return;
        }

        let Some(current_label) = self
            .store
            .read(cx)
            .workspace(workspace_id)
            .map(workspace_sidebar_title)
        else {
            return;
        };

        self.begin_rename(
            SidebarRenameTarget::Workspace(workspace_id.to_string()),
            current_label,
            window,
            cx,
        );
    }

    fn begin_rename(
        &mut self,
        target: SidebarRenameTarget,
        current_label: String,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        if self.rename_editor.is_some() {
            self.finish_rename(true, window, cx);
        }

        let rename_editor = cx.new(|cx| Editor::single_line(window, cx));
        let rename_editor_subscription = cx.subscribe_in(&rename_editor, window, {
            let rename_editor = rename_editor.clone();
            move |_this, _, event, window, cx| {
                if let EditorEvent::Blurred = event {
                    let rename_editor = rename_editor.clone();
                    cx.defer_in(window, move |this, window, cx| {
                        let still_current = this
                            .rename_editor
                            .as_ref()
                            .is_some_and(|current| current == &rename_editor);
                        if still_current && !rename_editor.focus_handle(cx).is_focused(window) {
                            this.finish_rename(true, window, cx);
                        }
                    });
                }
            }
        });

        self.context_menu.take();
        self.rename_target = Some(target);
        self.rename_editor = Some(rename_editor.clone());
        self.rename_editor_subscription = Some(rename_editor_subscription);

        rename_editor.update(cx, |editor, cx| {
            editor.set_text(current_label, window, cx);
            editor.select_all(&SelectAll, window, cx);
            editor.focus_handle(cx).focus(window, cx);
        });
        cx.notify();
    }

    fn finish_rename(&mut self, save: bool, window: &mut gpui::Window, cx: &mut Context<Self>) {
        let rename_target = self.rename_target.take();
        let editor = self.rename_editor.take();
        self.rename_editor_subscription = None;

        if save
            && let (Some(rename_target), Some(editor)) = (rename_target.as_ref(), editor.as_ref())
        {
            let label = editor.read(cx).text(cx).trim().to_string();
            match rename_target {
                SidebarRenameTarget::Project(project_id) => {
                    if !label.is_empty() {
                        self.store.update(cx, |store, cx| {
                            store.set_project_name(project_id, label, cx);
                        });
                    }
                }
                SidebarRenameTarget::Workspace(workspace_id) => {
                    let unchanged_visible_label = self
                        .store
                        .read(cx)
                        .workspace(workspace_id)
                        .is_some_and(|workspace| {
                            workspace.display_name.is_none()
                                && label == workspace_sidebar_title(workspace)
                        });
                    if !unchanged_visible_label {
                        self.store.update(cx, |store, cx| {
                            store.set_workspace_display_name(workspace_id, Some(label.clone()), cx);
                        });
                    }
                }
            }
        }

        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    fn expand_workspace_section(
        &mut self,
        _: &ExpandWorkspaceSection,
        _: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let Some(project_id) = self.store.read(cx).active_project_id().map(str::to_owned) else {
            return;
        };
        self.store.update(cx, |store, cx| {
            store.set_project_collapsed(&project_id, false, cx);
        });
    }

    fn collapse_workspace_section(
        &mut self,
        _: &CollapseWorkspaceSection,
        _: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let Some(project_id) = self.store.read(cx).active_project_id().map(str::to_owned) else {
            return;
        };
        self.store.update(cx, |store, cx| {
            store.set_project_collapsed(&project_id, true, cx);
        });
    }

    fn open_next_workspace_switcher(
        &mut self,
        _: &NextWorkspaceInWindow,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        cycle_workspace_in_window_from_window(WorkspaceSwitchDirection::Forward, window, cx);
    }

    fn open_previous_workspace_switcher(
        &mut self,
        _: &PreviousWorkspaceInWindow,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        cycle_workspace_in_window_from_window(WorkspaceSwitchDirection::Backward, window, cx);
    }

    fn move_workspace(
        &mut self,
        dragged: &DraggedWorkspaceRow,
        target_workspace_id: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        if Some(dragged.workspace_id.as_str()) == target_workspace_id {
            return;
        }

        self.store.update(cx, |store, cx| {
            store.reorder_workspace(&dragged.workspace_id, target_workspace_id, cx);
        });
    }

    fn move_project(
        &mut self,
        dragged: &DraggedProjectRow,
        target_project_id: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        if Some(dragged.project_id.as_str()) == target_project_id {
            return;
        }

        self.store.update(cx, |store, cx| {
            store.reorder_project(&dragged.project_id, target_project_id, cx);
        });
    }

    fn deploy_project_context_menu(
        &mut self,
        position: Point<Pixels>,
        project: ProjectEntry,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let entity = cx.entity();
        let context_menu = ContextMenu::build(window, cx, move |menu, _, _| {
            let mut menu = menu;

            if matches!(&project.location, ProjectLocation::Local { .. }) {
                menu = menu.entry("Sync Worktrees", None, {
                    let entity = entity.clone();
                    let project = project.clone();
                    move |window, cx| {
                        entity.update(cx, |this, cx| {
                            this.sync_project_worktrees(project.clone(), window, cx);
                        });
                    }
                });
            }

            menu = menu.entry("Rename Project", None, {
                let entity = entity.clone();
                let project_id = project.id.clone();
                move |window, cx| {
                    entity.update(cx, |this, cx| {
                        this.begin_project_rename(&project_id, window, cx);
                    });
                }
            });

            menu.entry("Close Project", None, {
                let entity = entity.clone();
                let project_id = project.id;
                move |window, cx| {
                    entity.update(cx, |this, cx| {
                        this.close_project(&project_id, window, cx);
                    });
                }
            })
        });

        window.focus(&context_menu.focus_handle(cx), cx);
        let subscription = cx.subscribe_in(
            &context_menu,
            window,
            |this, _, _: &DismissEvent, window, cx| {
                if this.context_menu.as_ref().is_some_and(|context_menu| {
                    context_menu.0.focus_handle(cx).contains_focused(window, cx)
                }) {
                    cx.focus_self(window);
                }
                this.context_menu.take();
                cx.notify();
            },
        );

        self.context_menu = Some((context_menu, position, subscription));
        cx.notify();
    }

    fn deploy_workspace_context_menu(
        &mut self,
        position: Point<Pixels>,
        workspace: WorkspaceEntry,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let entity = cx.entity();
        let context_menu = ContextMenu::build(window, cx, move |menu, _window, _cx| {
            let mut menu = menu.entry("Rename Workspace", None, {
                let entity = entity.clone();
                let workspace_id = workspace.id.clone();
                move |window, cx| {
                    entity.update(cx, |this, cx| {
                        this.begin_workspace_rename(&workspace_id, window, cx);
                    });
                }
            });

            menu = menu.entry("Close Workspace", Some(Box::new(CloseWorkspace)), {
                let entity = entity.clone();
                move |window, cx| {
                    let workspace = workspace.clone();
                    entity.update(cx, |this, cx| {
                        this.close_workspace(workspace, window, cx);
                    });
                }
            });

            menu
        });

        window.focus(&context_menu.focus_handle(cx), cx);
        let subscription = cx.subscribe_in(
            &context_menu,
            window,
            |this, _, _: &DismissEvent, window, cx| {
                if this.context_menu.as_ref().is_some_and(|context_menu| {
                    context_menu.0.focus_handle(cx).contains_focused(window, cx)
                }) {
                    cx.focus_self(window);
                }
                this.context_menu.take();
                cx.notify();
            },
        );

        self.context_menu = Some((context_menu, position, subscription));
        cx.notify();
    }

    fn close_project(
        &mut self,
        project_id: &str,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let Some(current_workspace) = self.current_workspace_entity(cx) else {
            return;
        };
        run_close_project_from_store(current_workspace, project_id.to_string(), window, cx);
    }

    fn close_workspace(
        &mut self,
        workspace_entry: WorkspaceEntry,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let Some(multi_workspace) = self.multi_workspace.upgrade() else {
            return;
        };

        let Some(index) = multi_workspace
            .read(cx)
            .workspaces()
            .iter()
            .enumerate()
            .find_map(|(index, workspace)| {
                workspace_matches_entry(workspace, &workspace_entry, cx).then_some(index)
            })
            .or_else(|| {
                (self.store.read(cx).active_workspace_id() == Some(workspace_entry.id.as_str()))
                    .then(|| multi_workspace.read(cx).active_workspace_index())
            })
        else {
            if let Some(current_workspace) = self.current_workspace_entity(cx) {
                show_workspace_toast(
                    &current_workspace,
                    "Workspace is not open in this window.",
                    cx,
                );
            }
            return;
        };

        multi_workspace.update(cx, |multi_workspace, cx| {
            multi_workspace
                .close_workspace_at_index(index, window, cx)
                .detach_and_log_err(cx);
        });
    }

    fn sync_project_worktrees(
        &mut self,
        project: ProjectEntry,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let Some(current_workspace) = self.current_workspace_entity(cx) else {
            return;
        };
        run_sync_project_worktrees_from_store(current_workspace, project, window, cx);
    }

    fn render_project_drop_zone(
        &self,
        target_project_id: Option<&str>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(match target_project_id {
                Some(target_project_id) => format!("project-drop-zone-{target_project_id}"),
                None => "project-drop-zone-end".to_string(),
            })
            .mx_2()
            .my_0p5()
            .h(px(4.))
            .rounded_sm()
            .drag_over::<DraggedProjectRow>(|style, _, _, cx| {
                style.bg(cx.theme().colors().drop_target_background)
            })
            .on_drop(cx.listener({
                let target_project_id = target_project_id.map(str::to_owned);
                move |this, dragged: &DraggedProjectRow, _, cx| {
                    this.move_project(dragged, target_project_id.as_deref(), cx);
                }
            }))
    }

    fn render_workspace_drop_zone(
        &self,
        project_id: &str,
        target_workspace_id: Option<&str>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(match target_workspace_id {
                Some(target_workspace_id) => {
                    format!("workspace-drop-zone-{project_id}-{target_workspace_id}")
                }
                None => format!("workspace-drop-zone-{project_id}-end"),
            })
            .mx_3()
            .my_0p5()
            .h(px(4.))
            .rounded_sm()
            .drag_over::<DraggedWorkspaceRow>({
                let project_id = project_id.to_string();
                move |style, dragged, _, cx| {
                    if dragged.project_id == project_id {
                        style.bg(cx.theme().colors().drop_target_background)
                    } else {
                        style
                    }
                }
            })
            .on_drop(cx.listener({
                let project_id = project_id.to_string();
                let target_workspace_id = target_workspace_id.map(str::to_owned);
                move |this, dragged: &DraggedWorkspaceRow, _, cx| {
                    if dragged.project_id == project_id {
                        this.move_workspace(dragged, target_workspace_id.as_deref(), cx);
                    }
                }
            }))
    }

    fn render_project(
        &self,
        project: &ProjectEntry,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let is_collapsed = project.collapsed;
        let workspaces = self
            .store
            .read(cx)
            .workspaces_for_project(&project.id)
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        let dragged_project = DraggedProjectRow {
            project_id: project.id.clone(),
            label: project.name.clone(),
        };

        v_flex()
            .w_full()
            .child(
                div()
                    .id(format!("project-row-wrap-{}", project.id))
                    .w_full()
                    .on_drag(dragged_project, |dragged, _, _, cx| {
                        let label = dragged.label.clone();
                        cx.new(move |_| DraggedRowPreview { label })
                    })
                    .child(
                        ListItem::new(format!("project-{}", project.id))
                            .spacing(ui::ListItemSpacing::Dense)
                            .rounded()
                            .start_slot(h_flex().gap_1p5().items_center().child(Icon::new(
                                if is_collapsed {
                                    IconName::ChevronRight
                                } else {
                                    IconName::ChevronDown
                                },
                            )))
                            .end_slot(
                                h_flex()
                                    .gap_1()
                                    .items_center()
                                    .child(
                                        Chip::new(project_workspace_label(workspaces.len()))
                                            .label_color(Color::Muted),
                                    )
                                    .child(
                                        IconButton::new(
                                            format!("project-new-{}", project.id),
                                            IconName::Plus,
                                        )
                                        .shape(ui::IconButtonShape::Square)
                                        .icon_color(Color::Muted)
                                        .on_click(
                                            cx.listener({
                                                let project_id = project.id.clone();
                                                move |this, _: &ClickEvent, window, cx| {
                                                    this.store.update(cx, |store, cx| {
                                                        store.set_active_workspace(
                                                            store
                                                                .primary_workspace_for_project(
                                                                    &project_id,
                                                                )
                                                                .map(|workspace| {
                                                                    workspace.id.clone()
                                                                }),
                                                            cx,
                                                        );
                                                    });
                                                    if let Some(workspace) =
                                                        this.current_workspace_entity(cx)
                                                    {
                                                        run_new_workspace_from_store(
                                                            workspace, window, cx,
                                                        );
                                                    }
                                                }
                                            }),
                                        ),
                                    ),
                            )
                            .on_secondary_mouse_down(cx.listener({
                                let project = project.clone();
                                move |this, event: &MouseDownEvent, window, cx| {
                                    this.deploy_project_context_menu(
                                        event.position,
                                        project.clone(),
                                        window,
                                        cx,
                                    );
                                }
                            }))
                            .on_click(cx.listener({
                                let project_id = project.id.clone();
                                move |this, _: &ClickEvent, _, cx| {
                                    let collapsed = this
                                        .store
                                        .read(cx)
                                        .project(&project_id)
                                        .map(|project| !project.collapsed)
                                        .unwrap_or(false);
                                    this.store.update(cx, |store, cx| {
                                        store.set_project_collapsed(&project_id, collapsed, cx);
                                    });
                                }
                            }))
                            .child(
                                v_flex()
                                    .w_full()
                                    .h(px(48.))
                                    .justify_center()
                                    .gap_0p5()
                                    .min_w_0()
                                    .child(self.render_project_title(project, cx))
                                    .child(
                                        Label::new(project.display_root())
                                            .size(LabelSize::XSmall)
                                            .color(Color::Muted)
                                            .truncate(),
                                    ),
                            ),
                    ),
            )
            .when(!is_collapsed, |this| {
                this.children(
                    workspaces
                        .iter()
                        .map(|workspace| {
                            self.render_workspace_row(workspace, window, cx)
                                .into_any_element()
                        })
                        .collect::<Vec<_>>(),
                )
                .child(self.render_workspace_drop_zone(&project.id, None, cx))
            })
    }

    fn render_workspace_row(
        &self,
        workspace: &WorkspaceEntry,
        _window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = self.store.read(cx).active_workspace_id() == Some(workspace.id.as_str());
        let is_deleting = self.workspace_is_deleting(&workspace.id);
        let is_open_in_current_window = self.is_workspace_open_in_current_window(workspace, cx);
        let attention_status = workspace.attention_status.clone();
        let workspace_for_open = workspace.clone();
        let workspace_for_delete = workspace.clone();
        let workspace_for_context_menu = workspace.clone();
        let dragged_workspace = DraggedWorkspaceRow {
            workspace_id: workspace.id.clone(),
            project_id: workspace.project_id.clone(),
            label: workspace_sidebar_title(workspace),
        };
        let branch_subtitle = workspace_branch_subtitle(workspace);
        let has_branch_subtitle = branch_subtitle.is_some();
        let row_status_pill = match workspace_row_status_kind(workspace, is_open_in_current_window)
        {
            WorkspaceRowStatusKind::Hidden => None,
            WorkspaceRowStatusKind::SetupInProgress => Some(render_workspace_setup_status_pill(
                "Setting up…",
                Color::Warning,
                workspace
                    .last_attention_reason
                    .clone()
                    .unwrap_or_else(|| WORKSPACE_SETUP_IN_PROGRESS_REASON.to_string()),
                cx,
            )),
            WorkspaceRowStatusKind::Open => Some(render_workspace_open_pill(cx)),
            WorkspaceRowStatusKind::GitChanges => render_workspace_git_status_pill(workspace, cx),
        };

        v_flex()
            .w_full()
            .child(self.render_workspace_drop_zone(&workspace.project_id, Some(&workspace.id), cx))
            .child(
                div()
                    .id(format!("workspace-row-wrap-{}", workspace.id))
                    .w_full()
                    .on_drag(dragged_workspace, |dragged, _, _, cx| {
                        let label = dragged.label.clone();
                        cx.new(move |_| DraggedRowPreview { label })
                    })
                    .child(
                        ListItem::new(format!("workspace-{}", workspace.id))
                            .toggle_state(selected)
                            .disabled(is_deleting)
                            .indent_level(1)
                            .spacing(ui::ListItemSpacing::Dense)
                            .rounded()
                            .start_slot(
                                h_flex()
                                    .gap_1()
                                    .items_center()
                                    .child(render_workspace_attention_indicator(
                                        &workspace.id,
                                        &attention_status,
                                        cx,
                                    ))
                                    .child(Icon::new(match workspace.kind {
                                        WorkspaceKind::Primary => IconName::Folder,
                                        WorkspaceKind::Worktree => IconName::GitBranch,
                                    })),
                            )
                            .when(workspace.managed && is_deleting, |this| {
                                this.end_slot(
                                    div().h_full().items_center().justify_center().child(
                                        Icon::new(IconName::ArrowCircle)
                                            .size(IconSize::Small)
                                            .color(Color::Muted)
                                            .with_rotate_animation(2),
                                    ),
                                )
                            })
                            .when(workspace.managed && !is_deleting, |this| {
                                this.end_hover_slot(
                                    IconButton::new(
                                        format!("delete-{}", workspace.id),
                                        IconName::Trash,
                                    )
                                    .shape(ui::IconButtonShape::Square)
                                    .icon_color(Color::Muted)
                                    .tooltip(|window, cx| {
                                        ui::Tooltip::text("Delete workspace")(window, cx)
                                    })
                                    .on_click(cx.listener(
                                        move |this, _: &ClickEvent, window, cx| {
                                            this.mark_workspace_deleting(
                                                &workspace_for_delete.id,
                                                true,
                                                cx,
                                            );
                                            if let Some(current_workspace) =
                                                this.current_workspace_entity(cx)
                                            {
                                                run_delete_workspace_from_store(
                                                    current_workspace,
                                                    workspace_for_delete.clone(),
                                                    Some(cx.entity().downgrade()),
                                                    window,
                                                    cx,
                                                );
                                            } else {
                                                this.mark_workspace_deleting(
                                                    &workspace_for_delete.id,
                                                    false,
                                                    cx,
                                                );
                                            }
                                        },
                                    )),
                                )
                            })
                            .tooltip({
                                let path = workspace.display_path();
                                move |window, cx| ui::Tooltip::text(path.clone())(window, cx)
                            })
                            .on_secondary_mouse_down(cx.listener({
                                let workspace = workspace_for_context_menu;
                                move |this, event: &MouseDownEvent, window, cx| {
                                    this.deploy_workspace_context_menu(
                                        event.position,
                                        workspace.clone(),
                                        window,
                                        cx,
                                    );
                                }
                            }))
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.store.update(cx, |store, cx| {
                                    store.record_workspace_opened(&workspace_for_open.id, cx);
                                });
                                this.refresh_workspace_metadata(
                                    workspace_for_open.clone(),
                                    window,
                                    cx,
                                );
                                this.focus_or_open_workspace(
                                    workspace_for_open.clone(),
                                    window,
                                    cx,
                                );
                            }))
                            .child(
                                v_flex()
                                    .w_full()
                                    .min_w_0()
                                    .h(px(48.))
                                    .py_1()
                                    .justify_center()
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .gap_2()
                                            .items_center()
                                            .child(
                                                v_flex()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .when_some(branch_subtitle, |this, branch| {
                                                        this.gap_0p5()
                                                            .child(self.render_workspace_title(
                                                                workspace, cx,
                                                            ))
                                                            .child(
                                                                Label::new(branch)
                                                                    .size(LabelSize::XSmall)
                                                                    .color(Color::Muted)
                                                                    .truncate(),
                                                            )
                                                    })
                                                    .when(!has_branch_subtitle, |this| {
                                                        this.child(
                                                            self.render_workspace_title(
                                                                workspace, cx,
                                                            ),
                                                        )
                                                    }),
                                            )
                                            .when_some(row_status_pill, |this, row_status_pill| {
                                                this.child(row_status_pill)
                                            }),
                                    ),
                            ),
                    ),
            )
    }

    fn render_project_title(
        &self,
        project: &ProjectEntry,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        if self.is_renaming_project(&project.id)
            && let Some(editor) = self.rename_editor.clone()
        {
            return div()
                .flex_1()
                .min_w_0()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                .child(
                    div()
                        .w_full()
                        .child(editor)
                        .on_action(cx.listener(move |this, _: &menu::Confirm, window, cx| {
                            this.finish_rename(true, window, cx);
                        }))
                        .on_action(cx.listener(move |this, _: &menu::Cancel, window, cx| {
                            this.finish_rename(false, window, cx);
                        })),
                )
                .into_any_element();
        }

        Label::new(project.name.clone())
            .size(LabelSize::Small)
            .truncate()
            .into_any_element()
    }

    fn render_workspace_title(
        &self,
        workspace: &WorkspaceEntry,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        if self.is_renaming_workspace(&workspace.id)
            && let Some(editor) = self.rename_editor.clone()
        {
            return div()
                .flex_1()
                .min_w_0()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                .child(
                    div()
                        .w_full()
                        .child(editor)
                        .on_action(cx.listener(move |this, _: &menu::Confirm, window, cx| {
                            this.finish_rename(true, window, cx);
                        }))
                        .on_action(cx.listener(move |this, _: &menu::Cancel, window, cx| {
                            this.finish_rename(false, window, cx);
                        })),
                )
                .into_any_element();
        }

        match workspace.kind {
            WorkspaceKind::Primary => Label::new(workspace_display_name(workspace))
                .size(LabelSize::Small)
                .truncate()
                .into_any_element(),
            WorkspaceKind::Worktree => Label::new(workspace_sidebar_title(workspace))
                .size(LabelSize::Small)
                .truncate()
                .into_any_element(),
        }
    }

    fn refresh_workspace_metadata(
        &self,
        workspace: WorkspaceEntry,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(live_workspace) = workspace_for_entry_in_window(window, cx, &workspace) {
            self.refresh_live_workspace_metadata(&live_workspace, true, cx);
            return;
        }

        match &workspace.location {
            WorkspaceLocation::Local { worktree_path } => {
                let worktree_path = worktree_path.clone();
                let workspace_id = workspace.id.clone();
                let store = self.store.clone();
                cx.spawn_in(window, async move |_, cx| {
                    let refresh = cx
                        .background_spawn(async move {
                            superzent_git::refresh_workspace_path(&worktree_path)
                        })
                        .await;

                    if let Ok(refresh) = refresh {
                        store.update(cx, |store, cx| {
                            store.refresh_workspace_metadata(
                                &workspace_id,
                                Some(refresh.branch),
                                refresh.git_status,
                                refresh.git_summary,
                                true,
                                cx,
                            );
                        });
                    }

                    anyhow::Ok(())
                })
                .detach_and_log_err(cx);
            }
            WorkspaceLocation::Ssh { .. } => {
                let branch_and_summary =
                    workspace_for_entry_in_window(window, cx, &workspace).map(|live_workspace| {
                        if let Some(repository) =
                            active_repository_for_workspace(&live_workspace, cx)
                        {
                            let repository = repository.read(cx);
                            (
                                repository_branch_display_name(&repository),
                                WorkspaceGitStatus::Available,
                                Some(git_change_summary_from_repository(&repository)),
                            )
                        } else {
                            (
                                superzent_git::NO_GIT_BRANCH_LABEL.to_string(),
                                WorkspaceGitStatus::Unavailable,
                                None,
                            )
                        }
                    });

                if let Some((branch, git_status, git_summary)) = branch_and_summary {
                    self.store.update(cx, |store, cx| {
                        store.refresh_workspace_metadata(
                            &workspace.id,
                            Some(branch),
                            git_status,
                            git_summary,
                            true,
                            cx,
                        );
                    });
                }
            }
        }
    }

    fn focus_or_open_workspace(
        &self,
        workspace_entry: WorkspaceEntry,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let Some(multi_workspace) = self.multi_workspace.upgrade() else {
            return;
        };

        if let Some(index) = multi_workspace
            .read(cx)
            .workspaces()
            .iter()
            .enumerate()
            .find_map(|(index, workspace)| {
                workspace_matches_entry(workspace, &workspace_entry, cx).then_some(index)
            })
        {
            multi_workspace.update(cx, |multi_workspace, cx| {
                multi_workspace.activate_index(index, window, cx);
            });
            window.activate_window();
            return;
        }

        multi_workspace
            .update(cx, |multi_workspace, cx| {
                open_workspace_entry(
                    workspace_entry.clone(),
                    multi_workspace.workspace().read(cx).app_state().clone(),
                    window,
                    cx,
                )
            })
            .detach_and_log_err(cx);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EmptyPaneMode {
    Initial,
    Workspace,
}

struct SuperzentEmptyPaneView {
    pane: WeakEntity<Pane>,
    pane_id: EntityId,
    store: Entity<SuperzentStore>,
    _subscriptions: Vec<Subscription>,
}

impl SuperzentEmptyPaneView {
    fn new(pane: WeakEntity<Pane>, pane_id: EntityId, cx: &mut Context<Self>) -> Self {
        let store = SuperzentStore::global(cx);
        Self {
            pane_id,
            pane,
            store: store.clone(),
            _subscriptions: vec![cx.observe(&store, |_, _, cx| cx.notify())],
        }
    }

    fn mode(&self, cx: &App) -> EmptyPaneMode {
        let store = self.store.read(cx);
        if store.projects().is_empty() || store.workspaces().is_empty() {
            return EmptyPaneMode::Initial;
        }

        let has_worktrees = self.current_workspace_entity(cx).is_some_and(|ws| {
            ws.read(cx)
                .project()
                .read(cx)
                .visible_worktrees(cx)
                .next()
                .is_some()
        });

        if has_worktrees {
            EmptyPaneMode::Workspace
        } else {
            EmptyPaneMode::Initial
        }
    }

    fn focus_pane(&self, window: &mut gpui::Window, cx: &mut App) {
        if let Some(pane) = self.pane.upgrade() {
            let focus_handle = pane.read(cx).focus_handle(cx);
            window.focus(&focus_handle, cx);
        }
    }

    fn current_workspace_entity(&self, cx: &App) -> Option<Entity<Workspace>> {
        self.pane
            .upgrade()
            .and_then(|pane| pane.read(cx).workspace())
    }

    fn action_button(
        &self,
        id: &'static str,
        label: &'static str,
        icon: IconName,
        primary: bool,
        on_click: impl Fn(&Self, &mut gpui::Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        Button::new(format!("{id}-{}", self.pane_id), label)
            .full_width()
            .start_icon(Icon::new(icon).size(IconSize::Small))
            .label_size(LabelSize::Small)
            .style(if primary {
                ui::ButtonStyle::Filled
            } else {
                ui::ButtonStyle::Subtle
            })
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                on_click(this, window, cx);
            }))
            .into_any_element()
    }
}

impl Render for SuperzentEmptyPaneView {
    fn render(&mut self, _window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mode = self.mode(cx);
        let (title, subtitle) = match mode {
            EmptyPaneMode::Initial => ("No projects yet", "Add a project folder to get started."),
            EmptyPaneMode::Workspace => ("This pane is empty", "Open something in this pane."),
        };

        let buttons = match mode {
            EmptyPaneMode::Initial => vec![
                self.action_button(
                    "superzent-empty-add-project",
                    "Add Project",
                    IconName::OpenFolder,
                    true,
                    |this, window, cx| {
                        if let Some(current_workspace) = this.current_workspace_entity(cx) {
                            run_add_project_from_store(current_workspace, window, cx);
                        }
                    },
                    cx,
                ),
                self.action_button(
                    "superzent-empty-open-recent",
                    "Open Recent",
                    IconName::HistoryRerun,
                    false,
                    |_this, window, cx| {
                        window.dispatch_action(Box::new(OpenRecent::default()), cx);
                    },
                    cx,
                ),
            ],
            EmptyPaneMode::Workspace => vec![
                self.action_button(
                    "superzent-empty-new-terminal",
                    "New Terminal",
                    IconName::Terminal,
                    true,
                    |this, window, cx| {
                        this.focus_pane(window, cx);
                        window
                            .dispatch_action(Box::new(workspace::NewCenterTerminal::default()), cx);
                    },
                    cx,
                ),
                self.action_button(
                    "superzent-empty-reveal-changes",
                    "Reveal Changes",
                    IconName::GitBranchAlt,
                    false,
                    |this, window, cx| {
                        this.focus_pane(window, cx);
                        reveal_changes_from_window(window, cx);
                    },
                    cx,
                ),
                self.action_button(
                    "superzent-empty-search-files",
                    "Search Files",
                    IconName::MagnifyingGlass,
                    false,
                    |this, window, cx| {
                        this.focus_pane(window, cx);
                        window
                            .dispatch_action(Box::new(workspace::ToggleFileFinder::default()), cx);
                    },
                    cx,
                ),
            ],
        };

        v_flex()
            .size_full()
            .justify_center()
            .items_center()
            .px_8()
            .py_8()
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(360.))
                    .gap_4()
                    .items_center()
                    .child(
                        v_flex()
                            .items_center()
                            .gap_1()
                            .child(Label::new(title).size(LabelSize::Large))
                            .child(
                                Label::new(subtitle)
                                    .size(LabelSize::Small)
                                    .color(Color::Muted),
                            ),
                    )
                    .child(v_flex().w_full().gap_2().children(buttons)),
            )
    }
}

impl EventEmitter<SidebarEvent> for SuperzentSidebar {}

impl Focusable for SuperzentSidebar {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SuperzentSidebar {
    fn render(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        let projects = self.store.read(cx).projects().to_vec();
        let project_content = if projects.is_empty() {
            vec![
                v_flex()
                    .gap_1()
                    .py_4()
                    .child(Label::new("No repositories yet"))
                    .child(
                        Label::new("Add a local folder or connect to a remote SSH project.")
                            .size(LabelSize::XSmall)
                            .color(Color::Muted),
                    )
                    .into_any_element(),
            ]
        } else {
            let mut content = Vec::with_capacity(projects.len() * 2 + 1);
            for project in &projects {
                content.push(
                    self.render_project_drop_zone(Some(&project.id), cx)
                        .into_any_element(),
                );
                content.push(self.render_project(project, window, cx).into_any_element());
            }
            content.push(self.render_project_drop_zone(None, cx).into_any_element());
            content
        };

        v_flex()
            .id("workspace-sidebar")
            .key_context("WorkspaceSidebar")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::expand_workspace_section))
            .on_action(cx.listener(Self::collapse_workspace_section))
            .on_action(cx.listener(Self::open_next_workspace_switcher))
            .on_action(cx.listener(Self::open_previous_workspace_switcher))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .size_full()
            .bg(cx.theme().colors().panel_background)
            .border_r_1()
            .border_color(cx.theme().colors().border)
            .child(
                v_flex()
                    .border_t_1()
                    .border_color(cx.theme().colors().border)
                    .h_full()
                    .child(
                        h_flex()
                            .border_b_1()
                            .border_color(cx.theme().colors().border)
                            .px_2()
                            .h(Tab::container_height(cx))
                            .gap_1()
                            .items_center()
                            .child(Icon::new(IconName::FileTree).color(Color::Muted))
                            .child(
                                Label::new("Workspaces")
                                    .size(LabelSize::Small)
                                    .color(Color::Default),
                            )
                            .child(div().flex_1()),
                    )
                    .child(
                        v_flex()
                            .id("workspace-list")
                            .flex_1()
                            .overflow_y_scroll()
                            .px_2()
                            .pb_1()
                            .children(project_content),
                    )
                    .child(
                        v_flex()
                            .border_t_1()
                            .border_color(cx.theme().colors().border)
                            .px_2()
                            .py_2()
                            .child(
                                Button::new("superzent-sidebar-add-project", "Add Project")
                                    .full_width()
                                    .style(ui::ButtonStyle::Subtle)
                                    .start_icon(
                                        Icon::new(IconName::FolderOpen).size(IconSize::Small),
                                    )
                                    .label_size(LabelSize::Small)
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                        if let Some(current_workspace) =
                                            this.current_workspace_entity(cx)
                                        {
                                            run_add_project_from_store(
                                                current_workspace,
                                                window,
                                                cx,
                                            );
                                        }
                                    })),
                            ),
                    ),
            )
            .children(self.context_menu.as_ref().map(|(menu, position, _)| {
                deferred(
                    anchored()
                        .position(*position)
                        .anchor(gpui::Corner::TopLeft)
                        .child(menu.clone()),
                )
                .with_priority(1)
            }))
    }
}

impl WorkspaceSidebar for SuperzentSidebar {
    fn width(&self, _: &App) -> Pixels {
        self.width.unwrap_or_else(|| px(300.))
    }

    fn set_width(&mut self, width: Option<Pixels>, cx: &mut Context<Self>) {
        self.width = width;
        cx.notify();
    }

    fn has_notifications(&self, cx: &App) -> bool {
        self.store
            .read(cx)
            .workspaces()
            .iter()
            .any(|workspace| workspace.attention_status != WorkspaceAttentionStatus::Idle)
    }

    fn toggle_recent_projects_popover(&self, _window: &mut Window, _cx: &mut App) {}

    fn is_recent_projects_popover_deployed(&self) -> bool {
        false
    }
}

pub struct SuperzentRightSidebar {
    right_dock: Entity<workspace::dock::Dock>,
    project_panel: Entity<ProjectPanel>,
    git_panel: Entity<GitPanel>,
    focus_handle: FocusHandle,
    width: Option<Pixels>,
    _active: bool,
    tab: RightSidebarTab,
    external_panel_tabs: Vec<EntityId>,
    _subscriptions: Vec<Subscription>,
}

impl SuperzentRightSidebar {
    pub fn load(
        workspace: WeakEntity<Workspace>,
        project_panel: Entity<ProjectPanel>,
        git_panel: Entity<GitPanel>,
        mut cx: AsyncWindowContext,
    ) -> Result<Entity<Self>> {
        let workspace_weak = workspace.clone();
        workspace.update_in(&mut cx, |workspace, window, cx| {
            let right_dock = workspace.right_dock().clone();
            cx.new(|cx| {
                Self::new(
                    workspace_weak.clone(),
                    right_dock.clone(),
                    project_panel,
                    git_panel,
                    window,
                    cx,
                )
            })
        })
    }

    fn new(
        workspace: WeakEntity<Workspace>,
        right_dock: Entity<workspace::dock::Dock>,
        project_panel: Entity<ProjectPanel>,
        git_panel: Entity<GitPanel>,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let store = SuperzentStore::global(cx);
        let mut subscriptions = vec![cx.observe(&store, |_, _, cx| cx.notify())];
        let workspace_for_restore = workspace;
        subscriptions.push(cx.observe_in(&right_dock, window, {
            move |this, dock, window, cx| {
                this.restore_active_sidebar(workspace_for_restore.clone(), dock, window, cx);
            }
        }));

        Self {
            right_dock,
            project_panel,
            git_panel,
            focus_handle: cx.focus_handle(),
            width: None,
            _active: false,
            tab: RightSidebarTab::Changes,
            external_panel_tabs: Vec::new(),
            _subscriptions: subscriptions,
        }
    }

    fn set_active_tab(&mut self, tab: RightSidebarTab, cx: &mut Context<Self>) {
        self.tab = tab;
        cx.notify();
    }

    fn is_tab_active(&self, tab: RightSidebarTab) -> bool {
        self.tab == tab
    }

    pub fn debug_active_tab(&self, cx: &App) -> String {
        match self.tab {
            RightSidebarTab::Changes => "changes".to_string(),
            RightSidebarTab::Files => "files".to_string(),
            RightSidebarTab::Panel(panel_id) => self
                .right_dock
                .read(cx)
                .panel_for_id(panel_id)
                .map(|panel| panel.persistent_name().to_string())
                .unwrap_or_default(),
        }
    }

    fn visible_external_tabs(
        &self,
        window: &Window,
        cx: &App,
    ) -> Vec<(EntityId, SharedString, Option<IconName>)> {
        let dock = self.right_dock.read(cx);
        self.external_panel_tabs
            .iter()
            .filter_map(|panel_id| {
                dock.panel_for_id(*panel_id).map(|panel| {
                    let label = panel
                        .icon_label(window, cx)
                        .unwrap_or_else(|| panel.persistent_name().to_string())
                        .into();
                    (*panel_id, label, panel.icon(window, cx))
                })
            })
            .collect()
    }

    fn sync_active_tab(&mut self, active_panel_exists: bool, cx: &mut Context<Self>) {
        if matches!(self.tab, RightSidebarTab::Panel(_)) && !active_panel_exists {
            self.tab = RightSidebarTab::Changes;
            cx.notify();
        }
    }

    fn sync_external_tabs(&mut self, cx: &mut Context<Self>) {
        let valid_panel_ids = self
            .right_dock
            .read(cx)
            .panels()
            .into_iter()
            .filter(|panel| {
                !matches!(
                    panel.panel_key(),
                    key if key == Self::panel_key()
                        || key == ProjectPanel::panel_key()
                        || key == GitPanel::panel_key()
                )
            })
            .map(|panel| panel.panel_id())
            .collect::<Vec<_>>();

        let previous_len = self.external_panel_tabs.len();
        self.external_panel_tabs
            .retain(|panel_id| valid_panel_ids.contains(panel_id));
        if previous_len != self.external_panel_tabs.len() {
            cx.notify();
        }
    }

    fn ensure_external_tab(&mut self, panel_id: EntityId, cx: &mut Context<Self>) {
        if !self.external_panel_tabs.contains(&panel_id) {
            self.external_panel_tabs.push(panel_id);
            cx.notify();
        }
    }

    fn dismiss_external_tab(&mut self, panel_id: EntityId, cx: &mut Context<Self>) {
        let previous_len = self.external_panel_tabs.len();
        self.external_panel_tabs.retain(|id| *id != panel_id);
        if self.tab == RightSidebarTab::Panel(panel_id) {
            self.tab = RightSidebarTab::Changes;
        }
        if previous_len != self.external_panel_tabs.len() {
            cx.notify();
        }
    }

    fn restore_active_sidebar(
        &mut self,
        workspace: WeakEntity<Workspace>,
        dock: Entity<workspace::dock::Dock>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let is_open = dock.read(cx).is_open();
        if !is_open {
            return;
        }

        let active_tab_exists = match self.tab {
            RightSidebarTab::Panel(panel_id) => dock.read(cx).panel_for_id(panel_id).is_some(),
            RightSidebarTab::Changes | RightSidebarTab::Files => true,
        };
        self.sync_external_tabs(cx);
        self.sync_active_tab(active_tab_exists, cx);

        let active_panel = {
            let dock = dock.read(cx);
            dock.active_panel().cloned()
        };
        let Some(active_panel) = active_panel else {
            return;
        };
        if active_panel.panel_key() == Self::panel_key() {
            return;
        }

        let tab = if active_panel.panel_key() == ProjectPanel::panel_key() {
            Some(RightSidebarTab::Files)
        } else if active_panel.panel_key() == GitPanel::panel_key() {
            Some(RightSidebarTab::Changes)
        } else {
            self.ensure_external_tab(active_panel.panel_id(), cx);
            Some(RightSidebarTab::Panel(active_panel.panel_id()))
        };

        window.defer(cx, move |window, cx| {
            let _ = workspace.update(cx, |workspace, cx| {
                show_superzent_right_sidebar(workspace, tab, false, window, cx);
            });
        });
    }

    fn render_tab_button(
        &self,
        id: impl Into<gpui::ElementId>,
        label: SharedString,
        icon: Option<IconName>,
        tab: RightSidebarTab,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let active = self.tab == tab;
        let compact = self.width.unwrap_or_else(|| px(320.)) < px(250.);
        let tooltip_label = label.clone();

        if compact && let Some(icon) = icon {
            return IconButton::new(id, icon)
                .shape(ui::IconButtonShape::Square)
                .style(ui::ButtonStyle::Subtle)
                .toggle_state(active)
                .selected_style(ui::ButtonStyle::Filled)
                .tooltip(move |window, cx| ui::Tooltip::text(tooltip_label.clone())(window, cx))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.set_active_tab(tab, cx);
                }))
                .into_any_element();
        }

        Button::new(id, label)
            .when_some(icon, |button, icon| {
                button.start_icon(Icon::new(icon).size(IconSize::Small))
            })
            .label_size(LabelSize::Small)
            .style(ui::ButtonStyle::Subtle)
            .toggle_state(active)
            .selected_style(ui::ButtonStyle::Filled)
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.set_active_tab(tab, cx);
            }))
            .into_any_element()
    }

    fn render_external_tab(
        &self,
        panel_id: EntityId,
        label: SharedString,
        icon: Option<IconName>,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let compact = self.width.unwrap_or_else(|| px(320.)) < px(250.);

        h_flex()
            .gap_0p5()
            .items_center()
            .child(self.render_tab_button(
                ("superzent-right-tab-panel", panel_id.as_u64()),
                label.clone(),
                icon,
                RightSidebarTab::Panel(panel_id),
                cx,
            ))
            .child(
                IconButton::new(
                    ("superzent-right-tab-panel-close", panel_id.as_u64()),
                    IconName::Close,
                )
                .shape(ui::IconButtonShape::Square)
                .style(ui::ButtonStyle::Subtle)
                .size(if compact {
                    ui::ButtonSize::Compact
                } else {
                    ui::ButtonSize::Default
                })
                .tooltip(move |window, cx| {
                    ui::Tooltip::text(format!("Dismiss {label}"))(window, cx)
                })
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.dismiss_external_tab(panel_id, cx);
                })),
            )
            .into_any_element()
    }
}

impl EventEmitter<PanelEvent> for SuperzentRightSidebar {}

impl Focusable for SuperzentRightSidebar {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SuperzentRightSidebar {
    fn render(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        let external_tabs = self.visible_external_tabs(window, cx);
        let content = match self.tab {
            RightSidebarTab::Changes => self.git_panel.clone().into_any_element(),
            RightSidebarTab::Files => self.project_panel.clone().into_any_element(),
            RightSidebarTab::Panel(panel_id) => self
                .right_dock
                .read(cx)
                .panel_for_id(panel_id)
                .map(|panel| panel.to_any().into_any_element())
                .unwrap_or_else(|| self.git_panel.clone().into_any_element()),
        };

        v_flex()
            .size_full()
            .child(
                v_flex()
                    .border_b_1()
                    .border_color(cx.theme().colors().border)
                    .bg(cx.theme().colors().panel_background)
                    .child(
                        h_flex()
                            .h(px(31.))
                            .px_2()
                            .gap_1()
                            .items_center()
                            .child(self.render_tab_button(
                                "superzent-right-tab-changes",
                                "Changes".into(),
                                Some(IconName::GitBranchAlt),
                                RightSidebarTab::Changes,
                                cx,
                            ))
                            .child(self.render_tab_button(
                                "superzent-right-tab-files",
                                "Files".into(),
                                Some(IconName::FileTree),
                                RightSidebarTab::Files,
                                cx,
                            ))
                            .children(external_tabs.into_iter().map(|(panel_id, label, icon)| {
                                self.render_external_tab(panel_id, label, icon, cx)
                            }))
                            .child(div().flex_1()),
                    ),
            )
            .child(div().flex_1().overflow_hidden().child(content))
    }
}

impl Panel for SuperzentRightSidebar {
    fn persistent_name() -> &'static str {
        "Superzent Right Sidebar"
    }

    fn panel_key() -> &'static str {
        "SuperzentRightSidebar"
    }

    fn position(&self, _: &gpui::Window, _: &App) -> DockPosition {
        DockPosition::Right
    }

    fn position_is_valid(&self, position: DockPosition) -> bool {
        position == DockPosition::Right
    }

    fn set_position(&mut self, _: DockPosition, _: &mut gpui::Window, _: &mut Context<Self>) {}

    fn size(&self, _: &gpui::Window, _: &App) -> Pixels {
        self.width.unwrap_or_else(|| px(320.))
    }

    fn set_size(&mut self, size: Option<Pixels>, _: &mut gpui::Window, cx: &mut Context<Self>) {
        self.width = size;
        cx.notify();
    }

    fn icon(&self, _: &gpui::Window, _: &App) -> Option<IconName> {
        Some(IconName::SplitAlt)
    }

    fn icon_tooltip(&self, _: &gpui::Window, _: &App) -> Option<&'static str> {
        Some("Details Sidebar")
    }

    fn toggle_action(&self) -> Box<dyn gpui::Action> {
        Box::new(workspace::ToggleRightDock)
    }

    fn starts_open(&self, _: &gpui::Window, _: &App) -> bool {
        true
    }

    fn set_active(&mut self, active: bool, _: &mut gpui::Window, cx: &mut Context<Self>) {
        self._active = active;
        cx.notify();
    }

    fn activation_priority(&self) -> u32 {
        10
    }
}

fn run_add_local_project(
    _workspace: &mut Workspace,
    window: &mut gpui::Window,
    cx: &mut Context<Workspace>,
) {
    let store = SuperzentStore::global(cx);
    let workspace_handle = cx.entity();
    let prompt = cx.prompt_for_paths(PathPromptOptions {
        files: false,
        directories: true,
        multiple: false,
        prompt: Some("Add Project".into()),
    });
    let default_preset_id = store.read(cx).default_preset().id.clone();

    cx.spawn_in(window, async move |_, cx| {
        let Ok(result) = prompt.await else {
            return anyhow::Ok(());
        };
        let paths = match result {
            Ok(Some(paths)) => paths,
            Ok(None) => return anyhow::Ok(()),
            Err(error) => {
                workspace_handle
                    .update_in(cx, |workspace, _, cx| {
                        workspace.show_toast(
                            Toast::new(
                                NotificationId::unique::<SuperzentSidebar>(),
                                format!("Failed to open picker: {error}"),
                            ),
                            cx,
                        );
                    })
                    .ok();
                return anyhow::Ok(());
            }
        };
        let Some(path) = paths.into_iter().next() else {
            return anyhow::Ok(());
        };

        let registration = cx
            .background_spawn(
                async move { superzent_git::register_project(&path, &default_preset_id) },
            )
            .await;

        workspace_handle
            .update_in(cx, |workspace, window, cx| match registration {
                Ok(registration) => {
                    let existing_primary = store
                        .read(cx)
                        .project_for_location(&registration.project.location)
                        .and_then(|project| {
                            store
                                .read(cx)
                                .primary_workspace_for_project(&project.id)
                                .cloned()
                        });

                    let primary_workspace = if let Some(existing) = existing_primary {
                        existing
                    } else {
                        store.update(cx, |store, cx| {
                            store.upsert_project_bundle(
                                registration.project.clone(),
                                registration.primary_workspace.clone(),
                                cx,
                            );
                        });
                        registration.primary_workspace
                    };

                    store.update(cx, |store, cx| {
                        store.record_workspace_opened(&primary_workspace.id, cx);
                    });
                    workspace.show_toast(
                        Toast::new(
                            NotificationId::unique::<SuperzentSidebar>(),
                            if primary_workspace.has_git() {
                                format!("Added {}", primary_workspace.name)
                            } else {
                                format!(
                                    "Added {} without Git. Initialize Git to enable managed workspaces.",
                                    primary_workspace.name
                                )
                            },
                        ),
                        cx,
                    );
                    open_workspace_entry(
                        primary_workspace,
                        workspace.app_state().clone(),
                        window,
                        cx,
                    )
                    .detach_and_log_err(cx);
                }
                Err(error) => workspace.show_toast(
                    Toast::new(
                        NotificationId::unique::<SuperzentSidebar>(),
                        format!("Failed to add project: {error}"),
                    ),
                    cx,
                ),
            })
            .ok();

        anyhow::Ok(())
    })
    .detach_and_log_err(cx);
}

fn run_add_remote_project(
    _workspace: &mut Workspace,
    window: &mut gpui::Window,
    cx: &mut Context<Workspace>,
) {
    window.dispatch_action(
        OpenRemote {
            from_existing_connection: false,
            create_new_window: false,
        }
        .boxed_clone(),
        cx,
    );
}

fn run_add_project(
    workspace: &mut Workspace,
    window: &mut gpui::Window,
    cx: &mut Context<Workspace>,
) {
    let workspace_handle = cx.entity().downgrade();
    workspace.toggle_modal(window, cx, move |_window, cx| {
        AddProjectChooserModal::new(workspace_handle, cx)
    });
}

fn run_new_workspace(
    workspace: &mut Workspace,
    window: &mut gpui::Window,
    cx: &mut Context<Workspace>,
) {
    let store = SuperzentStore::global(cx);
    let Some(project) = store
        .read(cx)
        .active_project()
        .cloned()
        .or_else(|| store.read(cx).projects().first().cloned())
    else {
        workspace.show_toast(
            Toast::new(
                NotificationId::unique::<SuperzentSidebar>(),
                "Add a project before creating a workspace.",
            ),
            cx,
        );
        return;
    };

    let primary_workspace = store
        .read(cx)
        .primary_workspace_for_project(&project.id)
        .cloned();

    if let Some(primary_workspace) = primary_workspace
        && !primary_workspace.has_git()
        && let Some(project_root) = primary_workspace.local_worktree_path().map(PathBuf::from)
    {
        let project_name = project.name.clone();
        let workspace_id = primary_workspace.id;
        let prompt = window.prompt(
            PromptLevel::Info,
            "Initialize Git?",
            Some(
                "This project is not a Git repository yet. Initialize Git to enable managed workspaces.",
            ),
            &["Initialize Git", "Cancel"],
            cx,
        );
        let workspace_handle = cx.entity().downgrade();

        cx.spawn_in(window, async move |_, cx| {
            if prompt.await != Ok(0) {
                return anyhow::Ok(());
            }

            let refresh = cx
                .background_spawn(
                    async move { superzent_git::initialize_git_repository(&project_root) },
                )
                .await;

            if let Some(workspace_handle) = workspace_handle.upgrade() {
                workspace_handle
                    .update_in(cx, |workspace, window, cx| match refresh {
                        Ok(refresh) => {
                            store.update(cx, |store, cx| {
                                store.refresh_workspace_metadata(
                                    &workspace_id,
                                    Some(refresh.branch),
                                    refresh.git_status,
                                    refresh.git_summary,
                                    true,
                                    cx,
                                );
                            });
                            workspace.show_toast(
                                Toast::new(
                                    NotificationId::unique::<SuperzentSidebar>(),
                                    format!("Initialized Git for {project_name}."),
                                ),
                                cx,
                            );
                            open_new_workspace_modal(workspace, project.clone(), window, cx);
                        }
                        Err(error) => {
                            workspace.show_toast(
                                Toast::new(
                                    NotificationId::unique::<SuperzentSidebar>(),
                                    format!("Failed to initialize Git: {error}"),
                                ),
                                cx,
                            );
                        }
                    })
                    .ok();
            }

            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
        return;
    }

    open_new_workspace_modal(workspace, project, window, cx);
}

fn open_new_workspace_modal(
    workspace: &mut Workspace,
    project: ProjectEntry,
    window: &mut gpui::Window,
    cx: &mut Context<Workspace>,
) {
    let workspace_handle = cx.entity().downgrade();
    let initial_base_workspace_path = SuperzentStore::global(cx)
        .read(cx)
        .active_workspace()
        .filter(|workspace_entry| workspace_entry.project_id == project.id)
        .and_then(|workspace_entry| workspace_entry.local_worktree_path())
        .map(Path::to_path_buf);
    let bootstrap =
        build_new_workspace_modal_bootstrap(&project, initial_base_workspace_path.as_deref());
    workspace.toggle_modal(window, cx, move |window, cx| {
        NewWorkspaceModal::new(workspace_handle.clone(), project, bootstrap, window, cx)
    });
}

fn run_new_workspace_from_store(
    workspace_handle: Entity<Workspace>,
    window: &mut gpui::Window,
    cx: &mut App,
) {
    workspace_handle.update(cx, |workspace, cx| {
        run_new_workspace(workspace, window, cx);
    });
}

fn run_reveal_changes(
    workspace: &mut Workspace,
    window: &mut gpui::Window,
    cx: &mut Context<Workspace>,
) {
    let workspace_handle = cx.entity();
    let store = SuperzentStore::global(cx);
    let Some(workspace_entry) = store
        .read(cx)
        .active_workspace()
        .cloned()
        .or_else(|| store.read(cx).workspaces().first().cloned())
    else {
        workspace.show_toast(
            Toast::new(
                NotificationId::unique::<SuperzentSidebar>(),
                "Select a workspace first.",
            ),
            cx,
        );
        return;
    };
    let switch_task =
        open_workspace_entry(workspace_entry, workspace.app_state().clone(), window, cx);
    let maybe_multi_workspace = window.window_handle().downcast::<MultiWorkspace>();

    cx.spawn_in(window, async move |_, cx| {
        if let Err(error) = switch_task.await {
            workspace_handle
                .update_in(cx, |workspace, _, cx| {
                    workspace.show_toast(
                        Toast::new(
                            NotificationId::unique::<SuperzentSidebar>(),
                            format!("Failed to open workspace: {error}"),
                        ),
                        cx,
                    );
                })
                .ok();
            return anyhow::Ok(());
        }

        let active_workspace = if let Some(multi_workspace) = maybe_multi_workspace {
            multi_workspace.update(cx, |multi_workspace, _, _| {
                multi_workspace.workspace().clone()
            })?
        } else {
            workspace_handle.clone()
        };

        active_workspace
            .update_in(cx, |workspace, window, cx| {
                show_superzent_right_sidebar(
                    workspace,
                    Some(RightSidebarTab::Changes),
                    true,
                    window,
                    cx,
                );
            })
            .ok();

        anyhow::Ok(())
    })
    .detach_and_log_err(cx);
}

fn run_reveal_changes_from_store(
    workspace_handle: Entity<Workspace>,
    window: &mut gpui::Window,
    cx: &mut App,
) {
    workspace_handle.update(cx, |workspace, cx| {
        run_reveal_changes(workspace, window, cx);
    });
}

fn run_open_workspace_in_new_window(
    workspace: &mut Workspace,
    window: &mut gpui::Window,
    cx: &mut Context<Workspace>,
) {
    let store = SuperzentStore::global(cx);
    let Some(workspace_entry) = store
        .read(cx)
        .active_workspace()
        .cloned()
        .or_else(|| store.read(cx).workspaces().first().cloned())
    else {
        workspace.show_toast(
            Toast::new(
                NotificationId::unique::<SuperzentSidebar>(),
                "Select a workspace first.",
            ),
            cx,
        );
        return;
    };

    open_workspace_entry(workspace_entry, workspace.app_state().clone(), window, cx)
        .detach_and_log_err(cx);
}

fn run_delete_workspace(
    workspace: &mut Workspace,
    window: &mut gpui::Window,
    cx: &mut Context<Workspace>,
) {
    let store = SuperzentStore::global(cx);
    let Some(workspace_entry) = store
        .read(cx)
        .active_workspace()
        .cloned()
        .or_else(|| store.read(cx).workspaces().first().cloned())
    else {
        workspace.show_toast(
            Toast::new(
                NotificationId::unique::<SuperzentSidebar>(),
                "Select a workspace first.",
            ),
            cx,
        );
        return;
    };
    run_delete_workspace_entry(workspace, workspace_entry, None, window, cx);
}

fn run_close_workspace(
    _workspace: &mut Workspace,
    window: &mut gpui::Window,
    cx: &mut Context<Workspace>,
) {
    let current_workspace = cx.entity();
    let Some(multi_workspace) = window.window_handle().downcast::<MultiWorkspace>() else {
        return;
    };

    if let Err(error) = multi_workspace.update(cx, |multi_workspace, window, cx| {
        let Some(index) = multi_workspace
            .workspaces()
            .iter()
            .position(|workspace| *workspace == current_workspace)
        else {
            return;
        };

        multi_workspace
            .close_workspace_at_index(index, window, cx)
            .detach_and_log_err(cx);
    }) {
        log::error!("failed to close workspace in current window: {error:#}");
    }
}

#[derive(Clone)]
struct DeleteWorkspaceSession {
    project: ProjectEntry,
    workspace_entry: WorkspaceEntry,
    fallback_workspace: Option<WorkspaceEntry>,
    deleting_sidebar: Option<WeakEntity<SuperzentSidebar>>,
    delete_location: WorkspaceLocation,
    removal_targets: Vec<WorkspaceWindowRemovalTarget>,
    app_state: Arc<WorkspaceAppState>,
}

enum DeleteWorkspaceResult {
    Deleted { cleanup_error: Option<String> },
    BlockedByTeardown(superzent_git::WorkspaceLifecycleFailure),
}

impl DeleteWorkspaceSession {
    fn set_sidebar_deleting(&self, deleting: bool, cx: &mut App) {
        if let Some(sidebar) = self.deleting_sidebar.as_ref() {
            sidebar
                .update(cx, |sidebar, cx| {
                    sidebar.mark_workspace_deleting(&self.workspace_entry.id, deleting, cx);
                })
                .ok();
        }
    }

    fn register_in_flight(&self, cx: &mut App) {
        register_in_flight_workspace_delete(self.delete_location.clone(), cx);
    }

    fn finish_interaction(&self, cx: &mut AsyncWindowContext) {
        cx.update(|_, cx| {
            self.set_sidebar_deleting(false, cx);
            unregister_in_flight_workspace_delete(&self.delete_location, cx);
        })
        .ok();
    }

    async fn close_open_windows(&self, cx: &mut AsyncWindowContext) -> Option<String> {
        close_workspace_in_all_windows(
            self.workspace_entry.clone(),
            self.fallback_workspace.clone(),
            self.app_state.clone(),
            Some(self.removal_targets.clone()),
            cx,
        )
        .await
        .err()
        .map(|error| error.to_string())
    }

    fn finalize_deleted(
        &self,
        store: &Entity<SuperzentStore>,
        workspace: &mut Workspace,
        cleanup_error: Option<String>,
        cx: &mut Context<Workspace>,
    ) {
        store.update(cx, |store, cx| {
            store.remove_workspace(&self.workspace_entry.id, cx);
        });
        if let Some(cleanup_error) = cleanup_error {
            log::error!("workspace delete succeeded on disk but cleanup failed: {cleanup_error}");
            workspace.show_toast(
                Toast::new(
                    NotificationId::unique::<SuperzentSidebar>(),
                    format!(
                        "Deleted {}, but some open windows may need manual cleanup.",
                        self.workspace_entry.name
                    ),
                ),
                cx,
            );
        } else {
            workspace.show_toast(
                Toast::new(
                    NotificationId::unique::<SuperzentSidebar>(),
                    format!("Deleted {}", self.workspace_entry.name),
                ),
                cx,
            );
        }
    }
}

fn show_delete_workspace_modal(
    workspace: &mut Workspace,
    workspace_entry: WorkspaceEntry,
    delete_resolution: superzent_git::WorkspaceDeleteResolution,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> SmolReceiver<DeleteWorkspaceModalDecision> {
    let prompt_details = delete_workspace_prompt_details(&workspace_entry, &delete_resolution);
    let (sender, receiver) = smol::channel::bounded(1);
    let dismiss_sender = sender;
    let modal_sender = dismiss_sender.clone();

    workspace.toggle_modal(window, cx, move |_window, cx| {
        DeleteWorkspaceModal::new(prompt_details.clone(), modal_sender, cx)
    });

    if let Some(modal) = workspace.active_modal::<DeleteWorkspaceModal>(cx) {
        cx.subscribe_in(&modal, window, move |_, _, _: &DismissEvent, _, _| {
            let _ = dismiss_sender.try_send(DeleteWorkspaceModalDecision::Cancel);
        })
        .detach();
    } else {
        let _ = dismiss_sender.try_send(DeleteWorkspaceModalDecision::Cancel);
    }

    receiver
}

async fn perform_workspace_delete(
    session: &DeleteWorkspaceSession,
    store: &Entity<SuperzentStore>,
    delete_resolution: Option<&superzent_git::WorkspaceDeleteResolution>,
    force: bool,
    cx: &mut AsyncWindowContext,
) -> anyhow::Result<DeleteWorkspaceResult> {
    match &session.project.location {
        ProjectLocation::Local { repo_root } => {
            let workspace_to_delete = session.workspace_entry.clone();
            let delete_resolution = delete_resolution.cloned();
            let delete_outcome = cx
                .background_spawn({
                    let repo_root = repo_root.clone();
                    async move {
                        superzent_git::delete_workspace_with_resolution(
                            &workspace_to_delete,
                            &repo_root,
                            force,
                            delete_resolution.as_ref(),
                        )
                    }
                })
                .await?;

            match delete_outcome {
                superzent_git::WorkspaceDeleteOutcome::Deleted => {}
                superzent_git::WorkspaceDeleteOutcome::BlockedByTeardown(failure) => {
                    return Ok(DeleteWorkspaceResult::BlockedByTeardown(failure));
                }
            }
        }
        ProjectLocation::Ssh { .. } => {
            let (project_workspace, _) = resolve_remote_project_workspace(
                &session.project,
                store,
                session.app_state.clone(),
                false,
                cx,
            )
            .await?;

            let repository = cx.update(|_, cx| {
                active_repository_for_workspace(&project_workspace, cx)
                    .ok_or_else(|| anyhow::anyhow!("no active repository found"))
            })??;
            let target_path = PathBuf::from(
                session
                    .workspace_entry
                    .ssh_worktree_path()
                    .ok_or_else(|| anyhow::anyhow!("missing remote worktree path"))?,
            );
            let receiver = repository.update(cx, |repository, _| {
                repository.remove_worktree(target_path, false)
            });
            receiver.await??;
        }
    }

    Ok(DeleteWorkspaceResult::Deleted {
        cleanup_error: session.close_open_windows(cx).await,
    })
}

fn run_delete_workspace_entry(
    workspace: &mut Workspace,
    workspace_entry: WorkspaceEntry,
    deleting_sidebar: Option<WeakEntity<SuperzentSidebar>>,
    window: &mut gpui::Window,
    cx: &mut Context<Workspace>,
) {
    let store = SuperzentStore::global(cx);
    let clear_sidebar_deleting = |cx: &mut App| {
        if let Some(sidebar) = deleting_sidebar.as_ref() {
            sidebar
                .update(cx, |sidebar, cx| {
                    sidebar.mark_workspace_deleting(&workspace_entry.id, false, cx);
                })
                .ok();
        }
    };

    if workspace_entry.kind == WorkspaceKind::Primary || !workspace_entry.managed {
        workspace.show_toast(
            Toast::new(
                NotificationId::unique::<SuperzentSidebar>(),
                "Primary workspaces cannot be deleted.",
            ),
            cx,
        );
        clear_sidebar_deleting(cx);
        return;
    }
    let Some(project) = store.read(cx).project(&workspace_entry.project_id).cloned() else {
        workspace.show_toast(
            Toast::new(
                NotificationId::unique::<SuperzentSidebar>(),
                "Missing project metadata.",
            ),
            cx,
        );
        clear_sidebar_deleting(cx);
        return;
    };
    let session = DeleteWorkspaceSession {
        project,
        workspace_entry: workspace_entry.clone(),
        fallback_workspace: store
            .read(cx)
            .primary_workspace_for_project(&workspace_entry.project_id)
            .filter(|fallback_workspace| fallback_workspace.id != workspace_entry.id)
            .cloned(),
        deleting_sidebar: deleting_sidebar.clone(),
        delete_location: workspace_entry.location.clone(),
        removal_targets: workspace_removal_targets_for_entry(&workspace_entry, cx),
        app_state: workspace.app_state().clone(),
    };

    match &session.project.location {
        ProjectLocation::Local { repo_root } => {
            let delete_resolution = match superzent_git::resolve_workspace_delete_resolution(
                &session.workspace_entry,
                repo_root,
            ) {
                Ok(delete_resolution) => delete_resolution,
                Err(error) => {
                    workspace.show_toast(
                        Toast::new(
                            NotificationId::unique::<SuperzentSidebar>(),
                            format!("Failed to prepare workspace delete: {error}"),
                        ),
                        cx,
                    );
                    clear_sidebar_deleting(cx);
                    return;
                }
            };
            session.register_in_flight(cx);
            let prompt_receiver = show_delete_workspace_modal(
                workspace,
                session.workspace_entry.clone(),
                delete_resolution.clone(),
                window,
                cx,
            );

            cx.spawn_in(window, async move |this, cx| {
                let delete_decision = prompt_receiver
                    .recv()
                    .await
                    .unwrap_or(DeleteWorkspaceModalDecision::Cancel);
                if delete_decision == DeleteWorkspaceModalDecision::Cancel {
                    session.finish_interaction(cx);
                    return anyhow::Ok(());
                }

                let mut delete_result = perform_workspace_delete(
                    &session,
                    &store,
                    Some(&delete_resolution),
                    delete_decision == DeleteWorkspaceModalDecision::DeleteAnyway,
                    cx,
                )
                .await;

                if let Ok(DeleteWorkspaceResult::BlockedByTeardown(failure)) = &delete_result {
                    let prompt_detail = workspace_lifecycle_failure_prompt_detail(
                        &session.workspace_entry,
                        failure,
                        None,
                        true,
                    );
                    let retry_prompt = this.update_in(cx, |workspace, window, cx| {
                        workspace.show_toast(
                            Toast::new(
                                NotificationId::unique::<SuperzentSidebar>(),
                                failure.summary(),
                            ),
                            cx,
                        );
                        window.prompt(
                            PromptLevel::Warning,
                            "Workspace teardown failed",
                            Some(&prompt_detail),
                            &["Cancel", "Delete Anyway"],
                            cx,
                        )
                    })?;

                    if retry_prompt.await == Ok(1) {
                        delete_result = perform_workspace_delete(
                            &session,
                            &store,
                            Some(&delete_resolution),
                            true,
                            cx,
                        )
                        .await;
                        if matches!(
                            delete_result,
                            Ok(DeleteWorkspaceResult::BlockedByTeardown(_))
                        ) {
                            delete_result = Err(anyhow::anyhow!(
                                "force delete unexpectedly retried teardown"
                            ));
                        }
                    }
                }

                let _ = this.update_in(cx, |workspace, _window, cx| match delete_result {
                    Ok(DeleteWorkspaceResult::Deleted { cleanup_error }) => {
                        session.finalize_deleted(&store, workspace, cleanup_error, cx);
                    }
                    Ok(DeleteWorkspaceResult::BlockedByTeardown(_)) => {}
                    Err(error) => {
                        workspace.show_toast(
                            Toast::new(
                                NotificationId::unique::<SuperzentSidebar>(),
                                format!("Failed to remove workspace: {error}"),
                            ),
                            cx,
                        );
                    }
                });

                session.finish_interaction(cx);
                anyhow::Ok(())
            })
            .detach_and_log_err(cx);
        }
        ProjectLocation::Ssh { .. } => {
            session.register_in_flight(cx);
            let prompt = window.prompt(
                PromptLevel::Warning,
                "Delete workspace?",
                Some(&format!(
                    "Delete `{}` and remove its worktree at {}?",
                    session.workspace_entry.name,
                    session.workspace_entry.display_path()
                )),
                &["Cancel", "Delete"],
                cx,
            );

            cx.spawn_in(window, async move |this, cx| {
                if prompt.await != Ok(1) {
                    session.finish_interaction(cx);
                    return anyhow::Ok(());
                }

                let delete_result =
                    perform_workspace_delete(&session, &store, None, false, cx).await;

                let _ = this.update_in(cx, |workspace, _window, cx| match delete_result {
                    Ok(DeleteWorkspaceResult::Deleted { cleanup_error }) => {
                        session.finalize_deleted(&store, workspace, cleanup_error, cx);
                    }
                    Ok(DeleteWorkspaceResult::BlockedByTeardown(_)) => {}
                    Err(error) => {
                        workspace.show_toast(
                            Toast::new(
                                NotificationId::unique::<SuperzentSidebar>(),
                                format!("Failed to remove workspace: {error}"),
                            ),
                            cx,
                        );
                    }
                });

                session.finish_interaction(cx);
                anyhow::Ok(())
            })
            .detach_and_log_err(cx);
        }
    }
}

fn run_sync_project_worktrees(
    workspace: &mut Workspace,
    project: ProjectEntry,
    window: &mut gpui::Window,
    cx: &mut Context<Workspace>,
) {
    let ProjectLocation::Local { repo_root } = &project.location else {
        workspace.show_toast(
            Toast::new(
                NotificationId::unique::<SuperzentSidebar>(),
                "Worktree sync is only available for local projects.",
            ),
            cx,
        );
        return;
    };

    let store = SuperzentStore::global(cx);
    let repo_root = repo_root.clone();
    let app_state = workspace.app_state().clone();

    workspace.show_toast(
        Toast::new(
            NotificationId::unique::<SuperzentSidebar>(),
            format!("Syncing worktrees for {}...", project.name),
        ),
        cx,
    );

    cx.spawn_in(window, async move |this, cx| {
        let sync_result: anyhow::Result<()> = async {
            let discovered_worktrees = cx
                .background_spawn(async move { superzent_git::discover_worktrees(&repo_root) })
                .await?;

            let (
                workspaces_to_upsert,
                removed_workspaces,
                fallback_workspace,
                existing_workspace_ids,
            ) = cx.update(|_, cx| {
                let store = store.read(cx);
                let existing_workspaces = store
                    .workspaces_for_project(&project.id)
                    .into_iter()
                    .cloned()
                    .collect::<Vec<_>>();
                let existing_workspace_ids = existing_workspaces
                    .iter()
                    .map(|workspace| workspace.id.clone())
                    .collect::<BTreeSet<_>>();
                let discovered_paths = discovered_worktrees
                    .iter()
                    .map(|worktree| worktree.path.clone())
                    .collect::<BTreeSet<_>>();
                let workspaces_to_upsert = discovered_worktrees
                    .iter()
                    .filter_map(|worktree| {
                        build_synced_local_workspace_entry(&project, worktree, &store)
                    })
                    .collect::<Vec<_>>();
                let fallback_workspace = workspaces_to_upsert
                    .iter()
                    .find(|workspace| workspace.is_primary())
                    .cloned()
                    .or_else(|| store.primary_workspace_for_project(&project.id).cloned());
                let removed_workspaces = existing_workspaces
                    .into_iter()
                    .filter(|workspace| workspace.managed && !workspace.is_primary())
                    .filter(|workspace| {
                        workspace
                            .local_worktree_path()
                            .is_some_and(|worktree_path| !discovered_paths.contains(worktree_path))
                    })
                    .collect::<Vec<_>>();

                (
                    workspaces_to_upsert,
                    removed_workspaces,
                    fallback_workspace,
                    existing_workspace_ids,
                )
            })?;

            for removed_workspace in &removed_workspaces {
                close_workspace_in_all_windows(
                    removed_workspace.clone(),
                    fallback_workspace.clone(),
                    app_state.clone(),
                    None,
                    cx,
                )
                .await?;
            }

            let removed_workspace_ids = removed_workspaces
                .iter()
                .map(|workspace| workspace.id.clone())
                .collect::<Vec<_>>();
            let added_count = workspaces_to_upsert
                .iter()
                .filter(|workspace| !existing_workspace_ids.contains(&workspace.id))
                .count();
            let refreshed_count = workspaces_to_upsert.len().saturating_sub(added_count);
            let removed_count = removed_workspace_ids.len();

            this.update_in(cx, |workspace, _window, cx| {
                store.update(cx, |store, cx| {
                    store.sync_workspaces(
                        workspaces_to_upsert.clone(),
                        removed_workspace_ids.clone(),
                        cx,
                    );
                });
                workspace.show_toast(
                    Toast::new(
                        NotificationId::unique::<SuperzentSidebar>(),
                        project_worktree_sync_message(
                            &project.name,
                            added_count,
                            removed_count,
                            refreshed_count,
                        ),
                    ),
                    cx,
                );
            })?;

            Ok(())
        }
        .await;

        if let Err(error) = sync_result {
            this.update_in(cx, |workspace, _window, cx| {
                workspace.show_toast(
                    Toast::new(
                        NotificationId::unique::<SuperzentSidebar>(),
                        format!("Failed to sync worktrees: {error}"),
                    ),
                    cx,
                );
            })
            .ok();
        }

        anyhow::Ok(())
    })
    .detach_and_log_err(cx);
}

fn run_close_project(
    workspace: &mut Workspace,
    project_id: &str,
    window: &mut gpui::Window,
    cx: &mut Context<Workspace>,
) {
    let store = SuperzentStore::global(cx);
    let Some(project) = store.read(cx).project(project_id).cloned() else {
        workspace.show_toast(
            Toast::new(
                NotificationId::unique::<SuperzentSidebar>(),
                "Missing project metadata.",
            ),
            cx,
        );
        return;
    };

    let project_workspaces = store
        .read(cx)
        .workspaces_for_project(project_id)
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    let workspace_count = project_workspaces.len();
    let fallback_workspace = store
        .read(cx)
        .workspaces()
        .iter()
        .find(|workspace_entry| workspace_entry.project_id != project_id)
        .cloned();
    let prompt = window.prompt(
        PromptLevel::Warning,
        "Close project?",
        Some(&format!(
            "Close `{}` and remove its {} from superzent?\n\nFiles, worktrees, and git history will remain on disk.",
            project.name,
            project_workspace_label(workspace_count),
        )),
        &["Cancel", "Close Project"],
        cx,
    );

    let app_state = workspace.app_state().clone();
    let invoking_window = window.window_handle().downcast::<MultiWorkspace>();
    let current_workspace = cx.entity().downgrade();
    let project_id = project.id.clone();
    let project_name = project.name.clone();

    cx.spawn_in(window, async move |_this, cx| {
        if prompt.await != Ok(1) {
            return anyhow::Ok(());
        }

        let close_result = close_project_in_all_windows(
            project.location.clone(),
            project_workspaces.clone(),
            fallback_workspace,
            app_state,
            cx,
        )
        .await;

        match close_result {
            Ok(()) => {
                store.update(cx, |store, cx| {
                    store.remove_project(&project_id, cx);
                });
                show_project_close_toast(
                    invoking_window,
                    current_workspace.clone(),
                    format!("Closed {project_name}"),
                    cx,
                );
            }
            Err(error) => {
                show_project_close_toast(
                    invoking_window,
                    current_workspace.clone(),
                    format!("Failed to close project: {error}"),
                    cx,
                );
            }
        }

        anyhow::Ok(())
    })
    .detach_and_log_err(cx);
}

async fn close_project_in_all_windows(
    project_location: ProjectLocation,
    project_workspaces: Vec<WorkspaceEntry>,
    fallback_workspace: Option<WorkspaceEntry>,
    app_state: Arc<WorkspaceAppState>,
    cx: &mut gpui::AsyncApp,
) -> anyhow::Result<()> {
    let Some(serialized_location) = serialized_workspace_location_for_project(&project_location)
    else {
        return Ok(());
    };
    let workspace_windows =
        cx.update(|cx| workspace_windows_for_location(&serialized_location, cx));

    for workspace_window in workspace_windows {
        let matching_indexes = match workspace_window.update(cx, |multi_workspace, _, cx| {
            matching_workspace_indexes(multi_workspace, &project_workspaces, cx)
        }) {
            Ok(matching_indexes) => matching_indexes,
            Err(_) => continue,
        };

        if matching_indexes.is_empty() {
            continue;
        }

        let workspace_count = match workspace_window.update(cx, |multi_workspace, _, _| {
            multi_workspace.workspaces().len()
        }) {
            Ok(workspace_count) => workspace_count,
            Err(_) => continue,
        };

        if matching_indexes.len() == workspace_count {
            ensure_project_close_fallback(
                workspace_window,
                fallback_workspace.clone(),
                app_state.clone(),
                cx,
            )
            .await?;
        }

        let matching_indexes = match workspace_window.update(cx, |multi_workspace, _, cx| {
            matching_workspace_indexes(multi_workspace, &project_workspaces, cx)
        }) {
            Ok(matching_indexes) => matching_indexes,
            Err(_) => continue,
        };

        for index in matching_indexes.into_iter().rev() {
            if workspace_window
                .update(cx, |multi_workspace, window, cx| {
                    multi_workspace.remove_workspace(index, window, cx);
                })
                .is_err()
            {
                break;
            }
        }
    }

    Ok(())
}

async fn close_workspace_in_all_windows(
    workspace_entry: WorkspaceEntry,
    fallback_workspace: Option<WorkspaceEntry>,
    app_state: Arc<WorkspaceAppState>,
    removal_targets: Option<Vec<WorkspaceWindowRemovalTarget>>,
    cx: &mut gpui::AsyncApp,
) -> anyhow::Result<()> {
    let mut removal_targets = removal_targets.unwrap_or_default();
    let current_removal_targets =
        cx.update(|cx| workspace_removal_targets_for_entry(&workspace_entry, cx));
    if removal_targets.is_empty() {
        removal_targets = current_removal_targets;
    } else {
        removal_targets.extend(current_removal_targets);
    }

    for removal_target in removal_targets {
        let workspace_window = removal_target.window;
        let workspace_ids = removal_target.workspace_ids;

        let matching_indexes = match workspace_window.update(cx, |multi_workspace, _, _| {
            matching_workspace_indexes_for_ids(multi_workspace, &workspace_ids)
        }) {
            Ok(matching_indexes) => matching_indexes,
            Err(_) => continue,
        };

        if matching_indexes.is_empty() {
            continue;
        }

        let workspace_count = match workspace_window.update(cx, |multi_workspace, _, _| {
            multi_workspace.workspaces().len()
        }) {
            Ok(workspace_count) => workspace_count,
            Err(_) => continue,
        };

        if matching_indexes.len() == workspace_count {
            ensure_project_close_fallback(
                workspace_window,
                fallback_workspace.clone(),
                app_state.clone(),
                cx,
            )
            .await?;
        }

        let matching_indexes = match workspace_window.update(cx, |multi_workspace, _, _| {
            matching_workspace_indexes_for_ids(multi_workspace, &workspace_ids)
        }) {
            Ok(matching_indexes) => matching_indexes,
            Err(_) => continue,
        };

        for index in matching_indexes.into_iter().rev() {
            if workspace_window
                .update(cx, |multi_workspace, window, cx| {
                    multi_workspace.remove_workspace(index, window, cx);
                })
                .is_err()
            {
                break;
            }
        }
    }

    Ok(())
}

async fn ensure_project_close_fallback(
    workspace_window: WindowHandle<MultiWorkspace>,
    fallback_workspace: Option<WorkspaceEntry>,
    app_state: Arc<WorkspaceAppState>,
    cx: &mut gpui::AsyncApp,
) -> anyhow::Result<()> {
    if let Some(workspace_entry) = fallback_workspace {
        let open_result = match workspace_window.update(cx, |_, window, cx| {
            open_workspace_entry(workspace_entry.clone(), app_state.clone(), window, cx)
        }) {
            Ok(task) => task.await,
            Err(_) => return Ok(()),
        };

        match open_result {
            Ok(_) => return Ok(()),
            Err(_error) => {
                if !cx.update(|cx| workspace_window.read(cx).is_ok()) {
                    return Ok(());
                }
            }
        }
    }

    if !cx.update(|cx| workspace_window.read(cx).is_ok()) {
        return Ok(());
    }

    cx.update(|cx| {
        Workspace::new_local(
            vec![],
            app_state,
            Some(workspace_window),
            None,
            None,
            true,
            cx,
        )
    })
    .await?;

    Ok(())
}

fn matching_workspace_indexes(
    multi_workspace: &MultiWorkspace,
    project_workspaces: &[WorkspaceEntry],
    cx: &App,
) -> Vec<usize> {
    multi_workspace
        .workspaces()
        .iter()
        .enumerate()
        .filter_map(|(index, workspace_handle)| {
            project_workspaces
                .iter()
                .any(|workspace_entry| {
                    workspace_matches_entry(workspace_handle, workspace_entry, cx)
                })
                .then_some(index)
        })
        .collect()
}

#[derive(Clone)]
struct WorkspaceWindowRemovalTarget {
    window: WindowHandle<MultiWorkspace>,
    workspace_ids: Vec<EntityId>,
}

fn workspace_removal_targets_for_entry(
    workspace_entry: &WorkspaceEntry,
    cx: &App,
) -> Vec<WorkspaceWindowRemovalTarget> {
    let Some(serialized_location) = serialized_workspace_location_for_workspace(workspace_entry)
    else {
        return Vec::new();
    };

    workspace_windows_for_location(&serialized_location, cx)
        .into_iter()
        .filter_map(|window| {
            // `WindowHandle::read` returns Err for a stack-locked current
            // window instead of panicking (unlike `read_with`), so this
            // iteration is safe even when called from inside `cx.update`.
            let multi_workspace = window.read(cx).ok()?;
            let workspace_ids: Vec<_> = multi_workspace
                .workspaces()
                .iter()
                .filter(|workspace| workspace_matches_entry(workspace, workspace_entry, cx))
                .map(|workspace| workspace.entity_id())
                .collect();

            (!workspace_ids.is_empty()).then_some(WorkspaceWindowRemovalTarget {
                window,
                workspace_ids,
            })
        })
        .collect()
}

fn matching_workspace_indexes_for_ids(
    multi_workspace: &MultiWorkspace,
    workspace_ids: &[EntityId],
) -> Vec<usize> {
    multi_workspace
        .workspaces()
        .iter()
        .enumerate()
        .filter_map(|(index, workspace_handle)| {
            workspace_ids
                .iter()
                .any(|workspace_id| *workspace_id == workspace_handle.entity_id())
                .then_some(index)
        })
        .collect()
}

fn show_project_close_toast(
    invoking_window: Option<WindowHandle<MultiWorkspace>>,
    current_workspace: WeakEntity<Workspace>,
    message: String,
    cx: &mut gpui::AsyncApp,
) {
    if let Some(window_handle) = invoking_window {
        if window_handle
            .update(cx, |multi_workspace, _, cx| {
                let active_workspace = multi_workspace.workspace().clone();
                active_workspace.update(cx, |workspace, cx| {
                    workspace.show_toast(
                        Toast::new(
                            NotificationId::unique::<SuperzentSidebar>(),
                            message.clone(),
                        ),
                        cx,
                    );
                });
            })
            .is_ok()
        {
            return;
        }
    }

    if let Ok(()) = current_workspace.update(cx, |workspace, cx| {
        workspace.show_toast(
            Toast::new(
                NotificationId::unique::<SuperzentSidebar>(),
                message.clone(),
            ),
            cx,
        );
    }) {
        return;
    }
}

fn run_delete_workspace_from_store(
    workspace_handle: Entity<Workspace>,
    workspace_entry: WorkspaceEntry,
    deleting_sidebar: Option<WeakEntity<SuperzentSidebar>>,
    window: &mut gpui::Window,
    cx: &mut App,
) {
    workspace_handle.update(cx, |workspace, cx| {
        run_delete_workspace_entry(
            workspace,
            workspace_entry.clone(),
            deleting_sidebar.clone(),
            window,
            cx,
        );
    });
}

fn run_sync_project_worktrees_from_store(
    workspace_handle: Entity<Workspace>,
    project: ProjectEntry,
    window: &mut gpui::Window,
    cx: &mut App,
) {
    workspace_handle.update(cx, |workspace, cx| {
        run_sync_project_worktrees(workspace, project.clone(), window, cx);
    });
}

fn run_close_project_from_store(
    workspace_handle: Entity<Workspace>,
    project_id: String,
    window: &mut gpui::Window,
    cx: &mut App,
) {
    workspace_handle.update(cx, |workspace, cx| {
        run_close_project(workspace, &project_id, window, cx);
    });
}

fn run_open_workspace_in_new_window_from_store(
    workspace_handle: Entity<Workspace>,
    window: &mut gpui::Window,
    cx: &mut App,
) {
    workspace_handle.update(cx, |workspace, cx| {
        run_open_workspace_in_new_window(workspace, window, cx);
    });
}

fn run_add_project_from_store(
    workspace_handle: Entity<Workspace>,
    window: &mut gpui::Window,
    cx: &mut App,
) {
    workspace_handle.update(cx, |workspace, cx| {
        run_add_project(workspace, window, cx);
    });
}

fn open_local_workspace_path(
    path: PathBuf,
    app_state: Arc<WorkspaceAppState>,
    window: &mut gpui::Window,
    cx: &mut App,
) -> Task<anyhow::Result<()>> {
    let Some(multi_workspace) = window.window_handle().downcast::<MultiWorkspace>() else {
        let task = Workspace::new_local(vec![path], app_state, None, None, None, true, cx);
        return cx.spawn(async move |_| {
            task.await?;
            anyhow::Ok(())
        });
    };

    if let Ok(multi_workspace_ref) = multi_workspace.read(cx)
        && let Some(index) = multi_workspace_ref
            .workspaces()
            .iter()
            .enumerate()
            .find_map(|(index, workspace)| {
                workspace_contains_local_worktree_path(workspace, &path, cx).then_some(index)
            })
    {
        return cx.spawn(async move |cx| {
            multi_workspace.update(cx, |multi_workspace, window, cx| {
                window.activate_window();
                multi_workspace.activate_index(index, window, cx);
            })?;
            anyhow::Ok(())
        });
    }

    let task = Workspace::new_local(
        vec![path],
        app_state,
        Some(multi_workspace),
        None,
        None,
        true,
        cx,
    );
    cx.spawn(async move |_| {
        task.await?;
        anyhow::Ok(())
    })
}

fn open_workspace_entry(
    workspace_entry: WorkspaceEntry,
    app_state: Arc<WorkspaceAppState>,
    window: &mut gpui::Window,
    cx: &mut App,
) -> Task<anyhow::Result<()>> {
    if is_workspace_location_being_deleted(&workspace_entry.location, cx) {
        if let Some(current_workspace) = workspace_from_window(window, cx) {
            show_workspace_toast(
                &current_workspace,
                format!("Workspace `{}` is being deleted.", workspace_entry.name),
                cx,
            );
        }
        return Task::ready(Err(anyhow::anyhow!(
            "workspace `{}` is being deleted",
            workspace_entry.name
        )));
    }

    match &workspace_entry.location {
        WorkspaceLocation::Local { worktree_path } => {
            open_local_workspace_path(worktree_path.clone(), app_state, window, cx)
        }
        WorkspaceLocation::Ssh {
            connection,
            worktree_path,
        } => {
            let Some(remote_connection) = remote_connection_options_from_stored(connection) else {
                return Task::ready(Err(anyhow::anyhow!(
                    "unsupported remote workspace configuration"
                )));
            };

            let replace_window = window.window_handle().downcast::<MultiWorkspace>();
            let path = PathBuf::from(worktree_path);
            window.spawn(cx, async move |cx| {
                open_remote_project(
                    remote_connection,
                    vec![path],
                    app_state,
                    OpenOptions {
                        open_new_workspace: Some(true),
                        replace_window,
                        ..Default::default()
                    },
                    cx,
                )
                .await?;
                anyhow::Ok(())
            })
        }
    }
}

fn workspace_from_window(window: &gpui::Window, cx: &App) -> Option<Entity<Workspace>> {
    let multi_workspace = window.window_handle().downcast::<MultiWorkspace>()?;
    let multi_workspace = multi_workspace.read(cx).ok()?;
    Some(multi_workspace.workspace().clone())
}

fn workspace_location_candidates(
    workspace: &Entity<Workspace>,
    cx: &App,
) -> Vec<WorkspaceLocation> {
    let project = workspace.read(cx).project();
    let project = project.read(cx);

    if let Some(connection) = project.remote_connection_options(cx) {
        let Some(connection) = stored_ssh_connection_from_options(&connection) else {
            return Vec::new();
        };
        return project
            .visible_worktrees(cx)
            .map(|worktree| WorkspaceLocation::Ssh {
                connection: connection.clone(),
                worktree_path: worktree.read(cx).abs_path().to_string_lossy().into_owned(),
            })
            .collect();
    }

    if project.is_local() {
        return project
            .visible_worktrees(cx)
            .filter_map(|worktree| {
                worktree
                    .read(cx)
                    .as_local()
                    .map(|local| WorkspaceLocation::Local {
                        worktree_path: local.abs_path().to_path_buf(),
                    })
            })
            .collect();
    }

    Vec::new()
}

fn single_workspace_location_snapshot(
    workspace: &Entity<Workspace>,
    cx: &App,
) -> Option<WorkspaceLocation> {
    let mut candidate_locations = workspace_location_candidates(workspace, cx);
    if candidate_locations.len() == 1 {
        candidate_locations.pop()
    } else {
        None
    }
}

fn workspace_entry_matches_candidate_locations(
    workspace_entry: &WorkspaceEntry,
    candidate_locations: &[WorkspaceLocation],
) -> bool {
    candidate_locations
        .iter()
        .any(|location| workspace_entry.matches_locator(&workspace_location_to_locator(location)))
}

fn matched_workspace_ids_for_candidate_locations(
    candidate_locations: &[WorkspaceLocation],
    workspace_entries: &[WorkspaceEntry],
) -> Vec<String> {
    let mut workspace_ids = Vec::new();

    for location in candidate_locations {
        let Some(workspace_entry) = workspace_entries.iter().find(|workspace_entry| {
            workspace_entry.matches_locator(&workspace_location_to_locator(location))
        }) else {
            continue;
        };

        if workspace_ids
            .iter()
            .all(|workspace_id| workspace_id != &workspace_entry.id)
        {
            workspace_ids.push(workspace_entry.id.clone());
        }
    }

    workspace_ids
}

fn matched_workspace_id_for_candidate_locations(
    candidate_locations: &[WorkspaceLocation],
    workspace_entries: &[WorkspaceEntry],
    preferred_workspace_id: Option<&str>,
) -> Option<String> {
    let matched_workspace_ids =
        matched_workspace_ids_for_candidate_locations(candidate_locations, workspace_entries);

    if let Some(preferred_workspace_id) = preferred_workspace_id
        && matched_workspace_ids
            .iter()
            .any(|workspace_id| workspace_id == preferred_workspace_id)
    {
        return Some(preferred_workspace_id.to_string());
    }

    if matched_workspace_ids.len() == 1 {
        matched_workspace_ids.first().cloned()
    } else {
        None
    }
}

fn store_workspace_id_for_live_workspace(
    workspace: &Entity<Workspace>,
    store: &SuperzentStore,
    preferred_workspace_id: Option<&str>,
    cx: &App,
) -> Option<String> {
    let candidate_locations = workspace_location_candidates(workspace, cx);
    matched_workspace_id_for_candidate_locations(
        &candidate_locations,
        store.workspaces(),
        preferred_workspace_id,
    )
}

fn workspace_contains_local_worktree_path(
    workspace: &Entity<Workspace>,
    path: &Path,
    cx: &App,
) -> bool {
    workspace_location_candidates(workspace, cx)
        .iter()
        .any(|candidate_location| match candidate_location {
            WorkspaceLocation::Local { worktree_path } => worktree_path == path,
            WorkspaceLocation::Ssh { .. } => false,
        })
}

fn live_project_location_for_workspace(
    workspace: &Entity<Workspace>,
    cx: &App,
) -> Option<ProjectLocation> {
    let project = workspace.read(cx).project();
    let project = project.read(cx);
    let repository = project
        .active_repository(cx)
        .or_else(|| project.repositories(cx).values().next().cloned());

    if let Some(connection) = project.remote_connection_options(cx) {
        let connection = stored_ssh_connection_from_options(&connection)?;
        let repository = repository?;
        let repo_root = repository
            .read(cx)
            .original_repo_abs_path
            .to_string_lossy()
            .into_owned();
        return Some(ProjectLocation::Ssh {
            connection,
            repo_root,
        });
    }

    if project.is_local() {
        let repository = repository?;
        return Some(ProjectLocation::Local {
            repo_root: repository.read(cx).original_repo_abs_path.to_path_buf(),
        });
    }

    None
}

fn inferred_project_id_for_live_workspace(
    workspace: &Entity<Workspace>,
    store: &SuperzentStore,
    cx: &App,
) -> Option<String> {
    let candidate_locations = workspace_location_candidates(workspace, cx);
    let mut project_ids = Vec::new();

    for workspace_id in
        matched_workspace_ids_for_candidate_locations(&candidate_locations, store.workspaces())
    {
        let Some(project_id) = store
            .workspace(&workspace_id)
            .map(|workspace| workspace.project_id.clone())
        else {
            continue;
        };
        if project_ids
            .iter()
            .all(|existing_project_id| existing_project_id != &project_id)
        {
            project_ids.push(project_id);
        }
    }

    if project_ids.len() == 1 {
        return project_ids.first().cloned();
    }

    let project_location = live_project_location_for_workspace(workspace, cx)?;
    store
        .project_for_location(&project_location)
        .map(|project| project.id.clone())
}

fn workspace_matches_entry(
    workspace: &Entity<Workspace>,
    workspace_entry: &WorkspaceEntry,
    cx: &App,
) -> bool {
    let candidate_locations = workspace_location_candidates(workspace, cx);
    workspace_entry_matches_candidate_locations(workspace_entry, &candidate_locations)
}

fn workspace_for_entry_in_window(
    window: &Window,
    cx: &App,
    workspace_entry: &WorkspaceEntry,
) -> Option<Entity<Workspace>> {
    if let Some(multi_workspace) = window.window_handle().downcast::<MultiWorkspace>() {
        let multi_workspace = multi_workspace.read(cx).ok()?;
        return multi_workspace
            .workspaces()
            .iter()
            .find(|workspace| workspace_matches_entry(workspace, workspace_entry, cx))
            .cloned();
    }

    workspace_from_window(window, cx)
        .filter(|workspace| workspace_matches_entry(workspace, workspace_entry, cx))
}

fn workspace_for_entry_in_any_window(
    workspace_entry: &WorkspaceEntry,
    cx: &App,
) -> Option<Entity<Workspace>> {
    // Uses `WindowHandle::read` (graceful Err on stack-lock) so this helper
    // is safe to call from inside `cx.update(|_, cx| ...)` — the current
    // window is skipped rather than panicking. Callers that must also match
    // the current window should check it first via `workspace_for_entry_in_window`.
    ordered_multi_workspace_windows(cx)
        .into_iter()
        .find_map(|handle| {
            let multi_workspace = handle.read(cx).ok()?;
            multi_workspace
                .workspaces()
                .iter()
                .find(|workspace| workspace_matches_entry(workspace, workspace_entry, cx))
                .cloned()
        })
}

/// Returns the workspace entity only if it's the *currently visible* (active)
/// tab inside some MultiWorkspace window **other than the current one**.
/// Toasts attached to a workspace are only rendered by the bottom-right
/// status layer when that workspace is the active tab, so callers that need
/// to guarantee visibility must use this helper (combined with a separate
/// current-window check) instead of `workspace_for_entry_in_any_window`,
/// which would also match hidden tabs.
///
/// Uses `WindowHandle::read` (which returns `Err` for stack-locked windows)
/// rather than `read_with` (which panics via `cx.read_window`). This means
/// the current window is automatically skipped when called from inside
/// `cx.update(|_, cx| ...)` — callers must check the current window
/// separately.
fn currently_active_workspace_for_entry(
    workspace_entry: &WorkspaceEntry,
    cx: &App,
) -> Option<Entity<Workspace>> {
    ordered_multi_workspace_windows(cx)
        .into_iter()
        .find_map(|handle| {
            let multi_workspace = handle.read(cx).ok()?;
            let active = multi_workspace.workspace().clone();
            workspace_matches_entry(&active, workspace_entry, cx).then_some(active)
        })
}

fn workspace_for_project_in_any_window(
    project_id: &str,
    store: &SuperzentStore,
    cx: &App,
) -> Option<Entity<Workspace>> {
    let project_workspaces = store
        .workspaces_for_project(project_id)
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();

    // Uses `WindowHandle::read` (graceful Err on stack-lock) so this helper
    // is safe to call from inside `cx.update(|_, cx| ...)` — the current
    // window is skipped rather than panicking.
    ordered_multi_workspace_windows(cx)
        .into_iter()
        .find_map(|handle| {
            let multi_workspace = handle.read(cx).ok()?;
            project_workspaces.iter().find_map(|workspace_entry| {
                multi_workspace
                    .workspaces()
                    .iter()
                    .find(|workspace| workspace_matches_entry(workspace, workspace_entry, cx))
                    .cloned()
            })
        })
}

fn ordered_multi_workspace_windows(cx: &App) -> Vec<WindowHandle<MultiWorkspace>> {
    cx.window_stack()
        .unwrap_or_else(|| cx.windows())
        .into_iter()
        .filter_map(|window| window.downcast::<MultiWorkspace>())
        .collect()
}

/// Locations of workspaces that are currently in the middle of a delete flow
/// (from the moment the confirmation prompt is shown until cleanup finishes).
///
/// We track these so that `open_workspace_entry` can refuse to open a workspace
/// whose worktree is about to be (or has just been) removed. Without this
/// guard, a late open during the delete prompt could leave a stale tab behind:
/// the post-delete re-scan in `close_workspace_in_all_windows` depends on
/// `visible_worktrees()`, which no longer contains the deleted path, so late
/// instances would escape cleanup.
#[derive(Default)]
struct InFlightWorkspaceDeletes {
    locations: Vec<WorkspaceLocation>,
}

impl gpui::Global for InFlightWorkspaceDeletes {}

fn register_in_flight_workspace_delete(location: WorkspaceLocation, cx: &mut App) {
    cx.default_global::<InFlightWorkspaceDeletes>()
        .locations
        .push(location);
}

fn unregister_in_flight_workspace_delete(location: &WorkspaceLocation, cx: &mut App) {
    if cx.has_global::<InFlightWorkspaceDeletes>() {
        cx.global_mut::<InFlightWorkspaceDeletes>()
            .locations
            .retain(|existing| existing != location);
    }
}

fn is_workspace_location_being_deleted(location: &WorkspaceLocation, cx: &App) -> bool {
    cx.try_global::<InFlightWorkspaceDeletes>()
        .is_some_and(|in_flight| {
            in_flight
                .locations
                .iter()
                .any(|existing| existing == location)
        })
}

fn fallback_notification_window(cx: &App) -> Option<WindowHandle<MultiWorkspace>> {
    cx.active_window()
        .and_then(|window| window.downcast::<MultiWorkspace>())
        .or_else(|| ordered_multi_workspace_windows(cx).into_iter().next())
}

fn active_repository_for_workspace(
    workspace: &Entity<Workspace>,
    cx: &App,
) -> Option<Entity<Repository>> {
    let project = workspace.read(cx).project();
    let project = project.read(cx);
    project
        .active_repository(cx)
        .or_else(|| project.repositories(cx).values().next().cloned())
}

fn detached_head_display_name(head_commit_sha: Option<&str>) -> String {
    head_commit_sha
        .map(|sha| sha.chars().take(7).collect())
        .unwrap_or_else(|| "Detached".to_string())
}

fn repository_branch_display_name(repository: &Repository) -> String {
    repository
        .branch
        .as_ref()
        .map(|branch| branch.name().to_string())
        .unwrap_or_else(|| {
            detached_head_display_name(
                repository
                    .head_commit
                    .as_ref()
                    .map(|commit| commit.sha.as_ref()),
            )
        })
}

fn workspace_location_to_locator(
    location: &WorkspaceLocation,
) -> superzent_model::WorkspaceLocator<'_> {
    match location {
        WorkspaceLocation::Local { worktree_path } => {
            superzent_model::WorkspaceLocator::Local(worktree_path)
        }
        WorkspaceLocation::Ssh {
            connection,
            worktree_path,
        } => superzent_model::WorkspaceLocator::Ssh {
            connection,
            worktree_path,
        },
    }
}

fn serialized_workspace_location_for_project(
    project_location: &ProjectLocation,
) -> Option<SerializedWorkspaceLocation> {
    match project_location {
        ProjectLocation::Local { .. } => Some(SerializedWorkspaceLocation::Local),
        ProjectLocation::Ssh { connection, .. } => {
            let remote_connection = remote_connection_options_from_stored(connection)?;
            Some(SerializedWorkspaceLocation::Remote(remote_connection))
        }
    }
}

fn serialized_workspace_location_for_workspace(
    workspace_entry: &WorkspaceEntry,
) -> Option<SerializedWorkspaceLocation> {
    match &workspace_entry.location {
        WorkspaceLocation::Local { .. } => Some(SerializedWorkspaceLocation::Local),
        WorkspaceLocation::Ssh { connection, .. } => {
            let remote_connection = remote_connection_options_from_stored(connection)?;
            Some(SerializedWorkspaceLocation::Remote(remote_connection))
        }
    }
}

fn remote_connection_options_from_stored(
    connection: &StoredSshConnection,
) -> Option<RemoteConnectionOptions> {
    Some(RemoteConnectionOptions::Ssh(SshConnectionOptions {
        host: connection.host.clone().into(),
        username: connection.username.clone(),
        port: connection.port,
        password: None,
        args: Some(connection.args.clone()),
        port_forwards: (!connection.port_forwards.is_empty()).then_some(
            connection
                .port_forwards
                .iter()
                .map(|forward| settings::SshPortForwardOption {
                    local_host: forward.local_host.clone(),
                    local_port: forward.local_port,
                    remote_host: forward.remote_host.clone(),
                    remote_port: forward.remote_port,
                })
                .collect(),
        ),
        connection_timeout: connection.connection_timeout,
        nickname: connection.nickname.clone(),
        upload_binary_over_ssh: connection.upload_binary_over_ssh,
    }))
}

fn stored_ssh_connection_from_options(
    options: &RemoteConnectionOptions,
) -> Option<StoredSshConnection> {
    match options {
        RemoteConnectionOptions::Ssh(connection) => Some(StoredSshConnection {
            host: connection.host.to_string(),
            username: connection.username.clone(),
            port: connection.port,
            args: connection.args.clone().unwrap_or_default(),
            nickname: connection.nickname.clone(),
            upload_binary_over_ssh: connection.upload_binary_over_ssh,
            port_forwards: connection
                .port_forwards
                .clone()
                .unwrap_or_default()
                .into_iter()
                .map(|forward| StoredSshPortForward {
                    local_host: forward.local_host,
                    local_port: forward.local_port,
                    remote_host: forward.remote_host,
                    remote_port: forward.remote_port,
                })
                .collect(),
            connection_timeout: connection.connection_timeout,
        }),
        _ => None,
    }
}

fn git_change_summary_from_repository(repository: &Repository) -> GitChangeSummary {
    let mut changed_files = 0;
    let mut staged_files = 0;
    let mut untracked_files = 0;
    let mut added_lines = 0;
    let mut deleted_lines = 0;

    for status_entry in repository.cached_status() {
        let pending_ops = repository.pending_ops_for_path(&status_entry.repo_path);
        if pending_ops.as_ref().is_some_and(|ops| {
            ops.ops
                .iter()
                .any(|op| op.git_status == pending_op::GitStatus::Reverted && op.finished())
        }) {
            continue;
        }

        changed_files += 1;
        if status_entry.status.is_untracked() {
            untracked_files += 1;
        }

        let pending_stage_state = pending_ops
            .as_ref()
            .map(|ops| ops.staging() || ops.staged());
        if has_staged_changes(status_entry.status, pending_stage_state) {
            staged_files += 1;
        }

        if let Some(diff_stat) = status_entry.diff_stat {
            added_lines += diff_stat.added as usize;
            deleted_lines += diff_stat.deleted as usize;
        }
    }

    let tracking_status = repository
        .branch
        .as_ref()
        .and_then(|branch| branch.tracking_status());

    GitChangeSummary {
        changed_files,
        staged_files,
        untracked_files,
        added_lines,
        deleted_lines,
        ahead_commits: tracking_status.map_or(0, |status| status.ahead as usize),
        behind_commits: tracking_status.map_or(0, |status| status.behind as usize),
    }
}

fn has_staged_changes(status: git::status::FileStatus, pending_stage_state: Option<bool>) -> bool {
    pending_stage_state.unwrap_or_else(|| status.staging().has_staged())
}

fn remote_path_basename(path: &str) -> String {
    path.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .find(|segment| !segment.is_empty())
        .unwrap_or("Project")
        .to_string()
}

fn local_path_basename(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("Project")
        .to_string()
}

fn build_synced_local_workspace_entry(
    project: &ProjectEntry,
    discovered_worktree: &superzent_git::DiscoveredWorktree,
    store: &SuperzentStore,
) -> Option<WorkspaceEntry> {
    let repo_root = project.local_repo_root()?;
    let workspace_location = WorkspaceLocation::Local {
        worktree_path: discovered_worktree.path.clone(),
    };
    let existing_workspace = store.workspace_for_location(&workspace_location).cloned();
    let now = Utc::now();
    let kind = if discovered_worktree.path == repo_root {
        WorkspaceKind::Primary
    } else {
        WorkspaceKind::Worktree
    };

    Some(WorkspaceEntry {
        id: existing_workspace
            .as_ref()
            .map(|workspace| workspace.id.clone())
            .unwrap_or_else(|| Uuid::new_v4().to_string()),
        project_id: project.id.clone(),
        kind: kind.clone(),
        name: existing_workspace
            .as_ref()
            .map(|workspace| workspace.name.clone())
            .unwrap_or_else(|| match kind {
                WorkspaceKind::Primary => local_path_basename(&discovered_worktree.path),
                WorkspaceKind::Worktree => {
                    if discovered_worktree.git_status == WorkspaceGitStatus::Available {
                        discovered_worktree.branch.clone()
                    } else {
                        local_path_basename(&discovered_worktree.path)
                    }
                }
            }),
        display_name: existing_workspace
            .as_ref()
            .and_then(|workspace| workspace.display_name.clone()),
        branch: discovered_worktree.branch.clone(),
        location: workspace_location,
        agent_preset_id: existing_workspace
            .as_ref()
            .map(|workspace| workspace.agent_preset_id.clone())
            .unwrap_or_else(|| store.default_preset().id.clone()),
        managed: if kind == WorkspaceKind::Primary {
            false
        } else {
            existing_workspace
                .as_ref()
                .map(|workspace| workspace.managed)
                .unwrap_or(true)
        },
        git_status: discovered_worktree.git_status,
        git_summary: if discovered_worktree.git_status == WorkspaceGitStatus::Available {
            discovered_worktree.git_summary.clone().or_else(|| {
                existing_workspace
                    .as_ref()
                    .and_then(|workspace| workspace.git_summary.clone())
            })
        } else {
            None
        },
        attention_status: existing_workspace
            .as_ref()
            .map(|workspace| workspace.attention_status.clone())
            .unwrap_or(WorkspaceAttentionStatus::Idle),
        review_pending: existing_workspace
            .as_ref()
            .is_some_and(|workspace| workspace.review_pending),
        last_attention_reason: existing_workspace
            .as_ref()
            .and_then(|workspace| workspace.last_attention_reason.clone()),
        teardown_script_override: existing_workspace
            .as_ref()
            .and_then(|workspace| workspace.teardown_script_override.clone()),
        created_at: existing_workspace
            .as_ref()
            .map(|workspace| workspace.created_at)
            .unwrap_or(now),
        last_opened_at: existing_workspace
            .as_ref()
            .map(|workspace| workspace.last_opened_at)
            .unwrap_or(now),
    })
}

fn project_worktree_sync_message(
    project_name: &str,
    added_count: usize,
    removed_count: usize,
    refreshed_count: usize,
) -> String {
    if added_count == 0 && removed_count == 0 {
        return format!("Worktrees already in sync for {project_name}.");
    }

    let mut changes = Vec::new();
    if added_count > 0 {
        changes.push(format!("{added_count} added"));
    }
    if removed_count > 0 {
        changes.push(format!("{removed_count} removed"));
    }
    if refreshed_count > 0 {
        changes.push(format!("{refreshed_count} refreshed"));
    }

    format!(
        "Synced worktrees for {project_name}: {}.",
        changes.join(", ")
    )
}

fn build_local_workspace_bundle(
    workspace: &Entity<Workspace>,
    store: &SuperzentStore,
    cx: &App,
) -> Option<(ProjectEntry, WorkspaceEntry)> {
    let workspace_location = match single_workspace_location_snapshot(workspace, cx)? {
        WorkspaceLocation::Local { worktree_path } => WorkspaceLocation::Local { worktree_path },
        WorkspaceLocation::Ssh { .. } => return None,
    };

    let WorkspaceLocation::Local { worktree_path } = &workspace_location else {
        return None;
    };
    if !worktree_path.exists() {
        return None;
    }

    let project_handle = workspace.read(cx).project();
    let project = project_handle.read(cx);
    let active_repository = project
        .active_repository(cx)
        .or_else(|| project.repositories(cx).values().next().cloned());

    let (project_root, branch, git_status, git_summary) =
        if let Some(repository) = active_repository {
            let repository = repository.read(cx);
            (
                repository.original_repo_abs_path.to_path_buf(),
                repository_branch_display_name(&repository),
                WorkspaceGitStatus::Available,
                Some(git_change_summary_from_repository(&repository)),
            )
        } else {
            (
                worktree_path.clone(),
                superzent_git::NO_GIT_BRANCH_LABEL.to_string(),
                WorkspaceGitStatus::Unavailable,
                None,
            )
        };

    let project_location = ProjectLocation::Local {
        repo_root: project_root.clone(),
    };
    let existing_workspace = store.workspace_for_location(&workspace_location).cloned();
    let existing_project = store
        .project_for_workspace_sync(existing_workspace.as_ref(), &project_location)
        .cloned();
    let now = Utc::now();
    let project_id = existing_project
        .as_ref()
        .map(|project| project.id.clone())
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    let project_entry = ProjectEntry {
        id: project_id.clone(),
        name: existing_project
            .as_ref()
            .map(|project| project.name.clone())
            .unwrap_or_else(|| local_path_basename(&project_root)),
        location: project_location,
        collapsed: existing_project
            .as_ref()
            .is_some_and(|project| project.collapsed),
        created_at: existing_project
            .as_ref()
            .map(|project| project.created_at)
            .unwrap_or(now),
        last_opened_at: now,
    };

    let kind = if let Some(existing_workspace) = &existing_workspace {
        existing_workspace.kind.clone()
    } else if existing_project.is_none()
        || (worktree_path == &project_root
            && store.primary_workspace_for_project(&project_id).is_none())
    {
        WorkspaceKind::Primary
    } else {
        WorkspaceKind::Worktree
    };

    let workspace_entry = WorkspaceEntry {
        id: existing_workspace
            .as_ref()
            .map(|workspace| workspace.id.clone())
            .unwrap_or_else(|| Uuid::new_v4().to_string()),
        project_id,
        kind: kind.clone(),
        name: existing_workspace
            .as_ref()
            .map(|workspace| workspace.name.clone())
            .unwrap_or_else(|| match kind {
                WorkspaceKind::Primary => local_path_basename(worktree_path),
                WorkspaceKind::Worktree => {
                    if git_status == WorkspaceGitStatus::Available {
                        branch.clone()
                    } else {
                        local_path_basename(worktree_path)
                    }
                }
            }),
        display_name: existing_workspace
            .as_ref()
            .and_then(|workspace| workspace.display_name.clone()),
        branch,
        location: workspace_location,
        agent_preset_id: existing_workspace
            .as_ref()
            .map(|workspace| workspace.agent_preset_id.clone())
            .unwrap_or_else(|| store.default_preset().id.clone()),
        managed: existing_workspace
            .as_ref()
            .is_some_and(|workspace| workspace.managed),
        git_status,
        git_summary: if git_status == WorkspaceGitStatus::Available {
            git_summary.or_else(|| {
                existing_workspace
                    .as_ref()
                    .and_then(|workspace| workspace.git_summary.clone())
            })
        } else {
            None
        },
        attention_status: existing_workspace
            .as_ref()
            .map(|workspace| workspace.attention_status.clone())
            .unwrap_or(WorkspaceAttentionStatus::Idle),
        review_pending: existing_workspace
            .as_ref()
            .is_some_and(|workspace| workspace.review_pending),
        last_attention_reason: existing_workspace
            .as_ref()
            .and_then(|workspace| workspace.last_attention_reason.clone()),
        teardown_script_override: existing_workspace
            .as_ref()
            .and_then(|workspace| workspace.teardown_script_override.clone()),
        created_at: existing_workspace
            .as_ref()
            .map(|workspace| workspace.created_at)
            .unwrap_or(now),
        last_opened_at: now,
    };

    Some((project_entry, workspace_entry))
}

fn build_remote_workspace_bundle(
    workspace: &Entity<Workspace>,
    store: &SuperzentStore,
    cx: &App,
) -> Option<(ProjectEntry, WorkspaceEntry)> {
    let workspace_location = match single_workspace_location_snapshot(workspace, cx)? {
        WorkspaceLocation::Ssh {
            connection,
            worktree_path,
        } => WorkspaceLocation::Ssh {
            connection,
            worktree_path,
        },
        WorkspaceLocation::Local { .. } => return None,
    };

    let WorkspaceLocation::Ssh {
        connection,
        worktree_path,
    } = &workspace_location
    else {
        return None;
    };

    let project_handle = workspace.read(cx).project();
    let project = project_handle.read(cx);
    let active_repository = project
        .active_repository(cx)
        .or_else(|| project.repositories(cx).values().next().cloned());

    let (repo_root, branch, git_status, git_summary) = if let Some(repository) = active_repository {
        let repository = repository.read(cx);
        let repo_root = repository
            .original_repo_abs_path
            .to_string_lossy()
            .into_owned();
        let branch = repository_branch_display_name(&repository);
        (
            repo_root,
            branch,
            WorkspaceGitStatus::Available,
            Some(git_change_summary_from_repository(&repository)),
        )
    } else {
        (
            worktree_path.clone(),
            superzent_git::NO_GIT_BRANCH_LABEL.to_string(),
            WorkspaceGitStatus::Unavailable,
            None,
        )
    };

    let project_location = ProjectLocation::Ssh {
        connection: connection.clone(),
        repo_root: repo_root.clone(),
    };
    let existing_workspace = store.workspace_for_location(&workspace_location).cloned();
    let existing_project = store
        .project_for_workspace_sync(existing_workspace.as_ref(), &project_location)
        .cloned();
    let now = Utc::now();
    let project_id = existing_project
        .as_ref()
        .map(|project| project.id.clone())
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    let project_entry = ProjectEntry {
        id: project_id.clone(),
        name: existing_project
            .as_ref()
            .map(|project| project.name.clone())
            .unwrap_or_else(|| remote_path_basename(&repo_root)),
        location: project_location,
        collapsed: existing_project
            .as_ref()
            .is_some_and(|project| project.collapsed),
        created_at: existing_project
            .as_ref()
            .map(|project| project.created_at)
            .unwrap_or(now),
        last_opened_at: now,
    };

    let kind = if let Some(existing_workspace) = &existing_workspace {
        existing_workspace.kind.clone()
    } else if existing_project.is_none() {
        WorkspaceKind::Primary
    } else {
        WorkspaceKind::Worktree
    };

    let workspace_entry = WorkspaceEntry {
        id: existing_workspace
            .as_ref()
            .map(|workspace| workspace.id.clone())
            .unwrap_or_else(|| Uuid::new_v4().to_string()),
        project_id,
        kind: kind.clone(),
        name: existing_workspace
            .as_ref()
            .map(|workspace| workspace.name.clone())
            .unwrap_or_else(|| match kind {
                WorkspaceKind::Primary => remote_path_basename(worktree_path),
                WorkspaceKind::Worktree => branch.clone(),
            }),
        display_name: existing_workspace
            .as_ref()
            .and_then(|workspace| workspace.display_name.clone()),
        branch,
        location: workspace_location,
        agent_preset_id: existing_workspace
            .as_ref()
            .map(|workspace| workspace.agent_preset_id.clone())
            .unwrap_or_else(|| store.default_preset().id.clone()),
        managed: existing_workspace
            .as_ref()
            .is_some_and(|workspace| workspace.managed),
        git_status,
        git_summary: if git_status == WorkspaceGitStatus::Available {
            git_summary.or_else(|| {
                existing_workspace
                    .as_ref()
                    .and_then(|workspace| workspace.git_summary.clone())
            })
        } else {
            None
        },
        attention_status: existing_workspace
            .as_ref()
            .map(|workspace| workspace.attention_status.clone())
            .unwrap_or(WorkspaceAttentionStatus::Idle),
        review_pending: existing_workspace
            .as_ref()
            .is_some_and(|workspace| workspace.review_pending),
        last_attention_reason: existing_workspace
            .as_ref()
            .and_then(|workspace| workspace.last_attention_reason.clone()),
        teardown_script_override: existing_workspace
            .as_ref()
            .and_then(|workspace| workspace.teardown_script_override.clone()),
        created_at: existing_workspace
            .as_ref()
            .map(|workspace| workspace.created_at)
            .unwrap_or(now),
        last_opened_at: now,
    };

    Some((project_entry, workspace_entry))
}

fn attention_priority(status: &WorkspaceAttentionStatus) -> u8 {
    match status {
        WorkspaceAttentionStatus::Idle => 0,
        WorkspaceAttentionStatus::Review => 1,
        WorkspaceAttentionStatus::Working => 2,
        WorkspaceAttentionStatus::Permission => 3,
    }
}

fn next_terminal_input_attention_status(
    current_live_status: Option<&WorkspaceAttentionStatus>,
) -> Option<WorkspaceAttentionStatus> {
    match current_live_status {
        Some(WorkspaceAttentionStatus::Permission) => None,
        Some(_) => Some(WorkspaceAttentionStatus::Working),
        None => None,
    }
}

fn workspace_attention_for_terminal_status(
    status: &TaskStatus,
) -> Option<(WorkspaceAttentionStatus, bool)> {
    match status {
        TaskStatus::Running => Some((WorkspaceAttentionStatus::Working, false)),
        TaskStatus::Completed | TaskStatus::Failed | TaskStatus::NeedsAttention => {
            Some((WorkspaceAttentionStatus::Review, true))
        }
        TaskStatus::Idle | TaskStatus::Starting => None,
    }
}

fn workspace_attention_reason_for_terminal_status(
    status: &TaskStatus,
    reason: Option<String>,
) -> Option<String> {
    match status {
        TaskStatus::Completed => Some("Agent task completed".to_string()),
        TaskStatus::Failed | TaskStatus::NeedsAttention => reason,
        TaskStatus::Idle | TaskStatus::Starting | TaskStatus::Running => None,
    }
}

fn workspace_id_for_terminal_unregister(
    live_attention: Option<&LiveTerminalAttention>,
    tracked_workspace_id: Option<&str>,
) -> Option<String> {
    live_attention
        .map(|attention| attention.workspace_id.clone())
        .or_else(|| tracked_workspace_id.map(ToOwned::to_owned))
}

fn workspace_row_status_kind(
    workspace: &WorkspaceEntry,
    is_open_in_current_window: bool,
) -> WorkspaceRowStatusKind {
    if !is_open_in_current_window {
        return WorkspaceRowStatusKind::Hidden;
    }

    if let Some(setup_state) = workspace_setup_row_state(workspace) {
        return match setup_state {
            WorkspaceSetupRowState::InProgress => WorkspaceRowStatusKind::SetupInProgress,
        };
    }

    if workspace_git_status_visual_summary(workspace).is_some() {
        WorkspaceRowStatusKind::GitChanges
    } else {
        WorkspaceRowStatusKind::Open
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WorkspaceRowStatusKind {
    Hidden,
    SetupInProgress,
    Open,
    GitChanges,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WorkspaceSetupRowState {
    InProgress,
}

fn render_workspace_open_pill(cx: &mut Context<SuperzentSidebar>) -> gpui::AnyElement {
    Chip::new("Open")
        .label_color(Color::Muted)
        .bg_color(cx.theme().colors().element_background)
        .border_color(cx.theme().colors().border_variant)
        .into_any_element()
}

fn render_workspace_setup_status_pill(
    label: &'static str,
    color: Color,
    tooltip: String,
    cx: &mut Context<SuperzentSidebar>,
) -> gpui::AnyElement {
    Chip::new(label)
        .label_color(color)
        .bg_color(cx.theme().colors().element_background)
        .border_color(cx.theme().colors().border_variant)
        .tooltip(move |window, cx| ui::Tooltip::text(tooltip.clone())(window, cx))
        .into_any_element()
}

fn render_workspace_attention_indicator(
    workspace_id: &str,
    attention_status: &WorkspaceAttentionStatus,
    _cx: &mut Context<SuperzentSidebar>,
) -> AnyElement {
    match attention_status {
        WorkspaceAttentionStatus::Idle => div()
            .w_3()
            .items_center()
            .justify_center()
            .opacity(0.)
            .child(Indicator::dot().color(Color::Muted))
            .into_any_element(),
        WorkspaceAttentionStatus::Review => div()
            .w_3()
            .items_center()
            .justify_center()
            .child(Indicator::dot().color(Color::Success))
            .into_any_element(),
        WorkspaceAttentionStatus::Working => div()
            .w_3()
            .items_center()
            .justify_center()
            .child(Indicator::dot().color(Color::Warning))
            .with_animation(
                gpui::ElementId::from(SharedString::from(format!(
                    "superzent-working-indicator-{workspace_id}"
                ))),
                Animation::new(Duration::from_millis(900)).repeat(),
                |indicator: gpui::Div, delta: f32| {
                    let alpha = 0.35 + (delta * std::f32::consts::PI).sin().abs() * 0.65;
                    indicator.opacity(alpha)
                },
            )
            .into_any_element(),
        WorkspaceAttentionStatus::Permission => div()
            .w_3()
            .items_center()
            .justify_center()
            .child(Indicator::dot().color(Color::Error))
            .with_animation(
                gpui::ElementId::from(SharedString::from(format!(
                    "superzent-permission-indicator-{workspace_id}"
                ))),
                Animation::new(Duration::from_millis(650)).repeat(),
                |indicator: gpui::Div, delta: f32| {
                    let alpha = 0.4 + (delta * std::f32::consts::PI).sin().abs() * 0.6;
                    indicator.opacity(alpha)
                },
            )
            .into_any_element(),
    }
}

fn should_show_terminal_notification(
    mode: TerminalAgentNotificationMode,
    workspace_id: &str,
    store: &Entity<SuperzentStore>,
    cx: &App,
) -> bool {
    let has_active_window = cx.active_window().is_some();
    let active_workspace_id = store.read(cx).active_workspace_id();
    should_show_terminal_notification_for_context(
        mode,
        has_active_window,
        active_workspace_id,
        workspace_id,
    )
}

fn should_show_terminal_notification_for_context(
    mode: TerminalAgentNotificationMode,
    has_active_window: bool,
    active_workspace_id: Option<&str>,
    workspace_id: &str,
) -> bool {
    match mode {
        TerminalAgentNotificationMode::Off => false,
        TerminalAgentNotificationMode::Always => true,
        TerminalAgentNotificationMode::AppBackground => !has_active_window,
        TerminalAgentNotificationMode::WorkspaceHidden => {
            !has_active_window || active_workspace_id != Some(workspace_id)
        }
    }
}

#[cfg(target_os = "macos")]
struct NativeTerminalNotificationChannel {
    sender: SmolSender<String>,
    receiver: Mutex<Option<SmolReceiver<String>>>,
}

#[cfg(target_os = "macos")]
fn native_terminal_notification_channel() -> &'static NativeTerminalNotificationChannel {
    static CHANNEL: OnceLock<NativeTerminalNotificationChannel> = OnceLock::new();

    CHANNEL.get_or_init(|| {
        let (sender, receiver) = smol::channel::unbounded();
        NativeTerminalNotificationChannel {
            sender,
            receiver: Mutex::new(Some(receiver)),
        }
    })
}

#[cfg(target_os = "macos")]
fn log_native_terminal_notifications_unavailable(message: &str) {
    static DID_LOG: OnceLock<()> = OnceLock::new();
    if DID_LOG.set(()).is_ok() {
        log::warn!("{message}");
    }
}

#[cfg(target_os = "macos")]
fn native_terminal_notification_center() -> Option<id> {
    let Some(notification_center_class) = Class::get("NSUserNotificationCenter") else {
        log_native_terminal_notifications_unavailable(
            "macOS native terminal notifications are unavailable: NSUserNotificationCenter class is missing",
        );
        return None;
    };

    let center: id = unsafe { msg_send![notification_center_class, defaultUserNotificationCenter] };
    if center.is_null() {
        log_native_terminal_notifications_unavailable(
            "macOS native terminal notifications are unavailable: defaultUserNotificationCenter returned nil",
        );
        return None;
    }

    Some(center)
}

#[cfg(target_os = "macos")]
fn native_terminal_notification_delegate_class() -> &'static Class {
    static DELEGATE_CLASS: OnceLock<&'static Class> = OnceLock::new();

    DELEGATE_CLASS.get_or_init(|| unsafe {
        if let Some(mut decl) =
            ClassDecl::new("SuperzentNotificationCenterDelegate", class!(NSObject))
        {
            decl.add_method(
                sel!(userNotificationCenter:didActivateNotification:),
                native_terminal_notification_did_activate as extern "C" fn(&Object, Sel, id, id),
            );
            decl.add_method(
                sel!(userNotificationCenter:shouldPresentNotification:),
                native_terminal_notification_should_present
                    as extern "C" fn(&Object, Sel, id, id) -> BOOL,
            );
            decl.register()
        } else if let Some(existing_class) = Class::get("SuperzentNotificationCenterDelegate") {
            existing_class
        } else {
            class!(NSObject)
        }
    })
}

#[cfg(target_os = "macos")]
fn install_native_terminal_notification_delegate() -> bool {
    static DELEGATE: OnceLock<Option<usize>> = OnceLock::new();

    let Some(delegate) = *DELEGATE.get_or_init(|| unsafe {
        let delegate: id = msg_send![native_terminal_notification_delegate_class(), new];
        if delegate.is_null() {
            log_native_terminal_notifications_unavailable(
                "macOS native terminal notifications are unavailable: failed to allocate notification delegate",
            );
            None
        } else {
            Some(delegate as usize)
        }
    }) else {
        return false;
    };

    let Some(center) = native_terminal_notification_center() else {
        return false;
    };

    unsafe {
        let _: () = msg_send![center, setDelegate: delegate as id];
    }

    true
}

#[cfg(target_os = "macos")]
fn take_native_terminal_notification_activation_receiver() -> Option<SmolReceiver<String>> {
    if !install_native_terminal_notification_delegate() {
        return None;
    }

    match native_terminal_notification_channel().receiver.lock() {
        Ok(mut receiver) => receiver.take(),
        Err(error) => {
            log::error!("failed to subscribe to macOS notification activations: {error}");
            None
        }
    }
}

#[cfg(target_os = "macos")]
unsafe fn ns_string_to_string(string: id) -> Option<String> {
    if string == nil {
        return None;
    }

    let bytes: *const c_char = unsafe { msg_send![string, UTF8String] };
    (!bytes.is_null()).then(|| unsafe { CStr::from_ptr(bytes).to_string_lossy().into_owned() })
}

#[cfg(target_os = "macos")]
extern "C" fn native_terminal_notification_did_activate(
    _this: &Object,
    _: Sel,
    center: id,
    notification: id,
) {
    unsafe {
        let activation_type: isize = msg_send![notification, activationType];
        if activation_type == 0 {
            return;
        }

        let identifier: id = msg_send![notification, identifier];
        let Some(workspace_id) = ns_string_to_string(identifier) else {
            return;
        };

        if let Err(error) = native_terminal_notification_channel()
            .sender
            .try_send(workspace_id)
        {
            log::error!("failed to forward macOS notification activation: {error}");
        }

        let _: () = msg_send![center, removeDeliveredNotification: notification];
    }
}

#[cfg(target_os = "macos")]
extern "C" fn native_terminal_notification_should_present(
    _this: &Object,
    _: Sel,
    _center: id,
    _notification: id,
) -> BOOL {
    YES
}

#[cfg(not(target_os = "macos"))]
fn take_native_terminal_notification_activation_receiver() -> Option<SmolReceiver<String>> {
    None
}

fn workspace_notification_title(workspace: &WorkspaceEntry) -> String {
    match workspace.kind {
        WorkspaceKind::Primary => workspace.name.clone(),
        WorkspaceKind::Worktree => workspace_sidebar_title(workspace),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WorkspaceGitStatusVisualSummary {
    changed_files: usize,
    untracked_files: usize,
    added_lines: usize,
    deleted_lines: usize,
    ahead_commits: usize,
    behind_commits: usize,
}

fn workspace_git_status_visual_summary(
    workspace: &WorkspaceEntry,
) -> Option<WorkspaceGitStatusVisualSummary> {
    if !workspace.has_git() {
        return None;
    }

    let summary = workspace.git_summary.as_ref()?;
    let has_sync = summary.ahead_commits > 0 || summary.behind_commits > 0;
    let has_diff = summary.added_lines > 0 || summary.deleted_lines > 0;
    let has_dirty_files = summary.changed_files > 0;

    if !has_sync && !has_diff && !has_dirty_files {
        return None;
    }

    Some(WorkspaceGitStatusVisualSummary {
        changed_files: summary.changed_files,
        untracked_files: summary.untracked_files,
        added_lines: summary.added_lines,
        deleted_lines: summary.deleted_lines,
        ahead_commits: summary.ahead_commits,
        behind_commits: summary.behind_commits,
    })
}

fn render_workspace_git_status_pill(
    workspace: &WorkspaceEntry,
    cx: &mut Context<SuperzentSidebar>,
) -> Option<gpui::AnyElement> {
    let summary = workspace_git_status_visual_summary(workspace)?;
    let has_sync = summary.ahead_commits > 0 || summary.behind_commits > 0;
    let has_diff = summary.added_lines > 0 || summary.deleted_lines > 0;
    let has_file_status = summary.changed_files > 0 && !has_diff;
    let is_untracked_only = summary.untracked_files > 0
        && summary.untracked_files == summary.changed_files
        && !has_diff;

    Some(
        h_flex()
            .gap_1()
            .items_center()
            .flex_none()
            .px_2()
            .py_0p5()
            .border_1()
            .rounded_md()
            .border_color(cx.theme().colors().border_variant)
            .bg(cx.theme().colors().element_background)
            .when(has_sync, |this| {
                this.child(
                    h_flex()
                        .gap_1()
                        .items_center()
                        .when(summary.behind_commits > 0, |this| {
                            this.child(
                                Label::new(format!("↓{}", summary.behind_commits))
                                    .size(LabelSize::XSmall)
                                    .color(Color::Warning),
                            )
                        })
                        .when(summary.ahead_commits > 0, |this| {
                            this.child(
                                Label::new(format!("↑{}", summary.ahead_commits))
                                    .size(LabelSize::XSmall)
                                    .color(Color::Success),
                            )
                        }),
                )
            })
            .when(has_sync && (has_diff || has_file_status), |this| {
                this.child(Label::new("·").size(LabelSize::XSmall).color(Color::Muted))
            })
            .when(has_diff, |this| {
                this.child(
                    h_flex()
                        .gap_1()
                        .items_center()
                        .when(summary.added_lines > 0, |this| {
                            this.child(
                                Label::new(format!("+{}", summary.added_lines))
                                    .size(LabelSize::XSmall)
                                    .color(Color::Success),
                            )
                        })
                        .when(summary.deleted_lines > 0, |this| {
                            this.child(
                                Label::new(format!("-{}", summary.deleted_lines))
                                    .size(LabelSize::XSmall)
                                    .color(Color::Error),
                            )
                        }),
                )
            })
            .when(has_file_status, |this| {
                this.child(
                    Label::new(if is_untracked_only {
                        format!("{} new", summary.untracked_files)
                    } else {
                        format!(
                            "{} file{}",
                            summary.changed_files,
                            if summary.changed_files == 1 { "" } else { "s" }
                        )
                    })
                    .size(LabelSize::XSmall)
                    .color(if is_untracked_only {
                        Color::Created
                    } else {
                        Color::Muted
                    }),
                )
            })
            .into_any_element(),
    )
}

fn workspace_branch_label(workspace: &WorkspaceEntry) -> String {
    if workspace.has_git() {
        workspace.branch.clone()
    } else {
        superzent_git::NO_GIT_BRANCH_LABEL.to_string()
    }
}

fn project_workspace_label(count: usize) -> String {
    match count {
        1 => "1 workspace".to_string(),
        _ => format!("{count} workspaces"),
    }
}

fn workspace_display_name(workspace: &WorkspaceEntry) -> String {
    workspace.display_name().to_string()
}

fn workspace_branch_subtitle(workspace: &WorkspaceEntry) -> Option<String> {
    if !workspace.has_git() {
        return None;
    }

    match workspace.kind {
        WorkspaceKind::Primary => Some(workspace_branch_label(workspace)),
        WorkspaceKind::Worktree if workspace_has_display_alias(workspace) => {
            Some(workspace_branch_label(workspace))
        }
        WorkspaceKind::Worktree => None,
    }
}

fn workspace_sidebar_title(workspace: &WorkspaceEntry) -> String {
    match workspace.kind {
        WorkspaceKind::Primary => workspace_display_name(workspace),
        WorkspaceKind::Worktree if workspace_has_display_alias(workspace) => {
            workspace_display_name(workspace)
        }
        WorkspaceKind::Worktree => workspace.branch.clone(),
    }
}

fn workspace_has_display_alias(workspace: &WorkspaceEntry) -> bool {
    workspace
        .display_name
        .as_deref()
        .is_some_and(|name| !name.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use git::status::StatusCode;
    use std::path::PathBuf;

    fn workspace_entry(kind: WorkspaceKind) -> WorkspaceEntry {
        WorkspaceEntry {
            id: "workspace".to_string(),
            project_id: "project".to_string(),
            kind,
            name: "workspace".to_string(),
            display_name: None,
            branch: "feature/visual-update".to_string(),
            location: WorkspaceLocation::Local {
                worktree_path: PathBuf::from("/tmp/workspace"),
            },
            agent_preset_id: "codex".to_string(),
            managed: false,
            git_status: WorkspaceGitStatus::Available,
            git_summary: None,
            attention_status: WorkspaceAttentionStatus::Idle,
            review_pending: false,
            last_attention_reason: None,
            teardown_script_override: None,
            created_at: Utc::now(),
            last_opened_at: Utc::now(),
        }
    }

    fn local_workspace_entry(id: &str, project_id: &str, worktree_path: &str) -> WorkspaceEntry {
        WorkspaceEntry {
            id: id.to_string(),
            project_id: project_id.to_string(),
            kind: WorkspaceKind::Worktree,
            name: id.to_string(),
            display_name: None,
            branch: id.to_string(),
            location: WorkspaceLocation::Local {
                worktree_path: PathBuf::from(worktree_path),
            },
            agent_preset_id: "codex".to_string(),
            managed: true,
            git_status: WorkspaceGitStatus::Available,
            git_summary: None,
            attention_status: WorkspaceAttentionStatus::Idle,
            review_pending: false,
            last_attention_reason: None,
            teardown_script_override: None,
            created_at: Utc::now(),
            last_opened_at: Utc::now(),
        }
    }

    fn ssh_connection() -> StoredSshConnection {
        StoredSshConnection {
            host: "example.com".to_string(),
            username: Some("developer".to_string()),
            port: Some(22),
            args: Vec::new(),
            nickname: None,
            upload_binary_over_ssh: false,
            port_forwards: Vec::new(),
            connection_timeout: None,
        }
    }

    fn ssh_workspace_entry(id: &str, project_id: &str, worktree_path: &str) -> WorkspaceEntry {
        WorkspaceEntry {
            id: id.to_string(),
            project_id: project_id.to_string(),
            kind: WorkspaceKind::Worktree,
            name: id.to_string(),
            display_name: None,
            branch: id.to_string(),
            location: WorkspaceLocation::Ssh {
                connection: ssh_connection(),
                worktree_path: worktree_path.to_string(),
            },
            agent_preset_id: "codex".to_string(),
            managed: true,
            git_status: WorkspaceGitStatus::Available,
            git_summary: None,
            attention_status: WorkspaceAttentionStatus::Idle,
            review_pending: false,
            last_attention_reason: None,
            teardown_script_override: None,
            created_at: Utc::now(),
            last_opened_at: Utc::now(),
        }
    }

    fn lifecycle_failure(
        phase: superzent_git::WorkspaceLifecyclePhase,
    ) -> superzent_git::WorkspaceLifecycleFailure {
        superzent_git::WorkspaceLifecycleFailure {
            phase,
            command: "echo failure".to_string(),
            exit_code: Some(1),
            stdout: "stdout".to_string(),
            stderr: "stderr".to_string(),
        }
    }

    #[test]
    fn move_changes_source_path_prefers_base_workspace_path() {
        let project = ProjectEntry {
            id: "project".to_string(),
            name: "project".to_string(),
            location: ProjectLocation::Local {
                repo_root: PathBuf::from("/tmp/repo"),
            },
            collapsed: false,
            created_at: Utc::now(),
            last_opened_at: Utc::now(),
        };

        assert_eq!(
            move_changes_source_path(Some(Path::new("/tmp/repo/worktrees/feature")), &project),
            Some(PathBuf::from("/tmp/repo/worktrees/feature"))
        );
    }

    #[test]
    fn move_changes_source_path_falls_back_to_project_root() {
        let project = ProjectEntry {
            id: "project".to_string(),
            name: "project".to_string(),
            location: ProjectLocation::Local {
                repo_root: PathBuf::from("/tmp/repo"),
            },
            collapsed: false,
            created_at: Utc::now(),
            last_opened_at: Utc::now(),
        };

        assert_eq!(
            move_changes_source_path(None, &project),
            Some(PathBuf::from("/tmp/repo"))
        );
    }

    #[test]
    fn new_workspace_modal_bootstrap_applies_base_branch_resolution_and_defaults() {
        let bootstrap = new_workspace_modal_bootstrap(
            Ok(superzent_git::WorkspaceBaseBranchResolution {
                effective_base_branch: "develop".to_string(),
                notice: Some("Using the current base workspace branch.".to_string()),
            }),
            superzent_git::WorkspaceLifecycleDefaults {
                setup_script: Some("echo setup".to_string()),
                teardown_script: Some("echo teardown".to_string()),
            },
        );

        assert_eq!(bootstrap.base_branch, Some("develop".to_string()));
        assert_eq!(
            bootstrap.base_branch_notice,
            Some("Using the current base workspace branch.".to_string())
        );
        assert_eq!(bootstrap.base_branch_error, None);
        assert_eq!(bootstrap.setup_script, Some("echo setup".to_string()));
        assert_eq!(bootstrap.teardown_script, Some("echo teardown".to_string()));
    }

    #[test]
    fn new_workspace_create_options_marks_only_teardown_for_repo_default_persistence() {
        let options = new_workspace_create_options(
            "feature/bootstrap".to_string(),
            Some("main".to_string()),
            Some(PathBuf::from("/tmp/repo")),
            Some("echo setup".to_string()),
            Some("echo teardown".to_string()),
            true,
            false,
        );

        assert_eq!(options.setup_script, Some("echo setup".to_string()));
        assert_eq!(options.teardown_script, Some("echo teardown".to_string()));
        assert!(options.save_teardown_script_as_repo_default);
        assert_eq!(options.base_branch_override, Some("main".to_string()));
    }

    #[test]
    fn delete_workspace_prompt_details_show_script_preview_when_teardown_will_run() {
        let workspace = local_workspace_entry("workspace", "project", "/tmp/workspace");
        let prompt_details = delete_workspace_prompt_details(
            &workspace,
            &superzent_git::WorkspaceDeleteResolution::RunTeardownScript {
                script: "echo teardown".to_string(),
            },
        );

        assert_eq!(prompt_details.title, "Delete workspace?");
        assert_eq!(prompt_details.confirm_label, "Delete");
        assert_eq!(
            prompt_details.confirm_decision,
            DeleteWorkspaceModalDecision::Delete
        );
        assert_eq!(
            prompt_details.teardown_script,
            Some("echo teardown".to_string())
        );
        assert!(prompt_details.failure_details.is_none());
    }

    #[test]
    fn delete_workspace_prompt_details_blocked_config_path_skips_teardown() {
        let workspace = local_workspace_entry("workspace", "project", "/tmp/workspace");
        let prompt_details = delete_workspace_prompt_details(
            &workspace,
            &superzent_git::WorkspaceDeleteResolution::BlockedByConfig(lifecycle_failure(
                superzent_git::WorkspaceLifecyclePhase::Teardown,
            )),
        );

        assert_eq!(prompt_details.title, "Delete blocked");
        assert_eq!(prompt_details.confirm_label, "Delete Anyway");
        assert_eq!(
            prompt_details.confirm_decision,
            DeleteWorkspaceModalDecision::DeleteAnyway
        );
        assert!(prompt_details.teardown_script.is_none());
        assert!(prompt_details.teardown_message.contains("skip"));
        assert!(prompt_details.failure_details.is_some());
    }

    #[test]
    fn terminal_input_only_restores_working_for_tracked_live_terminal() {
        assert_eq!(next_terminal_input_attention_status(None), None);
        assert_eq!(
            next_terminal_input_attention_status(Some(&WorkspaceAttentionStatus::Working)),
            Some(WorkspaceAttentionStatus::Working)
        );
    }

    #[test]
    fn workspace_lifecycle_failure_prompt_detail_includes_notice_and_logs() {
        let workspace = local_workspace_entry("workspace", "project", "/tmp/workspace");
        let detail = workspace_lifecycle_failure_prompt_detail(
            &workspace,
            &lifecycle_failure(superzent_git::WorkspaceLifecyclePhase::Setup),
            Some("Configured base branch `missing` was not found."),
            false,
        );

        assert!(detail.contains("workspace"));
        assert!(detail.contains("Configured base branch `missing` was not found."));
        assert!(detail.contains("Stdout:\nstdout"));
        assert!(detail.contains("Stderr:\nstderr"));
    }

    #[test]
    fn workspace_lifecycle_failure_prompt_detail_mentions_force_delete_when_requested() {
        let workspace = local_workspace_entry("workspace", "project", "/tmp/workspace");
        let detail = workspace_lifecycle_failure_prompt_detail(
            &workspace,
            &lifecycle_failure(superzent_git::WorkspaceLifecyclePhase::Teardown),
            None,
            true,
        );

        assert!(detail.contains("Delete Anyway"));
        assert!(detail.contains("teardown failure"));
    }

    #[test]
    fn terminal_input_preserves_permission_attention() {
        assert_eq!(
            next_terminal_input_attention_status(Some(&WorkspaceAttentionStatus::Permission)),
            None
        );
    }

    #[test]
    fn running_terminal_status_maps_to_working_without_review_pending() {
        assert_eq!(
            workspace_attention_for_terminal_status(&TaskStatus::Running),
            Some((WorkspaceAttentionStatus::Working, false))
        );
        assert_eq!(
            workspace_attention_reason_for_terminal_status(&TaskStatus::Running, None),
            None
        );
    }

    #[test]
    fn completed_terminal_status_maps_to_review_with_default_reason() {
        assert_eq!(
            workspace_attention_for_terminal_status(&TaskStatus::Completed),
            Some((WorkspaceAttentionStatus::Review, true))
        );
        assert_eq!(
            workspace_attention_reason_for_terminal_status(&TaskStatus::Completed, None),
            Some("Agent task completed".to_string())
        );
    }

    #[test]
    fn failed_terminal_status_preserves_failure_reason_for_review_attention() {
        let reason = Some("Codex exited with code 1.".to_string());

        assert_eq!(
            workspace_attention_for_terminal_status(&TaskStatus::Failed),
            Some((WorkspaceAttentionStatus::Review, true))
        );
        assert_eq!(
            workspace_attention_reason_for_terminal_status(&TaskStatus::Failed, reason.clone()),
            reason
        );
    }

    #[test]
    fn terminal_unregister_prefers_live_attention_workspace() {
        let live_attention = LiveTerminalAttention {
            workspace_id: "workspace-live".to_string(),
            status: WorkspaceAttentionStatus::Working,
        };

        assert_eq!(
            workspace_id_for_terminal_unregister(Some(&live_attention), Some("workspace-tracked")),
            Some("workspace-live".to_string())
        );
    }

    #[test]
    fn terminal_unregister_falls_back_to_tracked_workspace_when_live_attention_is_missing() {
        assert_eq!(
            workspace_id_for_terminal_unregister(None, Some("workspace-tracked")),
            Some("workspace-tracked".to_string())
        );
    }

    #[test]
    fn render_preset_command_line_preserves_verbatim_shell_commands() {
        let command = r#"codex -c model_reasoning_summary="detailed" -c model_supports_reasoning_summaries=true"#;

        assert_eq!(
            render_preset_command_line(command, &[], ShellKind::Posix),
            command
        );
    }

    #[test]
    fn render_preset_command_line_quotes_split_arguments() {
        let arguments = vec![
            "-c".to_string(),
            r#"model_reasoning_summary="detailed""#.to_string(),
        ];

        assert_eq!(
            render_preset_command_line("codex", &arguments, ShellKind::Posix),
            r#"codex -c 'model_reasoning_summary="detailed"'"#
        );
    }

    #[test]
    fn always_mode_shows_terminal_notifications_even_when_workspace_is_visible() {
        assert!(should_show_terminal_notification_for_context(
            TerminalAgentNotificationMode::Always,
            true,
            Some("workspace-1"),
            "workspace-1",
        ));
    }

    #[test]
    fn workspace_hidden_mode_preserves_existing_visibility_gate() {
        assert!(!should_show_terminal_notification_for_context(
            TerminalAgentNotificationMode::WorkspaceHidden,
            true,
            Some("workspace-1"),
            "workspace-1",
        ));
        assert!(should_show_terminal_notification_for_context(
            TerminalAgentNotificationMode::WorkspaceHidden,
            true,
            Some("workspace-2"),
            "workspace-1",
        ));
    }

    #[test]
    fn app_background_mode_depends_only_on_active_window_state() {
        assert!(!should_show_terminal_notification_for_context(
            TerminalAgentNotificationMode::AppBackground,
            true,
            Some("workspace-1"),
            "workspace-1",
        ));
        assert!(should_show_terminal_notification_for_context(
            TerminalAgentNotificationMode::AppBackground,
            false,
            Some("workspace-1"),
            "workspace-1",
        ));
    }

    #[test]
    fn staged_summary_counts_renamed_entries() {
        assert!(has_staged_changes(StatusCode::Renamed.index(), None));
        assert!(has_staged_changes(StatusCode::Copied.index(), None));
    }

    #[test]
    fn staged_summary_prefers_pending_stage_state() {
        assert!(has_staged_changes(
            git::status::FileStatus::Untracked,
            Some(true)
        ));
        assert!(!has_staged_changes(
            StatusCode::Modified.index(),
            Some(false)
        ));
    }

    #[test]
    fn workspace_branch_subtitle_only_shows_for_primary_and_aliased_worktrees() {
        let primary = workspace_entry(WorkspaceKind::Primary);
        let mut aliased_worktree = workspace_entry(WorkspaceKind::Worktree);
        aliased_worktree.display_name = Some("local".to_string());
        let worktree = workspace_entry(WorkspaceKind::Worktree);

        assert_eq!(
            workspace_branch_subtitle(&primary),
            Some("feature/visual-update".to_string())
        );
        assert_eq!(
            workspace_branch_subtitle(&aliased_worktree),
            Some("feature/visual-update".to_string())
        );
        assert_eq!(workspace_branch_subtitle(&worktree), None);
    }

    #[test]
    fn workspace_git_status_visual_summary_hides_empty_and_gitless_rows() {
        let mut workspace = workspace_entry(WorkspaceKind::Primary);
        workspace.git_summary = Some(GitChangeSummary::default());
        assert_eq!(workspace_git_status_visual_summary(&workspace), None);

        workspace.git_status = WorkspaceGitStatus::Unavailable;
        workspace.git_summary = Some(GitChangeSummary {
            added_lines: 4,
            deleted_lines: 1,
            ..GitChangeSummary::default()
        });
        assert_eq!(workspace_git_status_visual_summary(&workspace), None);
    }

    #[test]
    fn workspace_git_status_visual_summary_exposes_sync_and_diff_counts() {
        let mut workspace = workspace_entry(WorkspaceKind::Primary);
        workspace.git_summary = Some(GitChangeSummary {
            changed_files: 2,
            added_lines: 19,
            deleted_lines: 3,
            ahead_commits: 2,
            behind_commits: 1,
            ..GitChangeSummary::default()
        });

        assert_eq!(
            workspace_git_status_visual_summary(&workspace),
            Some(WorkspaceGitStatusVisualSummary {
                changed_files: 2,
                untracked_files: 0,
                added_lines: 19,
                deleted_lines: 3,
                ahead_commits: 2,
                behind_commits: 1,
            })
        );
    }

    #[test]
    fn workspace_git_status_visual_summary_preserves_untracked_only_dirty_state() {
        let mut workspace = workspace_entry(WorkspaceKind::Primary);
        workspace.git_summary = Some(GitChangeSummary {
            changed_files: 1,
            untracked_files: 1,
            ..GitChangeSummary::default()
        });

        assert_eq!(
            workspace_git_status_visual_summary(&workspace),
            Some(WorkspaceGitStatusVisualSummary {
                changed_files: 1,
                untracked_files: 1,
                added_lines: 0,
                deleted_lines: 0,
                ahead_commits: 0,
                behind_commits: 0,
            })
        );
    }

    #[test]
    fn workspace_row_status_kind_hides_closed_workspaces_even_with_cached_git_summary() {
        let mut workspace = workspace_entry(WorkspaceKind::Primary);
        workspace.git_summary = Some(GitChangeSummary {
            changed_files: 2,
            added_lines: 8,
            deleted_lines: 1,
            ..GitChangeSummary::default()
        });

        assert_eq!(
            workspace_row_status_kind(&workspace, false),
            WorkspaceRowStatusKind::Hidden
        );
    }

    #[test]
    fn workspace_row_status_kind_shows_open_for_open_workspace_without_visual_git_summary() {
        let mut workspace = workspace_entry(WorkspaceKind::Primary);
        workspace.git_status = WorkspaceGitStatus::Unavailable;

        assert_eq!(
            workspace_row_status_kind(&workspace, true),
            WorkspaceRowStatusKind::Open
        );
    }

    #[test]
    fn workspace_row_status_kind_shows_git_changes_for_open_workspace_with_changes() {
        let mut workspace = workspace_entry(WorkspaceKind::Primary);
        workspace.git_summary = Some(GitChangeSummary {
            changed_files: 3,
            added_lines: 12,
            deleted_lines: 4,
            ..GitChangeSummary::default()
        });

        assert_eq!(
            workspace_row_status_kind(&workspace, true),
            WorkspaceRowStatusKind::GitChanges
        );
    }

    #[test]
    fn workspace_row_status_kind_shows_setup_in_progress_before_open_pill() {
        let mut workspace = workspace_entry(WorkspaceKind::Primary);
        workspace.attention_status = WorkspaceAttentionStatus::Working;
        workspace.last_attention_reason = Some(WORKSPACE_SETUP_IN_PROGRESS_REASON.to_string());

        assert_eq!(
            workspace_row_status_kind(&workspace, true),
            WorkspaceRowStatusKind::SetupInProgress
        );
    }

    #[test]
    fn can_mark_workspace_setup_attention_only_when_idle_without_review_pending() {
        let workspace = workspace_entry(WorkspaceKind::Primary);
        assert!(can_mark_workspace_setup_attention(&workspace));

        let mut permission_workspace = workspace_entry(WorkspaceKind::Primary);
        permission_workspace.attention_status = WorkspaceAttentionStatus::Permission;
        assert!(!can_mark_workspace_setup_attention(&permission_workspace));

        let mut review_workspace = workspace_entry(WorkspaceKind::Primary);
        review_workspace.review_pending = true;
        assert!(!can_mark_workspace_setup_attention(&review_workspace));
    }

    #[test]
    fn detached_head_display_name_uses_short_sha() {
        assert_eq!(
            detached_head_display_name(Some("0123456789abcdef0123456789abcdef01234567")),
            "0123456"
        );
    }

    #[test]
    fn detached_head_display_name_falls_back_when_sha_missing() {
        assert_eq!(detached_head_display_name(None), "Detached");
    }

    #[test]
    fn workspace_entry_matches_candidate_locations_matches_secondary_local_root() {
        let workspace_entry =
            local_workspace_entry("workspace-b", "project", "/tmp/project/worktrees/b");
        let candidate_locations = vec![
            WorkspaceLocation::Local {
                worktree_path: PathBuf::from("/tmp/project"),
            },
            WorkspaceLocation::Local {
                worktree_path: PathBuf::from("/tmp/project/worktrees/b"),
            },
        ];

        assert!(workspace_entry_matches_candidate_locations(
            &workspace_entry,
            &candidate_locations
        ));
    }

    #[test]
    fn matched_workspace_id_for_candidate_locations_returns_none_when_multiroot_match_is_ambiguous()
    {
        let workspace_entries = vec![
            local_workspace_entry("workspace-a", "project", "/tmp/project"),
            local_workspace_entry("workspace-b", "project", "/tmp/project/worktrees/b"),
        ];
        let candidate_locations = vec![
            WorkspaceLocation::Local {
                worktree_path: PathBuf::from("/tmp/project"),
            },
            WorkspaceLocation::Local {
                worktree_path: PathBuf::from("/tmp/project/worktrees/b"),
            },
        ];

        assert_eq!(
            matched_workspace_id_for_candidate_locations(
                &candidate_locations,
                &workspace_entries,
                None
            ),
            None
        );
    }

    #[test]
    fn matched_workspace_id_for_candidate_locations_prefers_current_workspace_when_ambiguous() {
        let workspace_entries = vec![
            local_workspace_entry("workspace-a", "project", "/tmp/project"),
            local_workspace_entry("workspace-b", "project", "/tmp/project/worktrees/b"),
        ];
        let candidate_locations = vec![
            WorkspaceLocation::Local {
                worktree_path: PathBuf::from("/tmp/project"),
            },
            WorkspaceLocation::Local {
                worktree_path: PathBuf::from("/tmp/project/worktrees/b"),
            },
        ];

        assert_eq!(
            matched_workspace_id_for_candidate_locations(
                &candidate_locations,
                &workspace_entries,
                Some("workspace-b")
            ),
            Some("workspace-b".to_string())
        );
    }

    #[test]
    fn matched_workspace_id_for_candidate_locations_matches_remote_worktree_candidates() {
        let workspace_entries = vec![ssh_workspace_entry(
            "workspace-remote",
            "project",
            "/repo/worktrees/feature-a",
        )];
        let candidate_locations = vec![
            WorkspaceLocation::Ssh {
                connection: ssh_connection(),
                worktree_path: "/repo/main".to_string(),
            },
            WorkspaceLocation::Ssh {
                connection: ssh_connection(),
                worktree_path: "/repo/worktrees/feature-a".to_string(),
            },
        ];

        assert_eq!(
            matched_workspace_id_for_candidate_locations(
                &candidate_locations,
                &workspace_entries,
                None
            ),
            Some("workspace-remote".to_string())
        );
    }
}
