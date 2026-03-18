#[cfg(feature = "acp_tabs")]
mod acp_tabs;
#[cfg(feature = "acp_tabs")]
pub use acp_tabs::{FocusAcpTab, NewAcpTab, OpenAcpHistory};

#[cfg(feature = "acp_tabs")]
use agent_ui::{
    AgentNotification, AgentNotificationEvent, open_external_acp_tab, pane_has_external_acp_item,
};
use anyhow::Result;
use chrono::Utc;
#[cfg(target_os = "macos")]
use cocoa::{
    base::{id, nil},
    foundation::{NSAutoreleasePool, NSString},
};
use editor::{Editor, EditorEvent, actions::SelectAll};
use git::repository::validate_worktree_directory;
use git_ui::git_panel::GitPanel;
use gpui::{
    Action, Animation, AnimationExt, App, AsyncWindowContext, ClickEvent, DismissEvent, Entity,
    EntityId, EventEmitter, FocusHandle, Focusable, MouseButton, MouseDownEvent, PathPromptOptions,
    Point, PromptLevel, SharedString, Subscription, Task, WeakEntity, WindowHandle, actions,
    anchored, deferred, px,
};
use menu;
#[cfg(target_os = "macos")]
use objc::{
    class,
    declare::ClassDecl,
    msg_send,
    runtime::{BOOL, Class, Object, Sel, YES},
    sel, sel_impl,
};
#[cfg(feature = "acp_tabs")]
use project::AgentRegistryStore;
#[cfg(feature = "acp_tabs")]
use project::agent_server_store::{AllAgentServersSettings, CustomAgentServerSettings};
#[cfg(feature = "acp_tabs")]
use project::agent_server_store::{CLAUDE_AGENT_NAME, CODEX_NAME, GEMINI_NAME};
use project::git_store::{GitStoreEvent, Repository, RepositoryEvent, pending_op};
use project::project_settings::ProjectSettings;
use project_panel::ProjectPanel;
use recent_projects::open_remote_project;
use remote::{RemoteConnectionOptions, SshConnectionOptions};
use settings::Settings;
use smol::channel::Receiver as SmolReceiver;
#[cfg(target_os = "macos")]
use smol::channel::Sender as SmolSender;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
#[cfg(target_os = "macos")]
use std::{
    ffi::{CStr, c_char},
    sync::{Mutex, OnceLock},
};
use superzent_agent::{AGENT_TERMINAL_ID_ENV_VAR, AgentHookEvent, AgentHookEventType};
use superzent_model::{
    AgentPreset, GitChangeSummary, PresetLaunchMode, ProjectEntry, ProjectLocation,
    StoredSshConnection, StoredSshPortForward, SuperzentStore, TaskStatus,
    WorkspaceAttentionStatus, WorkspaceEntry, WorkspaceGitStatus, WorkspaceKind, WorkspaceLocation,
    aggregate_workspace_attention_status,
};
use task::{Shell, ShellKind};
use terminal::terminal_settings::{TerminalAgentNotificationMode, TerminalSettings};
use terminal_view::{TerminalView, terminal_panel::TerminalPanel};
#[cfg(feature = "acp_tabs")]
use ui::ContextMenuEntry;
use ui::{
    ButtonLike, Chip, ContextMenu, DropdownMenu, DropdownStyle, Icon, Indicator, ListItem, Tab,
    Tooltip, prelude::*,
};
use uuid::Uuid;
use workspace::{
    AppState as WorkspaceAppState, ModalView, MultiWorkspace, MultiWorkspaceEvent, OpenOptions,
    Pane, SerializedWorkspaceLocation, Sidebar as WorkspaceSidebar, SidebarEvent, Toast, Workspace,
    dock::{DockPosition, Panel, PanelEvent},
    notifications::NotificationId,
    workspace_windows_for_location,
};
#[cfg(feature = "acp_tabs")]
use zed_actions::AcpRegistry;
use zed_actions::{OpenRemote, OpenSettingsAt};

actions!(
    superzent,
    [
        AddProject,
        NewWorkspace,
        RevealChanges,
        OpenWorkspaceInNewWindow,
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
    live_terminal_attention: BTreeMap<String, LiveTerminalAttention>,
    #[cfg(feature = "acp_tabs")]
    notifications: Vec<WindowHandle<AgentNotification>>,
    #[cfg(feature = "acp_tabs")]
    notification_subscriptions: Vec<Subscription>,
    _hook_task: Task<Result<()>>,
    _notification_activation_task: Task<Result<()>>,
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
        cx: &mut Context<Self>,
    ) where
        T: 'static,
    {
        let entity_id = terminal.entity_id();
        self.terminal_ids_by_entity
            .insert(entity_id, terminal_id.clone());

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
        if let Some(live_attention) = self.live_terminal_attention.remove(terminal_id) {
            self.recompute_workspace_attention(&live_attention.workspace_id, cx);
        }
    }

    fn handle_hook_event(&mut self, event: AgentHookEvent, cx: &mut Context<Self>) {
        let Some((workspace_id, workspace_name)) = self
            .resolve_workspace_for_event(&event, cx)
            .map(|workspace| {
                (
                    workspace.id.clone(),
                    workspace_notification_title(&workspace),
                )
            })
        else {
            log::debug!("ignoring agent hook event without a matching workspace");
            return;
        };

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

                let review_pending = !self.workspace_is_visible(&workspace_id, cx);
                self.store.update(cx, |store, cx| {
                    store.set_workspace_attention(
                        &workspace_id,
                        WorkspaceAttentionStatus::Idle,
                        review_pending,
                        review_pending.then(|| "Agent task completed".to_string()),
                        cx,
                    );
                });
                self.recompute_workspace_attention(&workspace_id, cx);

                if review_pending {
                    self.maybe_show_terminal_notification(
                        TerminalLifecycleNotification::Completed,
                        &workspace_id,
                        &workspace_name,
                        cx,
                    );
                }
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

    fn workspace_is_visible(&self, workspace_id: &str, cx: &App) -> bool {
        cx.active_window().is_some()
            && self.store.read(cx).active_workspace_id() == Some(workspace_id)
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

        if let Some(target_window) = notification_window_for_workspace_entry(&workspace_entry, cx) {
            if let Err(error) = target_window.update(cx, |multi_workspace, window, cx| {
                let live_workspace = multi_workspace
                    .workspaces()
                    .iter()
                    .find(|workspace| workspace_matches_entry(workspace, &workspace_entry, cx))
                    .cloned();
                window.activate_window();
                if let Some(live_workspace) = live_workspace {
                    multi_workspace.activate(live_workspace, cx);
                }
            }) {
                log::error!("failed to activate workspace from notification: {error:#}");
            }
            return;
        }

        let Some(app_state) = WorkspaceAppState::try_global(cx).and_then(|state| state.upgrade())
        else {
            log::error!("failed to open workspace from notification: missing app state");
            return;
        };
        let Some(target_window) = fallback_notification_window(cx) else {
            log::error!("failed to open workspace from notification: no workspace window found");
            return;
        };

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
        if !should_show_native_notification(mode, workspace_id, &self.store, cx) {
            return;
        }

        if self.show_popup_notification(notification, workspace_id, workspace_name, cx) {
            return;
        }

        let title = notification.title().to_string();
        let body = format!("{workspace_name} in superzent");

        dispatch_native_terminal_notification(&title, &body, workspace_id);
    }

    #[cfg(feature = "acp_tabs")]
    fn show_popup_notification(
        &mut self,
        notification: TerminalLifecycleNotification,
        workspace_id: &str,
        workspace_name: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        self.dismiss_notifications(cx);

        let Some(screen) = cx
            .primary_display()
            .or_else(|| cx.displays().into_iter().next())
        else {
            return false;
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
                return false;
            }
        };

        let pop_up = match screen_window.entity(cx) {
            Ok(pop_up) => pop_up,
            Err(error) => {
                log::error!("failed to access terminal agent notification window: {error:#}");
                let _ = screen_window.update(cx, |_, window, _| {
                    window.remove_window();
                });
                return false;
            }
        };

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

        true
    }

    #[cfg(not(feature = "acp_tabs"))]
    fn show_popup_notification(
        &mut self,
        _notification: TerminalLifecycleNotification,
        _workspace_id: &str,
        _workspace_name: &str,
        _cx: &mut Context<Self>,
    ) -> bool {
        false
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

#[derive(Clone, Copy)]
enum TerminalLifecycleNotification {
    Completed,
    PermissionRequest,
}

impl TerminalLifecycleNotification {
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
            let Some(terminal_id) = terminal_view
                .terminal()
                .read(cx)
                .env_var(AGENT_TERMINAL_ID_ENV_VAR)
                .map(str::to_string)
            else {
                return;
            };

            let terminal = terminal_view.terminal().clone();
            attention_controller.update(cx, |controller, cx| {
                controller.register_terminal(terminal, terminal_id, cx);
            });
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
                .register_action(|workspace, _: &DeleteWorkspace, window, cx| {
                    run_delete_workspace(workspace, window, cx);
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
                    task_prompt,
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
            registry_store.agent(agent_name).is_none() || registry_store.fetch_error().is_some()
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
            }

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
            if let Some(error) = registry_store.fetch_error()
                && !registry_store.is_fetching()
                && registry_store.agent(agent_name).is_none()
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
    task_prompt: String,
    window: &mut Window,
    cx: &mut App,
) {
    let store = SuperzentStore::global(cx);
    let session = store.update(cx, |store, cx| {
        store.start_session(
            &workspace_entry.id,
            &preset,
            session_label_for_prompt(&preset, Some(task_prompt.as_str())),
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
                })
                .is_none()
                {
                    return Ok::<(), anyhow::Error>(());
                }
                show_workspace_toast_async(&workspace_handle, reason, cx);
                return Ok::<(), anyhow::Error>(());
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
                store.update_session_status(&session.id, status, reason.clone(), cx);
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
    warning: Option<String>,
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
            created_at: existing_workspace
                .as_ref()
                .map(|workspace| workspace.created_at)
                .unwrap_or(now),
            last_opened_at: now,
        }
    })?;

    Ok(WorkspaceCreationResult {
        workspace,
        warning: None,
    })
}

fn spawn_new_workspace_request(
    workspace_handle: Entity<Workspace>,
    app_state: Arc<WorkspaceAppState>,
    project: ProjectEntry,
    branch_name: String,
    window: &mut Window,
    cx: &mut App,
) {
    let store = SuperzentStore::global(cx);
    let preset_id = store.read(cx).default_preset().id.clone();
    window
        .spawn(cx, async move |cx| {
            let outcome = match &project.location {
                ProjectLocation::Local { .. } => {
                    cx.background_spawn({
                        let project = project.clone();
                        let branch_name = branch_name.clone();
                        let preset_id = preset_id.clone();
                        async move {
                            superzent_git::create_workspace(
                                &project,
                                &preset_id,
                                superzent_git::CreateWorkspaceOptions { branch_name },
                            )
                            .map(|outcome| WorkspaceCreationResult {
                                workspace: outcome.workspace,
                                warning: outcome.warning,
                            })
                        }
                    })
                    .await
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

            if let Some(target_workspace) =
                cx.update(|window, cx| workspace_for_entry_in_window(window, cx, &workspace_entry))?
            {
                cx.update(|window, cx| {
                    launch_workspace_preset(
                        target_workspace,
                        workspace_entry.clone(),
                        preset_id.clone(),
                        None,
                        window,
                        cx,
                    );
                    if let Some(warning) = outcome.warning.clone() {
                        show_workspace_toast(&workspace_handle, warning, cx);
                    }
                })?;
            } else {
                show_workspace_toast_async(
                    &workspace_handle,
                    "Workspace opened, but its window could not be resolved.",
                    cx,
                );
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
                                    .icon(IconName::FolderOpen)
                                    .icon_position(IconPosition::Start)
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
    last_error: Option<SharedString>,
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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let branch_name_editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("feature/my-branch", window, cx);
            editor
        });

        Self {
            workspace,
            project,
            branch_name_editor,
            last_error: None,
        }
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

        let app_state = workspace_handle.read(cx).app_state().clone();

        spawn_new_workspace_request(
            workspace_handle,
            app_state,
            self.project.clone(),
            branch_name,
            window,
            cx,
        );

        cx.emit(DismissEvent);
    }
}

impl Render for NewWorkspaceModal {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .key_context("SuperzentNewWorkspaceModal")
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::confirm))
            .elevation_3(cx)
            .w(px(480.))
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
                            .child(Label::new("Create Workspace").size(LabelSize::Large))
                            .child(
                                Label::new(format!(
                                    "Create a managed workspace for {}.",
                                    self.project.name
                                ))
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                            ),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .child(Label::new("Branch Name").size(LabelSize::Small))
                            .child(self.branch_name_editor.clone()),
                    )
                    .when_some(self.last_error.clone(), |this, error| {
                        this.child(Label::new(error).size(LabelSize::Small).color(Color::Error))
                    }),
            )
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .px_4()
                    .pb_4()
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
    context_menu: Option<(Entity<ContextMenu>, Point<Pixels>, Subscription)>,
    rename_workspace_id: Option<String>,
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
            context_menu: None,
            rename_workspace_id: None,
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
        let workspace_bundle = {
            let store = self.store.read(cx);
            build_local_workspace_bundle(&current_workspace, &store, cx)
                .or_else(|| build_remote_workspace_bundle(&current_workspace, &store, cx))
        };
        self.store.update(cx, |store, cx| {
            if let Some((project_entry, workspace_entry)) = workspace_bundle.clone() {
                store.upsert_project_bundle(project_entry, workspace_entry.clone(), cx);
                store.record_workspace_opened(&workspace_entry.id, cx);
                return;
            }

            let workspace_id = workspace_location_snapshot(&current_workspace, cx)
                .as_ref()
                .and_then(|location| store.workspace_for_location(location))
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
        let Some(location) = workspace_location_snapshot(workspace, cx) else {
            return;
        };
        let Some(workspace_id) = self
            .store
            .read(cx)
            .workspace_for_location(&location)
            .map(|workspace| workspace.id.clone())
        else {
            return;
        };
        let (branch, git_status, git_summary) =
            if let Some(repository) = active_repository_for_workspace(workspace, cx) {
                let repository = repository.read(cx);
                (
                    repository
                        .branch
                        .as_ref()
                        .map(|branch| branch.name().to_string())
                        .unwrap_or_else(|| "HEAD".to_string()),
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

    fn current_workspace_entity(&self, cx: &App) -> Option<Entity<Workspace>> {
        self.multi_workspace
            .upgrade()
            .map(|multi_workspace| multi_workspace.read(cx).workspace().clone())
    }

    fn is_renaming_workspace(&self, workspace_id: &str) -> bool {
        self.rename_workspace_id.as_deref() == Some(workspace_id)
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

        if self.rename_editor.is_some() {
            self.finish_workspace_rename(true, window, cx);
        }

        let Some(current_label) = self
            .store
            .read(cx)
            .workspace(workspace_id)
            .map(|workspace| workspace.display_name().to_string())
        else {
            return;
        };

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
                            this.finish_workspace_rename(true, window, cx);
                        }
                    });
                }
            }
        });

        self.rename_workspace_id = Some(workspace_id.to_string());
        self.rename_editor = Some(rename_editor.clone());
        self.rename_editor_subscription = Some(rename_editor_subscription);

        rename_editor.update(cx, |editor, cx| {
            editor.set_text(current_label, window, cx);
            editor.select_all(&SelectAll, window, cx);
            editor.focus_handle(cx).focus(window, cx);
        });
        cx.notify();
    }

    fn finish_workspace_rename(
        &mut self,
        save: bool,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let workspace_id = self.rename_workspace_id.take();
        let editor = self.rename_editor.take();
        self.rename_editor_subscription = None;

        if save
            && let (Some(workspace_id), Some(editor)) = (workspace_id.as_deref(), editor.as_ref())
        {
            let label = editor.read(cx).text(cx).trim().to_string();
            self.store.update(cx, |store, cx| {
                store.set_workspace_display_name(workspace_id, Some(label), cx);
            });
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

    fn deploy_workspace_context_menu(
        &mut self,
        position: Point<Pixels>,
        workspace: WorkspaceEntry,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let entity = cx.entity();
        let context_menu = ContextMenu::build(window, cx, move |menu, _, _| {
            menu.entry("Rename Workspace", None, {
                let entity = entity.clone();
                let workspace_id = workspace.id;
                move |window, cx| {
                    entity.update(cx, |this, cx| {
                        this.begin_workspace_rename(&workspace_id, window, cx);
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

    fn deploy_project_context_menu(
        &mut self,
        position: Point<Pixels>,
        project: ProjectEntry,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let entity = cx.entity();
        let context_menu = ContextMenu::build(window, cx, move |menu, _, _| {
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
                                    .child(
                                        Label::new(project.name.clone())
                                            .size(LabelSize::Small)
                                            .truncate(),
                                    )
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
        let attention_status = workspace.attention_status.clone();
        let workspace_for_open = workspace.clone();
        let workspace_for_delete = workspace.clone();
        let workspace_for_menu = workspace.clone();
        let dragged_workspace = DraggedWorkspaceRow {
            workspace_id: workspace.id.clone(),
            project_id: workspace.project_id.clone(),
            label: workspace_sidebar_title(workspace),
        };
        let metadata_chips = workspace_metadata_chips(workspace);
        let has_metadata = !metadata_chips.is_empty();

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
                            .when(workspace.managed, |this| {
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
                                            if let Some(current_workspace) =
                                                this.current_workspace_entity(cx)
                                            {
                                                run_delete_workspace_from_store(
                                                    current_workspace,
                                                    workspace_for_delete.clone(),
                                                    window,
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
                            .on_secondary_mouse_down(cx.listener(
                                move |this, event: &MouseDownEvent, window, cx| {
                                    this.deploy_workspace_context_menu(
                                        event.position,
                                        workspace_for_menu.clone(),
                                        window,
                                        cx,
                                    );
                                },
                            ))
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
                                    .when(has_metadata, |this| {
                                        this.gap_0p5()
                                            .child(
                                                h_flex()
                                                    .w_full()
                                                    .gap_1()
                                                    .items_center()
                                                    .child(
                                                        self.render_workspace_title(workspace, cx),
                                                    )
                                                    .child(div().flex_1()),
                                            )
                                            .child(
                                                h_flex()
                                                    .w_full()
                                                    .gap_0p5()
                                                    .flex_wrap()
                                                    .children(metadata_chips),
                                            )
                                    })
                                    .when(!has_metadata, |this| {
                                        this.justify_center().child(
                                            h_flex()
                                                .w_full()
                                                .gap_1()
                                                .items_center()
                                                .child(self.render_workspace_title(workspace, cx))
                                                .child(div().flex_1()),
                                        )
                                    }),
                            ),
                    ),
            )
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
                            this.finish_workspace_rename(true, window, cx);
                        }))
                        .on_action(cx.listener(move |this, _: &menu::Cancel, window, cx| {
                            this.finish_workspace_rename(false, window, cx);
                        })),
                )
                .into_any_element();
        }

        match workspace.kind {
            WorkspaceKind::Primary => Label::new(workspace_display_name(workspace))
                .size(LabelSize::Small)
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
                                repository
                                    .branch
                                    .as_ref()
                                    .map(|branch| branch.name().to_string())
                                    .unwrap_or_else(|| "HEAD".to_string()),
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
            EmptyPaneMode::Initial
        } else {
            EmptyPaneMode::Workspace
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
            .icon(icon)
            .icon_size(IconSize::Small)
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
            EmptyPaneMode::Initial => vec![self.action_button(
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
            )],
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
                    .child(v_flex().flex_1().px_2().pb_1().children(project_content))
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
                                    .icon(IconName::FolderOpen)
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
        subscriptions.push(cx.observe_in(&right_dock, window, {
            let workspace = workspace.clone();
            move |this, dock, window, cx| {
                this.restore_active_sidebar(workspace.clone(), dock, window, cx);
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
            .when_some(icon, |button, icon| button.icon(icon))
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
            .child(div().size_full().child(content))
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
    workspace.toggle_modal(window, cx, move |window, cx| {
        NewWorkspaceModal::new(workspace_handle.clone(), project, window, cx)
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
    run_delete_workspace_entry(workspace, workspace_entry, window, cx);
}

fn run_delete_workspace_entry(
    workspace: &mut Workspace,
    workspace_entry: WorkspaceEntry,
    window: &mut gpui::Window,
    cx: &mut Context<Workspace>,
) {
    let store = SuperzentStore::global(cx);
    if workspace_entry.kind == WorkspaceKind::Primary || !workspace_entry.managed {
        workspace.show_toast(
            Toast::new(
                NotificationId::unique::<SuperzentSidebar>(),
                "Primary workspaces cannot be deleted.",
            ),
            cx,
        );
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
        return;
    };
    let reopen_primary_in_current_window =
        current_window_matches_workspace_entry(window, &workspace_entry, cx);

    let prompt = window.prompt(
        PromptLevel::Warning,
        "Delete workspace?",
        Some(&format!(
            "Delete `{}` and remove its worktree at {}?",
            workspace_entry.name,
            workspace_entry.display_path()
        )),
        &["Cancel", "Delete"],
        cx,
    );
    let app_state = workspace.app_state().clone();

    cx.spawn_in(window, async move |this, cx| {
        if prompt.await != Ok(1) {
            return anyhow::Ok(());
        }

        let workspace_to_delete = workspace_entry.clone();
        let delete_result = match &project.location {
            ProjectLocation::Local { repo_root } => {
                cx.background_spawn({
                    let repo_root = repo_root.clone();
                    async move {
                        superzent_git::delete_workspace(&workspace_to_delete, &repo_root, false)
                    }
                })
                .await
            }
            ProjectLocation::Ssh { .. } => {
                let (project_workspace, _) = resolve_remote_project_workspace(
                    &project,
                    &store,
                    app_state.clone(),
                    false,
                    cx,
                )
                .await?;

                let repository = cx.update(|_, cx| {
                    active_repository_for_workspace(&project_workspace, cx)
                        .ok_or_else(|| anyhow::anyhow!("no active repository found"))
                })??;
                let target_path = PathBuf::from(
                    workspace_to_delete
                        .ssh_worktree_path()
                        .ok_or_else(|| anyhow::anyhow!("missing remote worktree path"))?,
                );
                let receiver = repository.update(cx, |repository, _| {
                    repository.remove_worktree(target_path, false)
                });
                receiver.await??;
                Ok(())
            }
        };

        this.update_in(cx, |workspace, window, cx| match delete_result {
            Ok(()) => {
                store.update(cx, |store, cx| {
                    store.remove_workspace(&workspace_entry.id, cx);
                });
                workspace.show_toast(
                    Toast::new(
                        NotificationId::unique::<SuperzentSidebar>(),
                        format!("Deleted {}", workspace_entry.name),
                    ),
                    cx,
                );
                if reopen_primary_in_current_window {
                    if let Some(primary_workspace) = store
                        .read(cx)
                        .primary_workspace_for_project(&project.id)
                        .cloned()
                    {
                        store.update(cx, |store, cx| {
                            store.record_workspace_opened(&primary_workspace.id, cx);
                        });
                        focus_existing_or_open_workspace_entry(
                            primary_workspace,
                            workspace.app_state().clone(),
                            window,
                            cx,
                        )
                        .detach_and_log_err(cx);
                    }
                }
            }
            Err(error) => {
                workspace.show_toast(
                    Toast::new(
                        NotificationId::unique::<SuperzentSidebar>(),
                        format!("Failed to remove workspace: {error}"),
                    ),
                    cx,
                );
            }
        })
        .ok();

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
    window: &mut gpui::Window,
    cx: &mut App,
) {
    workspace_handle.update(cx, |workspace, cx| {
        run_delete_workspace_entry(workspace, workspace_entry.clone(), window, cx);
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
                local_workspace_root_path(workspace, cx)
                    .filter(|workspace_path| *workspace_path == path)
                    .map(|_| index)
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

fn focus_existing_or_open_workspace_entry(
    workspace_entry: WorkspaceEntry,
    app_state: Arc<WorkspaceAppState>,
    window: &mut gpui::Window,
    cx: &mut App,
) -> Task<anyhow::Result<()>> {
    if let Some(target_window) = notification_window_for_workspace_entry(&workspace_entry, cx) {
        return cx.spawn({
            let workspace_entry = workspace_entry.clone();
            async move |cx| {
                target_window.update(cx, |multi_workspace, window, cx| {
                    let Some(live_workspace) = multi_workspace
                        .workspaces()
                        .iter()
                        .find(|workspace| workspace_matches_entry(workspace, &workspace_entry, cx))
                        .cloned()
                    else {
                        anyhow::bail!("failed to locate open workspace");
                    };

                    window.activate_window();
                    multi_workspace.activate(live_workspace, cx);
                    Ok(())
                })??;
                Ok(())
            }
        });
    }

    open_workspace_entry(workspace_entry, app_state, window, cx)
}

fn workspace_from_window(window: &gpui::Window, cx: &App) -> Option<Entity<Workspace>> {
    let multi_workspace = window.window_handle().downcast::<MultiWorkspace>()?;
    let multi_workspace = multi_workspace.read(cx).ok()?;
    Some(multi_workspace.workspace().clone())
}

fn local_workspace_root_path(workspace: &Entity<Workspace>, cx: &App) -> Option<PathBuf> {
    let project = workspace.read(cx).project();
    project.read(cx).visible_worktrees(cx).find_map(|worktree| {
        worktree
            .read(cx)
            .as_local()
            .map(|local| local.abs_path().to_path_buf())
    })
}

fn workspace_location_snapshot(
    workspace: &Entity<Workspace>,
    cx: &App,
) -> Option<WorkspaceLocation> {
    let root_path = workspace.read(cx).root_paths(cx).into_iter().next()?;
    let project = workspace.read(cx).project();
    let project = project.read(cx);

    if let Some(connection) = project.remote_connection_options(cx) {
        let connection = stored_ssh_connection_from_options(&connection)?;
        return Some(WorkspaceLocation::Ssh {
            connection,
            worktree_path: root_path.to_string_lossy().into_owned(),
        });
    }

    project.is_local().then_some(WorkspaceLocation::Local {
        worktree_path: root_path.as_ref().to_path_buf(),
    })
}

fn workspace_matches_entry(
    workspace: &Entity<Workspace>,
    workspace_entry: &WorkspaceEntry,
    cx: &App,
) -> bool {
    workspace_location_snapshot(workspace, cx).is_some_and(|location| {
        workspace_entry.matches_locator(&workspace_location_to_locator(&location))
    })
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
    ordered_multi_workspace_windows(cx)
        .into_iter()
        .find_map(|window| {
            window
                .read_with(cx, |multi_workspace, cx| {
                    multi_workspace
                        .workspaces()
                        .iter()
                        .find(|workspace| workspace_matches_entry(workspace, workspace_entry, cx))
                        .cloned()
                })
                .ok()
                .flatten()
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

    ordered_multi_workspace_windows(cx)
        .into_iter()
        .find_map(|window| {
            window
                .read_with(cx, |multi_workspace, cx| {
                    project_workspaces.iter().find_map(|workspace_entry| {
                        multi_workspace
                            .workspaces()
                            .iter()
                            .find(|workspace| {
                                workspace_matches_entry(workspace, workspace_entry, cx)
                            })
                            .cloned()
                    })
                })
                .ok()
                .flatten()
        })
}

fn current_window_matches_workspace_entry(
    window: &Window,
    workspace_entry: &WorkspaceEntry,
    cx: &App,
) -> bool {
    workspace_from_window(window, cx)
        .as_ref()
        .is_some_and(|workspace| workspace_matches_entry(workspace, workspace_entry, cx))
}

fn ordered_multi_workspace_windows(cx: &App) -> Vec<WindowHandle<MultiWorkspace>> {
    cx.window_stack()
        .unwrap_or_else(|| cx.windows())
        .into_iter()
        .filter_map(|window| window.downcast::<MultiWorkspace>())
        .collect()
}

fn notification_window_for_workspace_entry(
    workspace_entry: &WorkspaceEntry,
    cx: &App,
) -> Option<WindowHandle<MultiWorkspace>> {
    ordered_multi_workspace_windows(cx)
        .into_iter()
        .find(|window| {
            window
                .read_with(cx, |multi_workspace, cx| {
                    multi_workspace
                        .workspaces()
                        .iter()
                        .any(|workspace| workspace_matches_entry(workspace, workspace_entry, cx))
                })
                .unwrap_or(false)
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
    }

    GitChangeSummary {
        changed_files,
        staged_files,
        untracked_files,
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

fn build_local_workspace_bundle(
    workspace: &Entity<Workspace>,
    store: &SuperzentStore,
    cx: &App,
) -> Option<(ProjectEntry, WorkspaceEntry)> {
    let workspace_location = match workspace_location_snapshot(workspace, cx)? {
        WorkspaceLocation::Local { worktree_path } => WorkspaceLocation::Local { worktree_path },
        WorkspaceLocation::Ssh { .. } => return None,
    };

    let WorkspaceLocation::Local { worktree_path } = &workspace_location else {
        return None;
    };

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
                repository
                    .branch
                    .as_ref()
                    .map(|branch| branch.name().to_string())
                    .unwrap_or_else(|| "HEAD".to_string()),
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
    let workspace_location = match workspace_location_snapshot(workspace, cx)? {
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
        let branch = repository
            .branch
            .as_ref()
            .map(|branch| branch.name().to_string())
            .unwrap_or_else(|| "HEAD".to_string());
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

fn should_show_native_notification(
    mode: TerminalAgentNotificationMode,
    workspace_id: &str,
    store: &Entity<SuperzentStore>,
    cx: &App,
) -> bool {
    match mode {
        TerminalAgentNotificationMode::Off => false,
        TerminalAgentNotificationMode::Always => true,
        TerminalAgentNotificationMode::AppBackground => cx.active_window().is_none(),
        TerminalAgentNotificationMode::WorkspaceHidden => {
            cx.active_window().is_none()
                || store.read(cx).active_workspace_id() != Some(workspace_id)
        }
    }
}

#[cfg(target_os = "macos")]
#[link(name = "Foundation", kind = "framework")]
unsafe extern "C" {
    static NSUserNotificationDefaultSoundName: id;
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
fn dispatch_native_terminal_notification(title: &str, body: &str, workspace_id: &str) {
    if !install_native_terminal_notification_delegate() {
        return;
    }

    let Some(center) = native_terminal_notification_center() else {
        return;
    };

    unsafe {
        let pool = NSAutoreleasePool::new(nil);
        let notification: id = msg_send![class!(NSUserNotification), new];

        let _: () = msg_send![notification, setTitle: ns_string(title)];
        let _: () = msg_send![notification, setInformativeText: ns_string(body)];
        let _: () = msg_send![notification, setIdentifier: ns_string(workspace_id)];
        let _: () = msg_send![notification, setSoundName: NSUserNotificationDefaultSoundName];
        let _: () = msg_send![center, deliverNotification: notification];
        let _: () = msg_send![notification, release];
        let _: () = msg_send![pool, drain];
    }
}

#[cfg(target_os = "macos")]
/// Allow NSString::alloc use here because it sets autorelease
#[allow(clippy::disallowed_methods)]
unsafe fn ns_string(string: &str) -> id {
    unsafe { NSString::alloc(nil).init_str(string).autorelease() }
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
fn dispatch_native_terminal_notification(_title: &str, _body: &str, _workspace_id: &str) {}

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

fn workspace_metadata_chips(workspace: &WorkspaceEntry) -> Vec<gpui::AnyElement> {
    let mut chips = Vec::new();
    let show_branch_chip = workspace.is_primary() || workspace_has_display_alias(workspace);

    if show_branch_chip {
        let branch_label = workspace_branch_label(workspace);
        chips.push(
            Chip::new(branch_label.clone())
                .label_color(Color::Muted)
                .tooltip({
                    let branch = branch_label;
                    move |window, cx| ui::Tooltip::text(branch.clone())(window, cx)
                })
                .into_any_element(),
        );
    }

    if let Some(summary) = &workspace.git_summary {
        if summary.changed_files > 0 {
            chips.push(
                Chip::new(format!("{} files", summary.changed_files))
                    .label_color(Color::Muted)
                    .tooltip(|window, cx| ui::Tooltip::text("Changed files")(window, cx))
                    .into_any_element(),
            );
        }
        if summary.staged_files > 0 {
            chips.push(
                Chip::new(format!("{} staged", summary.staged_files))
                    .label_color(Color::Accent)
                    .tooltip(|window, cx| ui::Tooltip::text("Staged files")(window, cx))
                    .into_any_element(),
            );
        }
        if summary.untracked_files > 0 {
            chips.push(
                Chip::new(format!("{} new", summary.untracked_files))
                    .label_color(Color::Created)
                    .tooltip(|window, cx| ui::Tooltip::text("Untracked files")(window, cx))
                    .into_any_element(),
            );
        }
    }

    chips
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
}
