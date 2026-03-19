use std::{any::Any, path::PathBuf, rc::Rc};

use acp_thread::{
    AgentConnection, AgentSessionInfo, AgentSessionList, AgentSessionListRequest,
    AgentSessionListResponse, SessionListUpdate,
};
use agent::ThreadStore;
use agent_client_protocol as acp;
use agent_servers::{AgentServer, AgentServerDelegate};
use anyhow::Result;
use db::kvp::KEY_VALUE_STORE;
use fs::Fs;
use gpui::{
    App, Context, Entity, EntityId, EventEmitter, FocusHandle, Focusable, Pixels, Render,
    SharedString, Subscription, Task, WeakEntity, Window,
};
use project::{AgentId, Project};
use serde::{Deserialize, Serialize};
use ui::{Color, Icon, IconName, Label, prelude::*};
use util::ResultExt as _;
use workspace::item::TabContentParams;
use workspace::{Item, Pane, Toast, Workspace, notifications::NotificationId};

use crate::{
    Agent, AgentInitialContent, ConnectionView,
    agent_connection_store::AgentConnectionStore,
    thread_history::{ThreadHistory, ThreadHistoryEvent, ThreadHistoryOptions},
};

const LAST_USED_EXTERNAL_AGENT_KEY: &str = "agent_panel__last_used_external_agent";
const ACP_TAB_TITLE_MAX_WIDTH: Pixels = px(175.);

#[derive(Serialize, Deserialize)]
struct LastUsedExternalAgent {
    agent: Agent,
}

pub fn open_external_acp_tab(
    workspace: &mut Workspace,
    agent_name: Option<String>,
    prompt: Option<String>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if workspace.project().read(cx).is_via_collab() {
        show_toast(
            workspace,
            "External ACP tabs are not available in shared projects.",
            cx,
        );
        return;
    }

    match agent_name {
        Some(agent_name) => {
            open_external_acp_tab_for_agent(
                workspace,
                agent_name.into(),
                None,
                prompt,
                None,
                window,
                cx,
            );
        }
        None => {
            let workspace_handle = workspace.weak_handle();
            cx.spawn_in(window, async move |_workspace, cx| {
                let agent_name = resolve_default_agent_name(workspace_handle.clone(), cx).await?;
                let Some(agent_name) = agent_name else {
                    workspace_handle
                        .update_in(cx, |workspace, _, cx| {
                            show_toast(workspace, "Add an ACP agent to open a tab.", cx);
                        })
                        .ok();
                    return anyhow::Ok(());
                };

                workspace_handle.update_in(cx, |workspace, window, cx| {
                    open_external_acp_tab_for_agent(
                        workspace,
                        agent_name.clone(),
                        None,
                        prompt.clone(),
                        None,
                        window,
                        cx,
                    );
                })?;
                anyhow::Ok(())
            })
            .detach_and_log_err(cx);
        }
    }
}

pub fn open_external_acp_history(
    workspace: &mut Workspace,
    agent_name: Option<String>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if workspace.project().read(cx).is_via_collab() {
        show_toast(
            workspace,
            "External ACP history is not available in shared projects.",
            cx,
        );
        return;
    }

    match agent_name {
        Some(agent_name) => {
            open_external_acp_history_for_agent(workspace, agent_name.into(), window, cx);
        }
        None => {
            let workspace_handle = workspace.weak_handle();
            cx.spawn_in(window, async move |_workspace, cx| {
                let agent_name = resolve_default_agent_name(workspace_handle.clone(), cx).await?;
                let Some(agent_name) = agent_name else {
                    workspace_handle
                        .update_in(cx, |workspace, _, cx| {
                            show_toast(workspace, "Add an ACP agent to open history.", cx);
                        })
                        .ok();
                    return anyhow::Ok(());
                };

                workspace_handle.update_in(cx, |workspace, window, cx| {
                    open_external_acp_history_for_agent(workspace, agent_name.clone(), window, cx);
                })?;
                anyhow::Ok(())
            })
            .detach_and_log_err(cx);
        }
    }
}

pub fn show_external_acp_history(
    workspace: &mut Workspace,
    agent_name: Option<String>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if workspace.project().read(cx).is_via_collab() {
        show_toast(
            workspace,
            "External ACP history is not available in shared projects.",
            cx,
        );
        return;
    }

    match agent_name {
        Some(agent_name) => {
            let agent_name: SharedString = agent_name.into();
            if show_external_acp_history_in_active_tab(workspace, agent_name.clone(), window, cx) {
                remember_last_used_external_agent(agent_name, cx);
                return;
            }

            open_external_acp_history_for_agent(workspace, agent_name, window, cx);
        }
        None => {
            let workspace_handle = workspace.weak_handle();
            cx.spawn_in(window, async move |_workspace, cx| {
                let agent_name = resolve_default_agent_name(workspace_handle.clone(), cx).await?;
                let Some(agent_name) = agent_name else {
                    workspace_handle
                        .update_in(cx, |workspace, _, cx| {
                            show_toast(workspace, "Add an ACP agent to open history.", cx);
                        })
                        .ok();
                    return anyhow::Ok(());
                };

                workspace_handle.update_in(cx, |workspace, window, cx| {
                    if show_external_acp_history_in_active_tab(
                        workspace,
                        agent_name.clone(),
                        window,
                        cx,
                    ) {
                        remember_last_used_external_agent(agent_name.clone(), cx);
                    } else {
                        open_external_acp_history_for_agent(
                            workspace,
                            agent_name.clone(),
                            window,
                            cx,
                        );
                    }
                })?;
                anyhow::Ok(())
            })
            .detach_and_log_err(cx);
        }
    }
}

pub fn active_external_acp_agent_name(workspace: &Workspace, cx: &App) -> Option<String> {
    let active_pane = workspace.active_pane();
    let active_pane = active_pane.read(cx);
    active_external_acp_agent_name_in_pane(active_pane, cx)
}

pub fn active_external_acp_agent_name_in_pane(pane: &Pane, cx: &App) -> Option<String> {
    pane.active_item_as::<ExternalAcpTabItem>()
        .map(|item| item.read(cx).agent_name.to_string())
        .or_else(|| {
            pane.active_item_as::<ExternalAcpHistoryItem>()
                .map(|item| item.read(cx).agent_name.to_string())
        })
}

pub fn pane_has_external_acp_item(pane: &Pane) -> bool {
    pane.active_item_as::<ExternalAcpTabItem>().is_some()
        || pane.active_item_as::<ExternalAcpHistoryItem>().is_some()
}

pub fn focus_external_acp_tab(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> bool {
    if let Some(active_tab) = workspace
        .active_pane()
        .read(cx)
        .active_item_as::<ExternalAcpTabItem>()
    {
        workspace.activate_item(&active_tab, true, true, window, cx);
        return true;
    }

    let existing_tab = { workspace.items_of_type::<ExternalAcpTabItem>(cx).next() };
    if let Some(existing_tab) = existing_tab {
        workspace.activate_item(&existing_tab, true, true, window, cx);
        return true;
    }

    false
}

pub(crate) fn open_session_in_active_external_acp_tab(
    workspace: &mut Workspace,
    session: AgentSessionInfo,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> bool {
    let Some(active_tab) = workspace
        .active_pane()
        .read(cx)
        .active_item_as::<ExternalAcpTabItem>()
    else {
        return false;
    };

    active_tab.update(cx, |tab, cx| {
        tab.open_history_session(session.clone(), window, cx);
    });
    true
}

fn show_external_acp_history_in_active_tab(
    workspace: &mut Workspace,
    agent_name: SharedString,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> bool {
    let workspace_root_paths = workspace_root_paths(workspace, cx);
    let Some(active_tab) = workspace
        .active_pane()
        .read(cx)
        .active_item_as::<ExternalAcpTabItem>()
    else {
        return false;
    };

    if active_tab.read(cx).agent_name != agent_name {
        return false;
    }

    active_tab.update(cx, |tab, cx| {
        tab.show_history(workspace_root_paths, window, cx);
    });
    true
}

fn open_external_acp_tab_for_agent(
    workspace: &mut Workspace,
    agent_name: SharedString,
    resume_thread: Option<AgentSessionInfo>,
    prompt: Option<String>,
    skip_existing_tab: Option<EntityId>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let existing_tab = resume_thread
        .as_ref()
        .map(|thread| thread.session_id.clone())
        .and_then(|session_id| {
            workspace
                .items_of_type::<ExternalAcpTabItem>(cx)
                .find(|item| {
                    if skip_existing_tab.is_some_and(|entity_id| item.entity_id() == entity_id) {
                        return false;
                    }

                    item.read(cx)
                        .session_id(cx)
                        .is_some_and(|existing_session_id| existing_session_id == session_id)
                })
        });
    if let Some(existing_tab) = existing_tab {
        workspace.activate_item(&existing_tab, true, true, window, cx);
        remember_last_used_external_agent(agent_name, cx);
        return;
    }

    let project = workspace.project().clone();
    let display_name = agent_display_name(&project, &agent_name, cx);
    let icon_path = agent_icon_path(&project, &agent_name, cx);
    let server = Agent::Custom {
        id: AgentId::new(agent_name.clone()),
    }
    .server(<dyn Fs>::global(cx), ThreadStore::global(cx));
    let thread_store = server
        .clone()
        .downcast::<agent::NativeAgentServer>()
        .is_some()
        .then(|| ThreadStore::global(cx));
    let tab = cx.new(|cx| {
        ExternalAcpTabItem::new(
            agent_name.clone(),
            display_name,
            icon_path,
            server,
            workspace.weak_handle(),
            project,
            resume_thread,
            prompt,
            thread_store,
            window,
            cx,
        )
    });
    workspace.add_item_to_center(Box::new(tab), window, cx);
    remember_last_used_external_agent(agent_name, cx);
}

fn open_external_acp_history_for_agent(
    workspace: &mut Workspace,
    agent_name: SharedString,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let existing_history = {
        workspace
            .items_of_type::<ExternalAcpHistoryItem>(cx)
            .find(|item| item.read(cx).agent_name == agent_name)
    };
    if let Some(existing_history) = existing_history {
        workspace.activate_item(&existing_history, true, true, window, cx);
        remember_last_used_external_agent(agent_name, cx);
        return;
    }

    let project = workspace.project().clone();
    let workspace_root_paths = workspace
        .root_paths(cx)
        .into_iter()
        .map(|path| path.to_path_buf())
        .collect::<Vec<_>>();
    let history = cx.new(|cx| {
        ExternalAcpHistoryItem::new(
            agent_name.clone(),
            workspace.weak_handle(),
            project,
            workspace_root_paths,
            window,
            cx,
        )
    });
    workspace.add_item_to_center(Box::new(history), window, cx);
    remember_last_used_external_agent(agent_name, cx);
}

async fn resolve_default_agent_name(
    workspace: WeakEntity<Workspace>,
    cx: &mut gpui::AsyncWindowContext,
) -> Result<Option<SharedString>> {
    let last_used_agent = cx
        .background_spawn(async move { read_last_used_external_agent() })
        .await;
    let configured_agents = workspace.read_with(cx, |workspace, cx| {
        configured_external_agent_names(&workspace.project(), cx)
    })?;

    Ok(select_default_agent_name(
        configured_agents,
        last_used_agent,
    ))
}

fn select_default_agent_name(
    configured_agents: Vec<SharedString>,
    last_used_agent: Option<SharedString>,
) -> Option<SharedString> {
    if let Some(last_used_agent) = last_used_agent
        && configured_agents
            .iter()
            .any(|agent| agent == &last_used_agent)
    {
        return Some(last_used_agent);
    }

    configured_agents.into_iter().next()
}

fn configured_external_agent_names(project: &Entity<Project>, cx: &App) -> Vec<SharedString> {
    let agent_server_store = project.read(cx).agent_server_store().clone();
    let agent_server_store = agent_server_store.read(cx);
    let mut agents = agent_server_store
        .external_agents()
        .map(|agent_name| {
            (
                agent_name.0.clone(),
                agent_server_store
                    .agent_display_name(agent_name)
                    .unwrap_or_else(|| agent_name.0.clone()),
            )
        })
        .collect::<Vec<_>>();
    agents.sort_by(|left, right| left.1.to_lowercase().cmp(&right.1.to_lowercase()));
    agents
        .into_iter()
        .map(|(agent_name, _)| agent_name)
        .collect()
}

fn read_last_used_external_agent() -> Option<SharedString> {
    KEY_VALUE_STORE
        .read_kvp(LAST_USED_EXTERNAL_AGENT_KEY)
        .log_err()
        .flatten()
        .and_then(|value| serde_json::from_str::<LastUsedExternalAgent>(&value).log_err())
        .and_then(|entry| match entry.agent {
            Agent::Custom { id } => Some(id.into()),
            Agent::NativeAgent => None,
        })
}

fn remember_last_used_external_agent(agent_name: SharedString, cx: &mut App) {
    cx.background_spawn(async move {
        let Some(serialized) = serde_json::to_string(&LastUsedExternalAgent {
            agent: Agent::Custom {
                id: AgentId::new(agent_name),
            },
        })
        .log_err() else {
            return;
        };

        KEY_VALUE_STORE
            .write_kvp(LAST_USED_EXTERNAL_AGENT_KEY.to_string(), serialized)
            .await
            .log_err();
    })
    .detach();
}

fn agent_display_name(
    project: &Entity<Project>,
    agent_name: &SharedString,
    cx: &App,
) -> SharedString {
    let agent_id = AgentId::new(agent_name.clone());
    let agent_server_store = project.read(cx).agent_server_store().clone();
    agent_server_store
        .read(cx)
        .agent_display_name(&agent_id)
        .unwrap_or_else(|| agent_name.clone())
}

fn agent_icon_path(
    project: &Entity<Project>,
    agent_name: &SharedString,
    cx: &App,
) -> Option<SharedString> {
    let agent_id = AgentId::new(agent_name.clone());
    let agent_server_store = project.read(cx).agent_server_store().clone();
    agent_server_store
        .read(cx)
        .agent_icon(&agent_id)
        .or_else(|| {
            project::AgentRegistryStore::try_global(cx).and_then(|registry_store| {
                registry_store
                    .read(cx)
                    .agent(&agent_id)
                    .and_then(|agent| agent.icon_path().cloned())
            })
        })
}

fn show_toast(
    workspace: &mut Workspace,
    message: impl Into<SharedString>,
    cx: &mut Context<Workspace>,
) {
    struct ExternalAcpToast;

    workspace.show_toast(
        Toast::new(
            NotificationId::unique::<ExternalAcpToast>(),
            message.into().to_string(),
        )
        .autohide(),
        cx,
    );
}

fn workspace_root_paths(workspace: &Workspace, cx: &App) -> Vec<PathBuf> {
    workspace
        .root_paths(cx)
        .into_iter()
        .map(|path| path.to_path_buf())
        .collect()
}

struct ProjectFilteredSessionList {
    inner: Rc<dyn AgentSessionList>,
    workspace_root_paths: Vec<PathBuf>,
}

impl ProjectFilteredSessionList {
    fn new(inner: Rc<dyn AgentSessionList>, workspace_root_paths: Vec<PathBuf>) -> Self {
        Self {
            inner,
            workspace_root_paths,
        }
    }

    fn filter_response(
        response: AgentSessionListResponse,
        workspace_root_paths: &[PathBuf],
    ) -> AgentSessionListResponse {
        if workspace_root_paths.is_empty() {
            return response;
        }

        let sessions = response
            .sessions
            .into_iter()
            .filter(|session| {
                session.work_dirs.as_ref().is_some_and(|work_dirs| {
                    work_dirs.paths().iter().any(|work_dir| {
                        workspace_root_paths
                            .iter()
                            .any(|root_path| work_dir.starts_with(root_path))
                    })
                })
            })
            .collect();

        AgentSessionListResponse {
            sessions,
            next_cursor: response.next_cursor,
            meta: response.meta,
        }
    }
}

impl AgentSessionList for ProjectFilteredSessionList {
    fn list_sessions(
        &self,
        request: AgentSessionListRequest,
        cx: &mut App,
    ) -> Task<Result<AgentSessionListResponse>> {
        let task = self.inner.list_sessions(request, cx);
        let workspace_root_paths = self.workspace_root_paths.clone();
        cx.foreground_executor().spawn(async move {
            let response = task.await?;
            Ok(Self::filter_response(response, &workspace_root_paths))
        })
    }

    fn supports_delete(&self) -> bool {
        self.inner.supports_delete()
    }

    fn delete_session(&self, session_id: &acp::SessionId, cx: &mut App) -> Task<Result<()>> {
        self.inner.delete_session(session_id, cx)
    }

    fn delete_sessions(&self, cx: &mut App) -> Task<Result<()>> {
        self.inner.delete_sessions(cx)
    }

    fn watch(&self, cx: &mut App) -> Option<smol::channel::Receiver<SessionListUpdate>> {
        self.inner.watch(cx)
    }

    fn notify_refresh(&self) {
        self.inner.notify_refresh();
    }

    fn into_any(self: Rc<Self>) -> Rc<dyn Any> {
        self
    }
}

struct ExternalAcpTabItem {
    agent_name: SharedString,
    display_name: SharedString,
    icon_path: Option<SharedString>,
    server: Rc<dyn AgentServer>,
    workspace: WeakEntity<Workspace>,
    project: Entity<Project>,
    thread_store: Option<Entity<ThreadStore>>,
    connection_view: Entity<ConnectionView>,
    history_view: Entity<ThreadHistory>,
    history_mode: ExternalAcpTabMode,
    history_workspace_root_paths: Option<Vec<PathBuf>>,
    history_source_session_list: Option<Rc<dyn AgentSessionList>>,
    _subscriptions: Vec<Subscription>,
}

#[derive(Clone, Copy, Default, Eq, PartialEq)]
enum ExternalAcpTabMode {
    #[default]
    Conversation,
    History,
}

impl ExternalAcpTabItem {
    #[allow(clippy::too_many_arguments)]
    fn new(
        agent_name: SharedString,
        display_name: SharedString,
        icon_path: Option<SharedString>,
        server: Rc<dyn AgentServer>,
        workspace: WeakEntity<Workspace>,
        project: Entity<Project>,
        resume_thread: Option<AgentSessionInfo>,
        prompt: Option<String>,
        thread_store: Option<Entity<ThreadStore>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let history_view = cx.new(|cx| {
            ThreadHistory::new_with_options(
                None,
                ThreadHistoryOptions {
                    show_back_button: true,
                    show_open_in_new_tab: true,
                },
                window,
                cx,
            )
        });
        let connection_view = Self::build_connection_view(
            agent_name.clone(),
            server.clone(),
            workspace.clone(),
            project.clone(),
            thread_store.clone(),
            resume_thread,
            prompt,
            window,
            cx,
        );
        let current_tab_id = cx.entity_id();

        let subscriptions = vec![
            cx.observe(&connection_view, |this, _, cx| {
                this.sync_history_view_session_list(false, cx);
                cx.notify();
            }),
            cx.observe(&history_view, |_, _, cx| {
                cx.notify();
            }),
            cx.subscribe_in(
                &history_view,
                window,
                move |this, _, event, window, cx| match event {
                    ThreadHistoryEvent::Back => {
                        this.hide_history(window, cx);
                    }
                    ThreadHistoryEvent::Open(thread) => {
                        this.open_history_session(thread.clone(), window, cx);
                    }
                    ThreadHistoryEvent::OpenInNewTab(thread) => {
                        let workspace = this.workspace.clone();
                        let agent_name = this.agent_name.clone();
                        let thread = thread.clone();
                        window.defer(cx, move |window, cx| {
                            workspace
                                .update(cx, |workspace, cx| {
                                    open_external_acp_tab_for_agent(
                                        workspace,
                                        agent_name.clone(),
                                        Some(thread.clone()),
                                        None,
                                        Some(current_tab_id),
                                        window,
                                        cx,
                                    );
                                })
                                .log_err();
                        });
                    }
                },
            ),
        ];

        Self {
            agent_name,
            display_name,
            icon_path,
            server,
            workspace,
            project,
            thread_store,
            connection_view,
            history_view,
            history_mode: ExternalAcpTabMode::Conversation,
            history_workspace_root_paths: None,
            history_source_session_list: None,
            _subscriptions: subscriptions,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn build_connection_view(
        agent_name: SharedString,
        server: Rc<dyn AgentServer>,
        workspace: WeakEntity<Workspace>,
        project: Entity<Project>,
        thread_store: Option<Entity<ThreadStore>>,
        resume_thread: Option<AgentSessionInfo>,
        prompt: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<ConnectionView> {
        let connection_store = cx.new(|cx| AgentConnectionStore::new(project.clone(), cx));
        let connection_key = Agent::Custom {
            id: AgentId::new(agent_name),
        };
        let resume_session_id = resume_thread
            .as_ref()
            .map(|thread| thread.session_id.clone());
        let work_dirs = resume_thread
            .as_ref()
            .and_then(|thread| thread.work_dirs.clone());
        let title = resume_thread
            .as_ref()
            .and_then(|thread| thread.title.clone());
        let initial_content = prompt.map(|text| AgentInitialContent::ContentBlock {
            blocks: vec![acp::ContentBlock::Text(acp::TextContent::new(text))],
            auto_submit: false,
        });

        cx.new(|cx| {
            ConnectionView::new(
                server,
                connection_store,
                connection_key,
                resume_session_id,
                work_dirs,
                title,
                initial_content,
                workspace,
                project,
                thread_store,
                None,
                window,
                cx,
            )
        })
    }

    fn session_id(&self, cx: &App) -> Option<acp::SessionId> {
        self.connection_view.read(cx).session_id(cx)
    }

    fn show_history(
        &mut self,
        workspace_root_paths: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.history_workspace_root_paths = Some(workspace_root_paths);
        self.sync_history_view_session_list(true, cx);
        self.history_view
            .update(cx, |history, cx| history.refresh_full_history(cx));
        self.history_mode = ExternalAcpTabMode::History;
        self.history_view
            .read(cx)
            .focus_handle(cx)
            .focus(window, cx);
        cx.notify();
    }

    fn hide_history(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.history_mode = ExternalAcpTabMode::Conversation;
        self.connection_view
            .read(cx)
            .focus_handle(cx)
            .focus(window, cx);
        cx.notify();
    }

    fn sync_history_view_session_list(&mut self, force_rebuild: bool, cx: &mut Context<Self>) {
        let Some(workspace_root_paths) = self.history_workspace_root_paths.clone() else {
            return;
        };

        let next_source = self
            .connection_view
            .read(cx)
            .history()
            .and_then(|history| history.read(cx).session_list());
        let source_changed = match (&self.history_source_session_list, &next_source) {
            (Some(current), Some(next)) => !Rc::ptr_eq(current, next),
            (None, None) => false,
            _ => true,
        };
        if !force_rebuild && !source_changed {
            return;
        }

        self.history_source_session_list = next_source.clone();
        let filtered_session_list = next_source.map(|session_list| {
            Rc::new(ProjectFilteredSessionList::new(
                session_list,
                workspace_root_paths,
            )) as Rc<dyn AgentSessionList>
        });
        self.history_view.update(cx, |history, cx| {
            history.set_session_list(filtered_session_list, cx);
        });
    }

    fn open_history_session(
        &mut self,
        thread: AgentSessionInfo,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let connection_view = Self::build_connection_view(
            self.agent_name.clone(),
            self.server.clone(),
            self.workspace.clone(),
            self.project.clone(),
            self.thread_store.clone(),
            Some(thread),
            None,
            window,
            cx,
        );
        self._subscriptions
            .push(cx.observe(&connection_view, |_, _, cx| {
                cx.notify();
            }));
        self.connection_view = connection_view;
        self.history_mode = ExternalAcpTabMode::Conversation;
        self.connection_view
            .read(cx)
            .focus_handle(cx)
            .focus(window, cx);
        cx.notify();
    }
}

impl Item for ExternalAcpTabItem {
    type Event = ();

    fn include_in_nav_history() -> bool {
        false
    }

    fn tab_content(&self, params: TabContentParams, _window: &Window, cx: &App) -> AnyElement {
        let text = match self.history_mode {
            ExternalAcpTabMode::Conversation => self.connection_view.read(cx).tab_title(cx),
            ExternalAcpTabMode::History => format!("{} History", self.display_name).into(),
        };
        render_truncated_tab_title(text, params)
    }

    fn tab_content_text(&self, _detail: usize, cx: &App) -> SharedString {
        match self.history_mode {
            ExternalAcpTabMode::Conversation => self.connection_view.read(cx).tab_title(cx),
            ExternalAcpTabMode::History => format!("{} History", self.display_name).into(),
        }
    }

    fn tab_tooltip_text(&self, cx: &App) -> Option<SharedString> {
        Some(match self.history_mode {
            ExternalAcpTabMode::Conversation => self.connection_view.read(cx).tab_title(cx),
            ExternalAcpTabMode::History => format!("{} History", self.display_name).into(),
        })
    }

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<Icon> {
        Some(match &self.icon_path {
            Some(icon_path) => Icon::from_external_svg(icon_path.clone()),
            None => Icon::new(IconName::Sparkle),
        })
    }
}

impl EventEmitter<()> for ExternalAcpTabItem {}

impl Focusable for ExternalAcpTabItem {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match self.history_mode {
            ExternalAcpTabMode::Conversation => self.connection_view.read(cx).focus_handle(cx),
            ExternalAcpTabMode::History => self.history_view.read(cx).focus_handle(cx),
        }
    }
}

impl Render for ExternalAcpTabItem {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        match self.history_mode {
            ExternalAcpTabMode::Conversation => self.connection_view.clone().into_any_element(),
            ExternalAcpTabMode::History => self.history_view.clone().into_any_element(),
        }
    }
}

enum HistoryState {
    Loading,
    Unsupported(SharedString),
    Error(SharedString),
    Ready {
        history: Entity<ThreadHistory>,
        _connection: Rc<dyn AgentConnection>,
    },
}

struct ExternalAcpHistoryItem {
    agent_name: SharedString,
    display_name: SharedString,
    icon_path: Option<SharedString>,
    workspace: WeakEntity<Workspace>,
    focus_handle: FocusHandle,
    state: HistoryState,
    _load_task: Task<Result<()>>,
    _subscriptions: Vec<Subscription>,
}

impl ExternalAcpHistoryItem {
    fn new(
        agent_name: SharedString,
        workspace: WeakEntity<Workspace>,
        project: Entity<Project>,
        workspace_root_paths: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let display_name = agent_display_name(&project, &agent_name, cx);
        let icon_path = agent_icon_path(&project, &agent_name, cx);
        let focus_handle = cx.focus_handle();
        let delegate =
            AgentServerDelegate::new(project.read(cx).agent_server_store().clone(), None);
        let server = Agent::Custom {
            id: AgentId::new(agent_name.clone()),
        }
        .server(<dyn Fs>::global(cx), ThreadStore::global(cx));
        let connect_task = server.connect(delegate, cx);

        let mut this = Self {
            agent_name,
            display_name,
            icon_path,
            workspace,
            focus_handle,
            state: HistoryState::Loading,
            _load_task: Task::ready(Ok(())),
            _subscriptions: Vec::new(),
        };

        this._load_task = cx.spawn_in(window, async move |this, cx| {
            let result: Result<()> = async {
                let connection = connect_task.await?;
                let session_list = if connection.supports_session_history() {
                    cx.update(|_, cx| connection.session_list(cx))?
                } else {
                    None
                };

                this.update_in(cx, move |this, window, cx| {
                    let Some(session_list) = session_list else {
                        this.state = HistoryState::Unsupported(
                            format!("{} does not expose session history.", this.display_name)
                                .into(),
                        );
                        cx.notify();
                        return;
                    };

                    let filtered_session_list: Rc<dyn AgentSessionList> = Rc::new(
                        ProjectFilteredSessionList::new(session_list, workspace_root_paths),
                    );
                    let history = cx.new(|cx| {
                        ThreadHistory::new_with_options(
                            Some(filtered_session_list),
                            ThreadHistoryOptions {
                                show_open_in_new_tab: true,
                                ..Default::default()
                            },
                            window,
                            cx,
                        )
                    });
                    this._subscriptions.push(cx.observe(&history, |_, _, cx| {
                        cx.notify();
                    }));
                    this._subscriptions.push(cx.subscribe_in(
                        &history,
                        window,
                        |this, _, event, window, cx| match event {
                            ThreadHistoryEvent::Open(thread)
                            | ThreadHistoryEvent::OpenInNewTab(thread) => {
                                let workspace = this.workspace.clone();
                                let thread = thread.clone();
                                let agent_name = this.agent_name.clone();
                                window.defer(cx, move |window, cx| {
                                    workspace
                                        .update(cx, |workspace, cx| {
                                            open_external_acp_tab_for_agent(
                                                workspace,
                                                agent_name.clone(),
                                                Some(thread.clone()),
                                                None,
                                                None,
                                                window,
                                                cx,
                                            );
                                        })
                                        .log_err();
                                });
                            }
                            ThreadHistoryEvent::Back => {}
                        },
                    ));
                    this.state = HistoryState::Ready {
                        history,
                        _connection: connection,
                    };
                    cx.notify();
                })?;

                Ok(())
            }
            .await;

            if let Err(error) = result {
                this.update_in(cx, |this, _, cx| {
                    this.state = HistoryState::Error(error.to_string().into());
                    cx.notify();
                })
                .ok();
            }

            Ok(())
        });

        this
    }
}

impl Item for ExternalAcpHistoryItem {
    type Event = ();

    fn include_in_nav_history() -> bool {
        false
    }

    fn tab_content(&self, params: TabContentParams, _window: &Window, _cx: &App) -> AnyElement {
        render_truncated_tab_title(format!("{} History", self.display_name).into(), params)
    }

    fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
        format!("{} History", self.display_name).into()
    }

    fn tab_tooltip_text(&self, _cx: &App) -> Option<SharedString> {
        Some(format!("{} History", self.display_name).into())
    }

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<Icon> {
        Some(match &self.icon_path {
            Some(icon_path) => Icon::from_external_svg(icon_path.clone()),
            None => Icon::new(IconName::Sparkle),
        })
    }
}

impl EventEmitter<()> for ExternalAcpHistoryItem {}

impl Focusable for ExternalAcpHistoryItem {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match &self.state {
            HistoryState::Ready { history, .. } => history.read(cx).focus_handle(cx),
            HistoryState::Loading | HistoryState::Unsupported(_) | HistoryState::Error(_) => {
                self.focus_handle.clone()
            }
        }
    }
}

impl Render for ExternalAcpHistoryItem {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        match &self.state {
            HistoryState::Ready { history, .. } => history.clone().into_any_element(),
            HistoryState::Loading => {
                centered_message(format!("Loading {} history…", self.display_name))
            }
            HistoryState::Unsupported(message) | HistoryState::Error(message) => {
                centered_message(message.clone())
            }
        }
    }
}

fn centered_message(message: impl Into<SharedString>) -> AnyElement {
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap_2()
        .child(
            Label::new(message.into())
                .color(Color::Muted)
                .size(LabelSize::Small),
        )
        .into_any_element()
}

fn render_truncated_tab_title(text: SharedString, params: TabContentParams) -> AnyElement {
    div()
        .max_w(ACP_TAB_TITLE_MAX_WIDTH)
        .min_w_0()
        .overflow_hidden()
        .child(Label::new(text).color(params.text_color()).truncate())
        .into_any_element()
}
