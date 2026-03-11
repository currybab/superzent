use anyhow::Result;
use chrono::Utc;
use editor::{Editor, EditorEvent, actions::SelectAll};
use git::repository::validate_worktree_directory;
use git_ui::git_panel::GitPanel;
use gpui::{
    Action, Animation, AnimationExt, AsyncWindowContext, ClickEvent, DismissEvent, Entity,
    EntityId, EventEmitter, FocusHandle, Focusable, MouseButton, MouseDownEvent, PathPromptOptions,
    Point, PromptLevel, Subscription, Task, WeakEntity, WindowHandle, actions, anchored, deferred,
};
use menu;
use project::git_store::Repository;
use project::project_settings::ProjectSettings;
use project_panel::ProjectPanel;
use recent_projects::open_remote_project;
use remote::{RemoteConnectionOptions, SshConnectionOptions};
use settings::Settings;
use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Duration};
use superzet_agent::{AGENT_TERMINAL_ID_ENV_VAR, AgentHookEvent, AgentHookEventType};
use superzet_model::{
    AgentPreset, GitChangeSummary, ProjectEntry, ProjectLocation, StoredSshConnection,
    StoredSshPortForward, SuperzetStore, TaskStatus, WorkspaceAttentionStatus, WorkspaceEntry,
    WorkspaceKind, WorkspaceLocation, aggregate_workspace_attention_status,
};
use terminal::terminal_settings::{TerminalAgentNotificationMode, TerminalSettings};
use terminal_view::{TerminalView, terminal_panel::TerminalPanel};
use ui::{
    ButtonLike, Chip, ContextMenu, DropdownMenu, DropdownStyle, Indicator, ListItem, Tab, Tooltip,
    prelude::*,
};
use uuid::Uuid;
use workspace::{
    AppState as WorkspaceAppState, ModalView, MultiWorkspace, MultiWorkspaceEvent, OpenOptions,
    OpenVisible, Pane, SerializedWorkspaceLocation, Sidebar as WorkspaceSidebar, SidebarEvent,
    Toast, Workspace,
    dock::{DockPosition, Panel, PanelEvent},
    notifications::NotificationId,
    workspace_windows_for_location,
};
use zed_actions::{OpenRemote, OpenSettingsAt};

actions!(
    superzet,
    [
        AddProject,
        NewWorkspace,
        RevealChanges,
        OpenWorkspaceInNewWindow,
        DeleteWorkspace,
        ToggleRightSidebar,
        CollapseWorkspaceSection,
        ExpandWorkspaceSection
    ]
);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RightSidebarTab {
    Changes,
    Files,
}

#[derive(Clone)]
struct LiveTerminalAttention {
    workspace_id: String,
    status: WorkspaceAttentionStatus,
}

struct WorkspaceAttentionController {
    store: Entity<SuperzetStore>,
    terminal_ids_by_entity: BTreeMap<EntityId, String>,
    live_terminal_attention: BTreeMap<String, LiveTerminalAttention>,
    _hook_task: Task<Result<()>>,
}

impl WorkspaceAttentionController {
    fn new(cx: &mut Context<Self>) -> Self {
        let store = SuperzetStore::global(cx);
        let hook_task = match superzet_agent::subscribe() {
            Ok(receiver) => cx.spawn(async move |this, cx| {
                while let Ok(event) = receiver.recv().await {
                    this.update(cx, |this, cx| {
                        this.handle_hook_event(event, cx);
                    })?;
                }
                Ok(())
            }),
            Err(error) => {
                log::error!("failed to subscribe to Superzet agent hooks: {error:#}");
                Task::ready(Ok(()))
            }
        };

        Self {
            store,
            terminal_ids_by_entity: BTreeMap::new(),
            live_terminal_attention: BTreeMap::new(),
            _hook_task: hook_task,
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
                self.maybe_show_native_notification(
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
                    self.maybe_show_native_notification(
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

    fn maybe_show_native_notification(
        &self,
        notification: TerminalLifecycleNotification,
        workspace_id: &str,
        workspace_name: &str,
        cx: &mut Context<Self>,
    ) {
        let mode = TerminalSettings::get_global(cx).agent_notifications;
        if !should_show_native_notification(mode, workspace_id, &self.store, cx) {
            return;
        }

        let title = notification.title().to_string();
        let body = format!("{workspace_name} in superzet");

        cx.background_spawn(async move {
            dispatch_native_terminal_notification(&title, &body);
        })
        .detach();
    }
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
}

pub fn init(cx: &mut App) {
    if SuperzetStore::try_global(cx).is_none() {
        return;
    }

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
            cx.new(|cx| SuperzetEmptyPaneView::new(pane_handle.downgrade(), pane_id, cx));
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
                })
                .register_action(|workspace, _: &ToggleRightSidebar, window, cx| {
                    if workspace.right_dock().read(cx).is_open() {
                        workspace.close_panel::<SuperzetRightSidebar>(window, cx);
                    } else {
                        workspace.open_panel::<SuperzetRightSidebar>(window, cx);
                    }
                });
        },
    )
    .detach();
}

pub fn install_pane_accessory(pane: &Entity<Pane>, cx: &mut Context<Workspace>) {
    let Some(store) = SuperzetStore::try_global(cx) else {
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
    let store = SuperzetStore::try_global(cx)?;
    let (workspace_entry, presets) = {
        let store = store.read(cx);
        let workspace_entry =
            if let Some(location) = workspace_location_snapshot(&workspace_handle, cx) {
                store.workspace_for_location(&location).cloned()
            } else {
                None
            }
            .or_else(|| store.active_workspace().cloned())?;

        (workspace_entry, store.presets().to_vec())
    };

    let (visible_presets, hidden_presets) =
        split_presets_for_width(&presets, estimated_preset_bar_width(window));
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
            .id(format!("superzet-preset-bar-{}", workspace_entry.id))
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
                    .child(
                        IconButton::new(
                            format!("superzet-preset-settings-{}", workspace_entry.id),
                            IconName::Settings,
                        )
                        .shape(ui::IconButtonShape::Square)
                        .style(ButtonStyle::Subtle)
                        .tooltip(|window, cx| Tooltip::text("Open agent presets")(window, cx))
                        .on_click(move |_, window, cx| {
                            open_agent_presets_settings(window, cx);
                        }),
                    ),
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
            "superzet-preset-button-{}-{}",
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
        format!("superzet-preset-overflow-{workspace_id}"),
        "More",
        menu,
    )
    .style(DropdownStyle::Ghost)
    .into_any_element()
}

fn estimated_preset_bar_width(window: &Window) -> Pixels {
    let width = (f32::from(window.viewport_size().width) * 0.5).clamp(180.0, 560.0);
    px(width)
}

fn split_presets_for_width(
    presets: &[AgentPreset],
    available_width: Pixels,
) -> (Vec<AgentPreset>, Vec<AgentPreset>) {
    let mut visible_presets = select_presets_for_width(presets, available_width, false);
    let mut hidden_presets = presets[visible_presets.len()..].to_vec();

    if !hidden_presets.is_empty() {
        visible_presets = select_presets_for_width(presets, available_width, true);
        hidden_presets = presets[visible_presets.len()..].to_vec();
    }

    (visible_presets, hidden_presets)
}

fn select_presets_for_width(
    presets: &[AgentPreset],
    available_width: Pixels,
    reserve_overflow: bool,
) -> Vec<AgentPreset> {
    let reserved_width = if reserve_overflow { 132.0 } else { 88.0 };
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

fn launch_workspace_preset(
    workspace_handle: Entity<Workspace>,
    workspace_entry: WorkspaceEntry,
    preset_id: String,
    task_prompt: Option<String>,
    window: &mut Window,
    cx: &mut App,
) {
    let store = SuperzetStore::global(cx);
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
        match superzet_agent::spawn_for_workspace(&workspace_entry, &session, &preset) {
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

            if let Some(task_prompt) = task_prompt
                && !task_prompt.trim().is_empty()
            {
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
            Toast::new(NotificationId::unique::<SuperzetSidebar>(), message),
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
    store: &Entity<SuperzetStore>,
    cx: &mut AsyncWindowContext,
    update: impl FnOnce(&mut SuperzetStore, &mut Context<SuperzetStore>) -> R,
) -> Option<R> {
    match cx.update(|_, cx| store.update(cx, update)) {
        Ok(result) => Some(result),
        Err(error) => {
            log::error!("failed to update Superzet store: {error:#}");
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
    store: &Entity<SuperzetStore>,
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

    let primary_live_workspace =
        cx.update(|window, cx| workspace_for_entry_in_window(window, cx, &primary_workspace))?;

    let project_live_workspace = cx.update(|window, cx| {
        store
            .read(cx)
            .workspaces_for_project(&project.id)
            .into_iter()
            .find_map(|workspace_entry| workspace_for_entry_in_window(window, cx, workspace_entry))
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
    store: Entity<SuperzetStore>,
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
    let store = SuperzetStore::global(cx);
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
                            superzet_git::create_workspace(
                                &project,
                                &preset_id,
                                superzet_git::CreateWorkspaceOptions { branch_name },
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
            .key_context("SuperzetAddProjectChooserModal")
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
                                    "Choose whether to add a local repository or connect to a remote SSH project.",
                                )
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                            ),
                    )
                    .child(
                        v_flex()
                            .gap_2()
                            .child(
                                Button::new("superzet-add-local-project", "Local Project")
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
                                ButtonLike::new("superzet-add-remote-project")
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
                        Button::new("superzet-add-project-cancel", "Cancel")
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
            .key_context("SuperzetNewWorkspaceModal")
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
                        Button::new("superzet-new-workspace-cancel", "Cancel")
                            .style(ButtonStyle::Subtle)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.cancel(&menu::Cancel, window, cx);
                            })),
                    )
                    .child(
                        Button::new("superzet-new-workspace-create", "Create")
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

pub struct SuperzetSidebar {
    store: Entity<SuperzetStore>,
    multi_workspace: WeakEntity<MultiWorkspace>,
    focus_handle: FocusHandle,
    width: Option<Pixels>,
    context_menu: Option<(Entity<ContextMenu>, Point<Pixels>, Subscription)>,
    rename_workspace_id: Option<String>,
    rename_editor: Option<Entity<Editor>>,
    rename_editor_subscription: Option<Subscription>,
    _subscriptions: Vec<Subscription>,
}

impl SuperzetSidebar {
    pub fn new(
        multi_workspace: Entity<MultiWorkspace>,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let store = SuperzetStore::global(cx);
        let weak_multi_workspace = multi_workspace.downgrade();
        let mut subscriptions = vec![cx.observe(&store, |_, _, cx| cx.notify())];
        subscriptions.push(
            cx.subscribe_in(
                &multi_workspace,
                window,
                |this, _, event, _, cx| match event {
                    MultiWorkspaceEvent::ActiveWorkspaceChanged
                    | MultiWorkspaceEvent::WorkspaceAdded(_)
                    | MultiWorkspaceEvent::WorkspaceRemoved(_) => {
                        this.sync_active_workspace(cx);
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
            _subscriptions: subscriptions,
        };
        this.sync_active_workspace(cx);
        this
    }

    fn sync_active_workspace(&mut self, cx: &mut Context<Self>) {
        let Some(current_workspace) = self.current_workspace_entity(cx) else {
            return;
        };
        let location = workspace_location_snapshot(&current_workspace, cx);
        let remote_bundle = {
            let store = self.store.read(cx);
            build_remote_workspace_bundle(&current_workspace, &store, cx)
        };
        self.store.update(cx, |store, cx| {
            if let Some((project_entry, workspace_entry)) = remote_bundle.clone() {
                store.upsert_project_bundle(project_entry, workspace_entry.clone(), cx);
                store.record_workspace_opened(&workspace_entry.id, cx);
                return;
            }

            let workspace_id = location
                .as_ref()
                .and_then(|location| store.workspace_for_location(location))
                .map(|workspace| workspace.id.clone());
            store.set_active_workspace(workspace_id, cx);
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
                                            this.store.update(cx, |store, cx| {
                                                store.set_active_workspace(
                                                    Some(workspace_for_delete.id.clone()),
                                                    cx,
                                                );
                                            });
                                            if let Some(current_workspace) =
                                                this.current_workspace_entity(cx)
                                            {
                                                run_delete_workspace_from_store(
                                                    current_workspace,
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
        match &workspace.location {
            WorkspaceLocation::Local { worktree_path } => {
                let worktree_path = worktree_path.clone();
                let workspace_id = workspace.id.clone();
                let store = self.store.clone();
                cx.spawn_in(window, async move |_, cx| {
                    let refresh = cx
                        .background_spawn(async move {
                            superzet_git::refresh_workspace_path(&worktree_path)
                        })
                        .await;

                    if let Ok(refresh) = refresh {
                        store.update(cx, |store, cx| {
                            store.refresh_workspace_metadata(
                                &workspace_id,
                                Some(refresh.branch),
                                refresh.git_summary,
                                cx,
                            );
                        });
                    }

                    anyhow::Ok(())
                })
                .detach_and_log_err(cx);
            }
            WorkspaceLocation::Ssh { .. } => {
                let branch_and_summary = workspace_for_entry_in_window(window, cx, &workspace)
                    .and_then(|live_workspace| {
                        active_repository_for_workspace(&live_workspace, cx).map(|repository| {
                            let repository = repository.read(cx);
                            let branch = repository
                                .branch
                                .as_ref()
                                .map(|branch| branch.name().to_string())
                                .unwrap_or_else(|| "HEAD".to_string());
                            (
                                branch,
                                Some(git_change_summary_from_repository(&repository)),
                            )
                        })
                    });

                if let Some((branch, git_summary)) = branch_and_summary {
                    self.store.update(cx, |store, cx| {
                        store.refresh_workspace_metadata(
                            &workspace.id,
                            Some(branch),
                            git_summary,
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

struct SuperzetEmptyPaneView {
    pane: WeakEntity<Pane>,
    pane_id: EntityId,
    store: Entity<SuperzetStore>,
    _subscriptions: Vec<Subscription>,
}

impl SuperzetEmptyPaneView {
    fn new(pane: WeakEntity<Pane>, pane_id: EntityId, cx: &mut Context<Self>) -> Self {
        let store = SuperzetStore::global(cx);
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

    fn current_workspace_entity(
        &self,
        window: &gpui::Window,
        cx: &App,
    ) -> Option<Entity<Workspace>> {
        workspace_from_window(window, cx)
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

impl Render for SuperzetEmptyPaneView {
    fn render(&mut self, _window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mode = self.mode(cx);
        let (title, subtitle) = match mode {
            EmptyPaneMode::Initial => ("No projects yet", "Add a repository to get started."),
            EmptyPaneMode::Workspace => ("This pane is empty", "Open something in this pane."),
        };

        let buttons = match mode {
            EmptyPaneMode::Initial => vec![self.action_button(
                "superzet-empty-add-project",
                "Add Project",
                IconName::OpenFolder,
                true,
                |this, window, cx| {
                    if let Some(current_workspace) = this.current_workspace_entity(window, cx) {
                        run_add_project_from_store(current_workspace, window, cx);
                    }
                },
                cx,
            )],
            EmptyPaneMode::Workspace => vec![
                self.action_button(
                    "superzet-empty-new-terminal",
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
                    "superzet-empty-reveal-changes",
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
                    "superzet-empty-search-files",
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

impl EventEmitter<SidebarEvent> for SuperzetSidebar {}

impl Focusable for SuperzetSidebar {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SuperzetSidebar {
    fn render(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        let projects = self.store.read(cx).projects().to_vec();
        let project_content = if projects.is_empty() {
            vec![
                v_flex()
                    .gap_1()
                    .py_4()
                    .child(Label::new("No repositories yet"))
                    .child(
                        Label::new("Add a local repository or connect to a remote SSH project.")
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
                                Button::new("superzet-sidebar-add-project", "Add Project")
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

impl WorkspaceSidebar for SuperzetSidebar {
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

pub struct SuperzetRightSidebar {
    project_panel: Entity<ProjectPanel>,
    git_panel: Entity<GitPanel>,
    focus_handle: FocusHandle,
    width: Option<Pixels>,
    _active: bool,
    tab: RightSidebarTab,
    _subscriptions: Vec<Subscription>,
}

impl SuperzetRightSidebar {
    pub fn load(
        workspace: WeakEntity<Workspace>,
        project_panel: Entity<ProjectPanel>,
        git_panel: Entity<GitPanel>,
        mut cx: AsyncWindowContext,
    ) -> Result<Entity<Self>> {
        workspace.update_in(&mut cx, |workspace, window, cx| {
            cx.new(|cx| Self::new(workspace, project_panel, git_panel, window, cx))
        })
    }

    fn new(
        _workspace: &Workspace,
        project_panel: Entity<ProjectPanel>,
        git_panel: Entity<GitPanel>,
        _window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let store = SuperzetStore::global(cx);
        Self {
            project_panel,
            git_panel,
            focus_handle: cx.focus_handle(),
            width: None,
            _active: false,
            tab: RightSidebarTab::Changes,
            _subscriptions: vec![cx.observe(&store, |_, _, cx| cx.notify())],
        }
    }

    fn set_active_tab(&mut self, tab: RightSidebarTab, cx: &mut Context<Self>) {
        self.tab = tab;
        cx.notify();
    }

    fn render_tab_button(
        &self,
        id: impl Into<gpui::ElementId>,
        label: &'static str,
        icon: IconName,
        tab: RightSidebarTab,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let active = self.tab == tab;
        let compact = self.width.unwrap_or_else(|| px(320.)) < px(250.);

        if compact {
            return IconButton::new(id, icon)
                .shape(ui::IconButtonShape::Square)
                .style(ui::ButtonStyle::Subtle)
                .toggle_state(active)
                .selected_style(ui::ButtonStyle::Filled)
                .tooltip(move |window, cx| ui::Tooltip::text(label)(window, cx))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.set_active_tab(tab, cx);
                }))
                .into_any_element();
        }

        Button::new(id, label)
            .icon(icon)
            .label_size(LabelSize::Small)
            .style(ui::ButtonStyle::Subtle)
            .toggle_state(active)
            .selected_style(ui::ButtonStyle::Filled)
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.set_active_tab(tab, cx);
            }))
            .into_any_element()
    }
}

impl EventEmitter<PanelEvent> for SuperzetRightSidebar {}

impl Focusable for SuperzetRightSidebar {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SuperzetRightSidebar {
    fn render(&mut self, _window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
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
                                "superzet-right-tab-changes",
                                "Changes",
                                IconName::GitBranchAlt,
                                RightSidebarTab::Changes,
                                cx,
                            ))
                            .child(self.render_tab_button(
                                "superzet-right-tab-files",
                                "Files",
                                IconName::FileTree,
                                RightSidebarTab::Files,
                                cx,
                            ))
                            .child(div().flex_1()),
                    ),
            )
            .child(div().size_full().child(match self.tab {
                RightSidebarTab::Changes => self.git_panel.clone().into_any_element(),
                RightSidebarTab::Files => self.project_panel.clone().into_any_element(),
            }))
    }
}

impl Panel for SuperzetRightSidebar {
    fn persistent_name() -> &'static str {
        "Superzet Right Sidebar"
    }

    fn panel_key() -> &'static str {
        "SuperzetRightSidebar"
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
        Box::new(ToggleRightSidebar)
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
    let store = SuperzetStore::global(cx);
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
                                NotificationId::unique::<SuperzetSidebar>(),
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
                async move { superzet_git::register_project(&path, &default_preset_id) },
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
                            NotificationId::unique::<SuperzetSidebar>(),
                            format!("Added {}", primary_workspace.name),
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
                        NotificationId::unique::<SuperzetSidebar>(),
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
    let store = SuperzetStore::global(cx);
    let Some(project) = store
        .read(cx)
        .active_project()
        .cloned()
        .or_else(|| store.read(cx).projects().first().cloned())
    else {
        workspace.show_toast(
            Toast::new(
                NotificationId::unique::<SuperzetSidebar>(),
                "Add a project before creating a workspace.",
            ),
            cx,
        );
        return;
    };

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
    let store = SuperzetStore::global(cx);
    let Some(workspace_entry) = store
        .read(cx)
        .active_workspace()
        .cloned()
        .or_else(|| store.read(cx).workspaces().first().cloned())
    else {
        workspace.show_toast(
            Toast::new(
                NotificationId::unique::<SuperzetSidebar>(),
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
                            NotificationId::unique::<SuperzetSidebar>(),
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
                workspace.open_panel::<SuperzetRightSidebar>(window, cx);
                workspace.focus_panel::<SuperzetRightSidebar>(window, cx);
                if let Some(panel) = workspace.panel::<SuperzetRightSidebar>(cx) {
                    panel.update(cx, |panel, cx| {
                        panel.set_active_tab(RightSidebarTab::Changes, cx)
                    });
                }
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
    _window: &mut gpui::Window,
    cx: &mut Context<Workspace>,
) {
    let store = SuperzetStore::global(cx);
    let Some(workspace_entry) = store
        .read(cx)
        .active_workspace()
        .cloned()
        .or_else(|| store.read(cx).workspaces().first().cloned())
    else {
        workspace.show_toast(
            Toast::new(
                NotificationId::unique::<SuperzetSidebar>(),
                "Select a workspace first.",
            ),
            cx,
        );
        return;
    };

    let app_state = workspace.app_state().clone();
    let open_task = open_workspace_entry_in_new_window(workspace_entry, app_state, cx);
    cx.spawn(async move |_, _| open_task.await)
        .detach_and_log_err(cx);
}

fn run_delete_workspace(
    workspace: &mut Workspace,
    window: &mut gpui::Window,
    cx: &mut Context<Workspace>,
) {
    let store = SuperzetStore::global(cx);
    let Some(workspace_entry) = store
        .read(cx)
        .active_workspace()
        .cloned()
        .or_else(|| store.read(cx).workspaces().first().cloned())
    else {
        workspace.show_toast(
            Toast::new(
                NotificationId::unique::<SuperzetSidebar>(),
                "Select a workspace first.",
            ),
            cx,
        );
        return;
    };
    if workspace_entry.kind == WorkspaceKind::Primary || !workspace_entry.managed {
        workspace.show_toast(
            Toast::new(
                NotificationId::unique::<SuperzetSidebar>(),
                "Primary workspaces cannot be deleted.",
            ),
            cx,
        );
        return;
    }
    let Some(project) = store.read(cx).project(&workspace_entry.project_id).cloned() else {
        workspace.show_toast(
            Toast::new(
                NotificationId::unique::<SuperzetSidebar>(),
                "Missing project metadata.",
            ),
            cx,
        );
        return;
    };

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
                        superzet_git::delete_workspace(&workspace_to_delete, &repo_root, false)
                    }
                })
                .await
            }
            ProjectLocation::Ssh { .. } => {
                let (project_workspace, primary_workspace) =
                    resolve_remote_project_workspace(&project, &store, app_state.clone(), true, cx)
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

                let open_primary_task = cx.update(|window, cx| {
                    open_workspace_entry(primary_workspace.clone(), app_state.clone(), window, cx)
                })?;
                open_primary_task.await?;
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
                        NotificationId::unique::<SuperzetSidebar>(),
                        format!("Deleted {}", workspace_entry.name),
                    ),
                    cx,
                );
                if let Some(primary_workspace) = store
                    .read(cx)
                    .primary_workspace_for_project(&project.id)
                    .cloned()
                {
                    store.update(cx, |store, cx| {
                        store.record_workspace_opened(&primary_workspace.id, cx);
                    });
                    if workspace_entry.local_worktree_path().is_some() {
                        open_workspace_entry(
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
                        NotificationId::unique::<SuperzetSidebar>(),
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
    let store = SuperzetStore::global(cx);
    let Some(project) = store.read(cx).project(project_id).cloned() else {
        workspace.show_toast(
            Toast::new(
                NotificationId::unique::<SuperzetSidebar>(),
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
            "Close `{}` and remove its {} from superzet?\n\nFiles, worktrees, and git history will remain on disk.",
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
                        Toast::new(NotificationId::unique::<SuperzetSidebar>(), message.clone()),
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
            Toast::new(NotificationId::unique::<SuperzetSidebar>(), message.clone()),
            cx,
        );
    }) {
        return;
    }
}

fn run_delete_workspace_from_store(
    workspace_handle: Entity<Workspace>,
    window: &mut gpui::Window,
    cx: &mut App,
) {
    workspace_handle.update(cx, |workspace, cx| {
        run_delete_workspace(workspace, window, cx);
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

fn open_workspace_entry_in_new_window(
    workspace_entry: WorkspaceEntry,
    app_state: Arc<WorkspaceAppState>,
    cx: &mut App,
) -> Task<anyhow::Result<()>> {
    match &workspace_entry.location {
        WorkspaceLocation::Local { worktree_path } => {
            let path = worktree_path.clone();
            cx.spawn(async move |cx| {
                cx.update(|cx| {
                    workspace::open_paths(
                        &[path],
                        app_state,
                        OpenOptions {
                            open_new_workspace: Some(true),
                            focus: Some(true),
                            visible: Some(OpenVisible::All),
                            ..Default::default()
                        },
                        cx,
                    )
                })
                .await?;
                anyhow::Ok(())
            })
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

            let path = PathBuf::from(worktree_path);
            cx.spawn(async move |cx| {
                open_remote_project(
                    remote_connection,
                    vec![path],
                    app_state,
                    OpenOptions {
                        open_new_workspace: Some(true),
                        focus: Some(true),
                        visible: Some(OpenVisible::All),
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
) -> superzet_model::WorkspaceLocator<'_> {
    match location {
        WorkspaceLocation::Local { worktree_path } => {
            superzet_model::WorkspaceLocator::Local(worktree_path)
        }
        WorkspaceLocation::Ssh {
            connection,
            worktree_path,
        } => superzet_model::WorkspaceLocator::Ssh {
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
    let summary = repository.status_summary();
    GitChangeSummary {
        changed_files: summary.count,
        staged_files: summary.index.added
            + summary.index.modified
            + summary.index.deleted
            + summary.conflict,
        untracked_files: summary.untracked,
    }
}

fn remote_path_basename(path: &str) -> String {
    path.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .find(|segment| !segment.is_empty())
        .unwrap_or("Project")
        .to_string()
}

fn build_remote_workspace_bundle(
    workspace: &Entity<Workspace>,
    store: &SuperzetStore,
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

    let (repo_root, branch, git_summary) = if let Some(repository) = active_repository {
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
            Some(git_change_summary_from_repository(&repository)),
        )
    } else {
        (worktree_path.clone(), "HEAD".to_string(), None)
    };

    let project_location = ProjectLocation::Ssh {
        connection: connection.clone(),
        repo_root: repo_root.clone(),
    };
    let existing_project = store.project_for_location(&project_location).cloned();
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

    let existing_workspace = store.workspace_for_location(&workspace_location).cloned();
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
        git_summary: git_summary.or_else(|| {
            existing_workspace
                .as_ref()
                .and_then(|workspace| workspace.git_summary.clone())
        }),
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
    _cx: &mut Context<SuperzetSidebar>,
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
                    "superzet-working-indicator-{workspace_id}"
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
                    "superzet-permission-indicator-{workspace_id}"
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
    store: &Entity<SuperzetStore>,
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
fn dispatch_native_terminal_notification(title: &str, body: &str) {
    let title = title.replace('\\', "\\\\").replace('"', "\\\"");
    let body = body.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!("display notification \"{body}\" with title \"{title}\"");

    let mut command = smol::process::Command::new("/usr/bin/osascript");
    if let Err(error) = command.arg("-e").arg(script).spawn() {
        log::error!("failed to dispatch macOS notification: {error}");
    }
}

#[cfg(not(target_os = "macos"))]
fn dispatch_native_terminal_notification(_title: &str, _body: &str) {}

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
        chips.push(
            Chip::new(workspace.branch.clone())
                .label_color(Color::Muted)
                .tooltip({
                    let branch = workspace.branch.clone();
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
