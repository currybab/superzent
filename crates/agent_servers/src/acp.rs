use acp_thread::{
    AgentConnection, AgentSessionInfo, AgentSessionList, AgentSessionListRequest,
    AgentSessionListResponse,
};
use acp_tools::AcpConnectionRegistry;
use action_log::ActionLog;
use agent_client_protocol::{self as acp, Agent as _, ErrorCode};
use anyhow::anyhow;
use collections::HashMap;
use futures::future::Shared;
use futures::io::BufReader;
use futures::{AsyncBufReadExt as _, FutureExt as _};
use project::agent_server_store::AgentServerCommand;
use project::{AgentId, Project};
use serde::Deserialize;
use settings::Settings as _;
use task::ShellBuilder;
use util::ResultExt as _;
use util::path_list::PathList;
use util::process::Child;

use std::path::PathBuf;
use std::process::Stdio;
use std::rc::Rc;
use std::sync::Arc;
use std::{any::Any, cell::RefCell};
use thiserror::Error;

use anyhow::{Context as _, Result};
use gpui::{App, AppContext as _, AsyncApp, Entity, SharedString, Task, WeakEntity};

use acp_thread::{AcpThread, AuthRequired, LoadError, TerminalProviderEvent};
use terminal::TerminalBuilder;
use terminal::terminal_settings::{AlternateScroll, CursorShape, TerminalSettings};

use crate::GEMINI_ID;

#[derive(Debug, Error)]
#[error("Unsupported version")]
pub struct UnsupportedVersion;

pub struct AcpConnection {
    id: AgentId,
    display_name: SharedString,
    telemetry_id: SharedString,
    connection: Rc<acp::ClientSideConnection>,
    sessions: Rc<RefCell<HashMap<acp::SessionId, AcpSession>>>,
    pending_sessions: Rc<RefCell<HashMap<acp::SessionId, PendingAcpSession>>>,
    auth_methods: Vec<acp::AuthMethod>,
    agent_capabilities: acp::AgentCapabilities,
    default_mode: Option<acp::SessionModeId>,
    default_model: Option<acp::ModelId>,
    default_config_options: HashMap<String, String>,
    child: Option<Child>,
    session_list: Option<Rc<AcpSessionList>>,
    _io_task: Task<Result<(), acp::Error>>,
    _wait_task: Task<Result<()>>,
    _stderr_task: Task<Result<()>>,
}

struct PendingAcpSession {
    task: Shared<Task<Result<Entity<AcpThread>, Arc<anyhow::Error>>>>,
    ref_count: usize,
}

struct SessionConfigResponse {
    modes: Option<acp::SessionModeState>,
    models: Option<acp::SessionModelState>,
    config_options: Option<Vec<acp::SessionConfigOption>>,
}

#[derive(Clone)]
struct ConfigOptions {
    config_options: Rc<RefCell<Vec<acp::SessionConfigOption>>>,
    tx: Rc<RefCell<watch::Sender<()>>>,
    rx: watch::Receiver<()>,
}

impl ConfigOptions {
    fn new(config_options: Rc<RefCell<Vec<acp::SessionConfigOption>>>) -> Self {
        let (tx, rx) = watch::channel(());
        Self {
            config_options,
            tx: Rc::new(RefCell::new(tx)),
            rx,
        }
    }
}

pub struct AcpSession {
    thread: WeakEntity<AcpThread>,
    suppress_abort_err: bool,
    models: Option<Rc<RefCell<acp::SessionModelState>>>,
    session_modes: Option<Rc<RefCell<acp::SessionModeState>>>,
    config_options: Option<ConfigOptions>,
    ref_count: usize,
}

pub struct AcpSessionList {
    connection: Rc<acp::ClientSideConnection>,
    updates_tx: smol::channel::Sender<acp_thread::SessionListUpdate>,
    updates_rx: smol::channel::Receiver<acp_thread::SessionListUpdate>,
}

impl AcpSessionList {
    fn new(connection: Rc<acp::ClientSideConnection>) -> Self {
        let (tx, rx) = smol::channel::unbounded();
        Self {
            connection,
            updates_tx: tx,
            updates_rx: rx,
        }
    }

    fn notify_update(&self) {
        self.updates_tx
            .try_send(acp_thread::SessionListUpdate::Refresh)
            .log_err();
    }

    fn send_info_update(&self, session_id: acp::SessionId, update: acp::SessionInfoUpdate) {
        self.updates_tx
            .try_send(acp_thread::SessionListUpdate::SessionInfo { session_id, update })
            .log_err();
    }
}

impl AgentSessionList for AcpSessionList {
    fn list_sessions(
        &self,
        request: AgentSessionListRequest,
        cx: &mut App,
    ) -> Task<Result<AgentSessionListResponse>> {
        let conn = self.connection.clone();
        cx.foreground_executor().spawn(async move {
            let acp_request = acp::ListSessionsRequest::new()
                .cwd(request.cwd)
                .cursor(request.cursor);
            let response = conn.list_sessions(acp_request).await?;
            Ok(AgentSessionListResponse {
                sessions: response
                    .sessions
                    .into_iter()
                    .map(|s| AgentSessionInfo {
                        session_id: s.session_id,
                        work_dirs: Some(PathList::new(&[s.cwd])),
                        title: s.title.map(Into::into),
                        updated_at: s.updated_at.and_then(|date_str| {
                            chrono::DateTime::parse_from_rfc3339(&date_str)
                                .ok()
                                .map(|dt| dt.with_timezone(&chrono::Utc))
                        }),
                        created_at: None,
                        meta: s.meta,
                    })
                    .collect(),
                next_cursor: response.next_cursor,
                meta: response.meta,
            })
        })
    }

    fn watch(
        &self,
        _cx: &mut App,
    ) -> Option<smol::channel::Receiver<acp_thread::SessionListUpdate>> {
        Some(self.updates_rx.clone())
    }

    fn notify_refresh(&self) {
        self.notify_update();
    }

    fn into_any(self: Rc<Self>) -> Rc<dyn Any> {
        self
    }
}

pub async fn connect(
    agent_id: AgentId,
    display_name: SharedString,
    command: AgentServerCommand,
    default_mode: Option<acp::SessionModeId>,
    default_model: Option<acp::ModelId>,
    default_config_options: HashMap<String, String>,
    cx: &mut AsyncApp,
) -> Result<Rc<dyn AgentConnection>> {
    let conn = AcpConnection::stdio(
        agent_id,
        display_name,
        command.clone(),
        default_mode,
        default_model,
        default_config_options,
        cx,
    )
    .await?;
    Ok(Rc::new(conn) as _)
}

const MINIMUM_SUPPORTED_VERSION: acp::ProtocolVersion = acp::ProtocolVersion::V1;

impl AcpConnection {
    pub async fn stdio(
        agent_id: AgentId,
        display_name: SharedString,
        command: AgentServerCommand,
        default_mode: Option<acp::SessionModeId>,
        default_model: Option<acp::ModelId>,
        default_config_options: HashMap<String, String>,
        cx: &mut AsyncApp,
    ) -> Result<Self> {
        let shell = cx.update(|cx| TerminalSettings::get(None, cx).shell.clone());
        let builder = ShellBuilder::new(&shell, cfg!(windows)).non_interactive();
        let mut child =
            builder.build_std_command(Some(command.path.display().to_string()), &command.args);
        child.envs(command.env.iter().flatten());
        let mut child = Child::spawn(child, Stdio::piped(), Stdio::piped(), Stdio::piped())?;

        let stdout = child.stdout.take().context("Failed to take stdout")?;
        let stdin = child.stdin.take().context("Failed to take stdin")?;
        let stderr = child.stderr.take().context("Failed to take stderr")?;
        log::debug!(
            "Spawning external agent server: {:?}, {:?}",
            command.path,
            command.args
        );
        log::trace!("Spawned (pid: {})", child.id());

        let sessions = Rc::new(RefCell::new(HashMap::default()));
        let pending_sessions = Rc::new(RefCell::new(HashMap::default()));

        let (release_channel, version): (Option<&str>, String) = cx.update(|cx| {
            (
                release_channel::ReleaseChannel::try_global(cx)
                    .map(|release_channel| release_channel.display_name()),
                release_channel::AppVersion::global(cx).to_string(),
            )
        });

        let client_session_list: Rc<RefCell<Option<Rc<AcpSessionList>>>> =
            Rc::new(RefCell::new(None));

        let client = ClientDelegate {
            sessions: sessions.clone(),
            session_list: client_session_list.clone(),
            cx: cx.clone(),
        };
        let (connection, io_task) = acp::ClientSideConnection::new(client, stdin, stdout, {
            let foreground_executor = cx.foreground_executor().clone();
            move |fut| {
                foreground_executor.spawn(fut).detach();
            }
        });

        let io_task = cx.background_spawn(io_task);

        let stderr_task = cx.background_spawn(async move {
            let mut stderr = BufReader::new(stderr);
            let mut line = String::new();
            while let Ok(n) = stderr.read_line(&mut line).await
                && n > 0
            {
                log::warn!("agent stderr: {}", line.trim());
                line.clear();
            }
            Ok(())
        });

        let wait_task = cx.spawn({
            let sessions = sessions.clone();
            let status_fut = child.status();
            async move |cx| {
                let status = status_fut.await?;
                emit_load_error_to_all_sessions(&sessions, LoadError::Exited { status }, cx);
                anyhow::Ok(())
            }
        });

        let connection = Rc::new(connection);

        cx.update(|cx| {
            AcpConnectionRegistry::default_global(cx).update(cx, |registry, cx| {
                registry.set_active_connection(agent_id.clone(), &connection, cx)
            });
        });

        let response = connection
            .initialize(
                acp::InitializeRequest::new(acp::ProtocolVersion::V1)
                    .client_capabilities(
                        acp::ClientCapabilities::new()
                            .fs(acp::FileSystemCapabilities::new()
                                .read_text_file(true)
                                .write_text_file(true))
                            .terminal(true)
                            // Experimental: Allow for rendering terminal output from the agents
                            .meta(acp::Meta::from_iter([
                                ("terminal_output".into(), true.into()),
                                ("terminal-auth".into(), true.into()),
                            ])),
                    )
                    .client_info(
                        acp::Implementation::new("zed", version)
                            .title(release_channel.map(ToOwned::to_owned)),
                    ),
            )
            .await?;

        if response.protocol_version < MINIMUM_SUPPORTED_VERSION {
            return Err(UnsupportedVersion.into());
        }

        let telemetry_id = response
            .agent_info
            // Use the one the agent provides if we have one
            .map(|info| info.name.into())
            // Otherwise, just use the name
            .unwrap_or_else(|| agent_id.0.to_string().into());

        let session_list = if response
            .agent_capabilities
            .session_capabilities
            .list
            .is_some()
        {
            let list = Rc::new(AcpSessionList::new(connection.clone()));
            *client_session_list.borrow_mut() = Some(list.clone());
            Some(list)
        } else {
            None
        };

        // TODO: Remove this override once Google team releases their official auth methods
        let auth_methods = if agent_id.0.as_ref() == GEMINI_ID {
            let mut args = command.args.clone();
            args.retain(|a| a != "--experimental-acp" && a != "--acp");
            let value = serde_json::json!({
                "label": "gemini /auth",
                "command": command.path.to_string_lossy().into_owned(),
                "args": args,
                "env": command.env.clone().unwrap_or_default(),
            });
            let meta = acp::Meta::from_iter([("terminal-auth".to_string(), value)]);
            vec![acp::AuthMethod::Agent(
                acp::AuthMethodAgent::new("spawn-gemini-cli", "Login")
                    .description("Login with your Google or Vertex AI account")
                    .meta(meta),
            )]
        } else {
            response.auth_methods
        };
        Ok(Self {
            id: agent_id,
            auth_methods,
            connection,
            display_name,
            telemetry_id,
            sessions,
            pending_sessions,
            agent_capabilities: response.agent_capabilities,
            default_mode,
            default_model,
            default_config_options,
            session_list,
            _io_task: io_task,
            _wait_task: wait_task,
            _stderr_task: stderr_task,
            child: Some(child),
        })
    }

    pub fn prompt_capabilities(&self) -> &acp::PromptCapabilities {
        &self.agent_capabilities.prompt_capabilities
    }

    fn open_or_create_session(
        self: Rc<Self>,
        session_id: acp::SessionId,
        project: Entity<Project>,
        work_dirs: PathList,
        title: Option<SharedString>,
        rpc_call: impl FnOnce(
            Rc<acp::ClientSideConnection>,
            acp::SessionId,
            PathBuf,
        )
            -> futures::future::LocalBoxFuture<'static, Result<SessionConfigResponse>>
        + 'static,
        cx: &mut App,
    ) -> Task<Result<Entity<AcpThread>>> {
        if let Some(pending) = self.pending_sessions.borrow_mut().get_mut(&session_id) {
            pending.ref_count += 1;
            let task = pending.task.clone();
            return cx
                .foreground_executor()
                .spawn(async move { task.await.map_err(|err| anyhow!(err)) });
        }

        if let Some(session) = self.sessions.borrow_mut().get_mut(&session_id) {
            session.ref_count += 1;
            if let Some(thread) = session.thread.upgrade() {
                return Task::ready(Ok(thread));
            }
        }

        let Some(cwd) = work_dirs.ordered_paths().next().cloned() else {
            return Task::ready(Err(anyhow!("Working directory cannot be empty")));
        };

        let shared_task = cx
            .spawn({
                let session_id = session_id.clone();
                let this = self.clone();
                async move |cx| {
                    let action_log = cx.new(|_| ActionLog::new(project.clone()));
                    let thread: Entity<AcpThread> = cx.new(|cx| {
                        AcpThread::new(
                            None,
                            title.unwrap_or_else(|| this.display_name.clone()),
                            Some(work_dirs),
                            this.clone(),
                            project,
                            action_log,
                            session_id.clone(),
                            watch::Receiver::constant(
                                this.agent_capabilities.prompt_capabilities.clone(),
                            ),
                            cx,
                        )
                    });

                    this.sessions.borrow_mut().insert(
                        session_id.clone(),
                        AcpSession {
                            thread: thread.downgrade(),
                            suppress_abort_err: false,
                            session_modes: None,
                            models: None,
                            config_options: None,
                            ref_count: 1,
                        },
                    );

                    let response =
                        match rpc_call(this.connection.clone(), session_id.clone(), cwd).await {
                            Ok(response) => response,
                            Err(err) => {
                                this.sessions.borrow_mut().remove(&session_id);
                                this.pending_sessions.borrow_mut().remove(&session_id);
                                return Err(Arc::new(err));
                            }
                        };

                    let (modes, models, config_options) =
                        config_state(response.modes, response.models, response.config_options);

                    if let Some(config_opts) = config_options.as_ref() {
                        this.apply_default_config_options(&session_id, config_opts, cx);
                    }

                    let ref_count = this
                        .pending_sessions
                        .borrow_mut()
                        .remove(&session_id)
                        .map_or(1, |pending| pending.ref_count);

                    {
                        let mut sessions = this.sessions.borrow_mut();
                        let Some(session) = sessions.get_mut(&session_id) else {
                            return Err(Arc::new(anyhow!(
                                "session was closed before load completed"
                            )));
                        };
                        session.session_modes = modes;
                        session.models = models;
                        session.config_options = config_options.map(ConfigOptions::new);
                        session.ref_count = ref_count;
                    }

                    Ok(thread)
                }
            })
            .shared();

        self.pending_sessions.borrow_mut().insert(
            session_id,
            PendingAcpSession {
                task: shared_task.clone(),
                ref_count: 1,
            },
        );

        cx.foreground_executor()
            .spawn(async move { shared_task.await.map_err(|err| anyhow!(err)) })
    }

    fn apply_default_config_options(
        &self,
        session_id: &acp::SessionId,
        config_options: &Rc<RefCell<Vec<acp::SessionConfigOption>>>,
        cx: &mut AsyncApp,
    ) {
        let id = self.id.clone();
        let defaults_to_apply: Vec<_> = {
            let config_opts_ref = config_options.borrow();
            config_opts_ref
                .iter()
                .filter_map(|config_option| {
                    let default_value = self.default_config_options.get(&*config_option.id.0)?;

                    let is_valid = match &config_option.kind {
                        acp::SessionConfigKind::Select(select) => match &select.options {
                            acp::SessionConfigSelectOptions::Ungrouped(options) => options
                                .iter()
                                .any(|opt| &*opt.value.0 == default_value.as_str()),
                            acp::SessionConfigSelectOptions::Grouped(groups) => {
                                groups.iter().any(|g| {
                                    g.options
                                        .iter()
                                        .any(|opt| &*opt.value.0 == default_value.as_str())
                                })
                            }
                            _ => false,
                        },
                        _ => false,
                    };

                    if is_valid {
                        let initial_value = match &config_option.kind {
                            acp::SessionConfigKind::Select(select) => {
                                Some(select.current_value.clone())
                            }
                            _ => None,
                        };
                        Some((
                            config_option.id.clone(),
                            default_value.clone(),
                            initial_value,
                        ))
                    } else {
                        log::warn!(
                            "`{}` is not a valid value for config option `{}` in {}",
                            default_value,
                            config_option.id.0,
                            id
                        );
                        None
                    }
                })
                .collect()
        };

        for (config_id, default_value, initial_value) in defaults_to_apply {
            cx.spawn({
                let default_value_id = acp::SessionConfigValueId::new(default_value.clone());
                let session_id = session_id.clone();
                let config_id_clone = config_id.clone();
                let config_opts = config_options.clone();
                let conn = self.connection.clone();
                async move |_| {
                    let result = conn
                        .set_session_config_option(acp::SetSessionConfigOptionRequest::new(
                            session_id,
                            config_id_clone.clone(),
                            default_value_id,
                        ))
                        .await
                        .log_err();

                    if result.is_none() {
                        if let Some(initial) = initial_value {
                            let mut opts = config_opts.borrow_mut();
                            if let Some(opt) = opts.iter_mut().find(|o| o.id == config_id_clone) {
                                if let acp::SessionConfigKind::Select(select) = &mut opt.kind {
                                    select.current_value = initial;
                                }
                            }
                        }
                    }
                }
            })
            .detach();

            let mut opts = config_options.borrow_mut();
            if let Some(opt) = opts.iter_mut().find(|o| o.id == config_id) {
                if let acp::SessionConfigKind::Select(select) = &mut opt.kind {
                    select.current_value = acp::SessionConfigValueId::new(default_value);
                }
            }
        }
    }
}

fn emit_load_error_to_all_sessions(
    sessions: &Rc<RefCell<HashMap<acp::SessionId, AcpSession>>>,
    error: LoadError,
    cx: &mut AsyncApp,
) {
    let threads = sessions
        .borrow()
        .values()
        .map(|session| session.thread.clone())
        .collect::<Vec<_>>();

    for thread in threads {
        thread
            .update(cx, |thread, cx| thread.emit_load_error(error.clone(), cx))
            .ok();
    }
}

impl Drop for AcpConnection {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            child.kill().log_err();
        }
    }
}

impl AgentConnection for AcpConnection {
    fn agent_id(&self) -> AgentId {
        self.id.clone()
    }

    fn telemetry_id(&self) -> SharedString {
        self.telemetry_id.clone()
    }

    fn new_session(
        self: Rc<Self>,
        project: Entity<Project>,
        work_dirs: PathList,
        cx: &mut App,
    ) -> Task<Result<Entity<AcpThread>>> {
        // TODO: remove this once ACP supports multiple working directories
        let Some(cwd) = work_dirs.ordered_paths().next().cloned() else {
            return Task::ready(Err(anyhow!("Working directory cannot be empty")));
        };
        let name = self.id.0.clone();
        let mcp_servers = mcp_servers_for_project(&project, cx);

        cx.spawn(async move |cx| {
            let response = self.connection
                .new_session(acp::NewSessionRequest::new(cwd.clone()).mcp_servers(mcp_servers))
                .await
                .map_err(map_acp_error)?;

            let (modes, models, config_options) = config_state(response.modes, response.models, response.config_options);

            if let Some(default_mode) = self.default_mode.clone() {
                if let Some(modes) = modes.as_ref() {
                    let mut modes_ref = modes.borrow_mut();
                    let has_mode = modes_ref.available_modes.iter().any(|mode| mode.id == default_mode);

                    if has_mode {
                        let initial_mode_id = modes_ref.current_mode_id.clone();

                        cx.spawn({
                            let default_mode = default_mode.clone();
                            let session_id = response.session_id.clone();
                            let modes = modes.clone();
                            let conn = self.connection.clone();
                            async move |_| {
                                let result = conn.set_session_mode(acp::SetSessionModeRequest::new(session_id, default_mode))
                                .await.log_err();

                                if result.is_none() {
                                    modes.borrow_mut().current_mode_id = initial_mode_id;
                                }
                            }
                        }).detach();

                        modes_ref.current_mode_id = default_mode;
                    } else {
                        let available_modes = modes_ref
                            .available_modes
                            .iter()
                            .map(|mode| format!("- `{}`: {}", mode.id, mode.name))
                            .collect::<Vec<_>>()
                            .join("\n");

                        log::warn!(
                            "`{default_mode}` is not valid {name} mode. Available options:\n{available_modes}",
                        );
                    }
                }
            }

            if let Some(default_model) = self.default_model.clone() {
                if let Some(models) = models.as_ref() {
                    let mut models_ref = models.borrow_mut();
                    let has_model = models_ref.available_models.iter().any(|model| model.model_id == default_model);

                    if has_model {
                        let initial_model_id = models_ref.current_model_id.clone();

                        cx.spawn({
                            let default_model = default_model.clone();
                            let session_id = response.session_id.clone();
                            let models = models.clone();
                            let conn = self.connection.clone();
                            async move |_| {
                                let result = conn.set_session_model(acp::SetSessionModelRequest::new(session_id, default_model))
                                .await.log_err();

                                if result.is_none() {
                                    models.borrow_mut().current_model_id = initial_model_id;
                                }
                            }
                        }).detach();

                        models_ref.current_model_id = default_model;
                    } else {
                        let available_models = models_ref
                            .available_models
                            .iter()
                            .map(|model| format!("- `{}`: {}", model.model_id, model.name))
                            .collect::<Vec<_>>()
                            .join("\n");

                        log::warn!(
                            "`{default_model}` is not a valid {name} model. Available options:\n{available_models}",
                        );
                    }
                }
            }

            if let Some(config_opts) = config_options.as_ref() {
                self.apply_default_config_options(&response.session_id, config_opts, cx);
            }

            let action_log = cx.new(|_| ActionLog::new(project.clone()));
            let thread: Entity<AcpThread> = cx.new(|cx| {
                AcpThread::new(
                    None,
                    self.display_name.clone(),
                    Some(work_dirs),
                    self.clone(),
                    project,
                    action_log,
                    response.session_id.clone(),
                    // ACP doesn't currently support per-session prompt capabilities or changing capabilities dynamically.
                    watch::Receiver::constant(self.agent_capabilities.prompt_capabilities.clone()),
                    cx,
                )
            });

            self.sessions.borrow_mut().insert(
                response.session_id,
                AcpSession {
                    thread: thread.downgrade(),
                    suppress_abort_err: false,
                    session_modes: modes,
                    models,
                    config_options: config_options.map(ConfigOptions::new),
                    ref_count: 1,
                },
            );

            Ok(thread)
        })
    }

    fn supports_load_session(&self) -> bool {
        self.agent_capabilities.load_session
    }

    fn supports_resume_session(&self) -> bool {
        self.agent_capabilities
            .session_capabilities
            .resume
            .is_some()
    }

    fn load_session(
        self: Rc<Self>,
        session_id: acp::SessionId,
        project: Entity<Project>,
        work_dirs: PathList,
        title: Option<SharedString>,
        cx: &mut App,
    ) -> Task<Result<Entity<AcpThread>>> {
        if !self.agent_capabilities.load_session {
            return Task::ready(Err(anyhow!(LoadError::Other(
                "Loading sessions is not supported by this agent.".into()
            ))));
        }
        let mcp_servers = mcp_servers_for_project(&project, cx);
        self.open_or_create_session(
            session_id,
            project,
            work_dirs,
            title,
            move |connection, session_id, cwd| {
                Box::pin(async move {
                    let response = connection
                        .load_session(
                            acp::LoadSessionRequest::new(session_id, cwd).mcp_servers(mcp_servers),
                        )
                        .await
                        .map_err(map_acp_error)?;
                    Ok(SessionConfigResponse {
                        modes: response.modes,
                        models: response.models,
                        config_options: response.config_options,
                    })
                })
            },
            cx,
        )
    }

    fn resume_session(
        self: Rc<Self>,
        session_id: acp::SessionId,
        project: Entity<Project>,
        work_dirs: PathList,
        title: Option<SharedString>,
        cx: &mut App,
    ) -> Task<Result<Entity<AcpThread>>> {
        if self
            .agent_capabilities
            .session_capabilities
            .resume
            .is_none()
        {
            return Task::ready(Err(anyhow!(LoadError::Other(
                "Resuming sessions is not supported by this agent.".into()
            ))));
        }
        let mcp_servers = mcp_servers_for_project(&project, cx);
        self.open_or_create_session(
            session_id,
            project,
            work_dirs,
            title,
            move |connection, session_id, cwd| {
                Box::pin(async move {
                    let response = connection
                        .resume_session(
                            acp::ResumeSessionRequest::new(session_id, cwd)
                                .mcp_servers(mcp_servers),
                        )
                        .await
                        .map_err(map_acp_error)?;
                    Ok(SessionConfigResponse {
                        modes: response.modes,
                        models: response.models,
                        config_options: response.config_options,
                    })
                })
            },
            cx,
        )
    }

    fn supports_close_session(&self) -> bool {
        self.agent_capabilities.session_capabilities.close.is_some()
    }

    fn close_session(
        self: Rc<Self>,
        session_id: &acp::SessionId,
        cx: &mut App,
    ) -> Task<Result<()>> {
        if !self.supports_close_session() {
            return Task::ready(Err(anyhow!(LoadError::Other(
                "Closing sessions is not supported by this agent.".into()
            ))));
        }

        let pending_ref_count = {
            let mut pending_sessions = self.pending_sessions.borrow_mut();
            pending_sessions.get_mut(session_id).map(|pending| {
                pending.ref_count = pending.ref_count.saturating_sub(1);
                pending.ref_count
            })
        };
        match pending_ref_count {
            Some(0) => {
                self.pending_sessions.borrow_mut().remove(session_id);
                self.sessions.borrow_mut().remove(session_id);

                let conn = self.connection.clone();
                let session_id = session_id.clone();
                return cx.foreground_executor().spawn(async move {
                    conn.close_session(acp::CloseSessionRequest::new(session_id))
                        .await?;
                    Ok(())
                });
            }
            Some(_) => return Task::ready(Ok(())),
            None => {}
        }

        let mut sessions = self.sessions.borrow_mut();
        let Some(session) = sessions.get_mut(session_id) else {
            return Task::ready(Ok(()));
        };

        session.ref_count = session.ref_count.saturating_sub(1);
        if session.ref_count > 0 {
            return Task::ready(Ok(()));
        }

        sessions.remove(session_id);
        drop(sessions);

        let conn = self.connection.clone();
        let session_id = session_id.clone();
        cx.foreground_executor().spawn(async move {
            conn.close_session(acp::CloseSessionRequest::new(session_id))
                .await?;
            Ok(())
        })
    }

    fn auth_methods(&self) -> &[acp::AuthMethod] {
        &self.auth_methods
    }

    fn authenticate(&self, method_id: acp::AuthMethodId, cx: &mut App) -> Task<Result<()>> {
        let conn = self.connection.clone();
        cx.foreground_executor().spawn(async move {
            conn.authenticate(acp::AuthenticateRequest::new(method_id))
                .await?;
            Ok(())
        })
    }

    fn prompt(
        &self,
        _id: Option<acp_thread::UserMessageId>,
        params: acp::PromptRequest,
        cx: &mut App,
    ) -> Task<Result<acp::PromptResponse>> {
        let conn = self.connection.clone();
        let sessions = self.sessions.clone();
        let session_id = params.session_id.clone();
        cx.foreground_executor().spawn(async move {
            let result = conn.prompt(params).await;

            let mut suppress_abort_err = false;

            if let Some(session) = sessions.borrow_mut().get_mut(&session_id) {
                suppress_abort_err = session.suppress_abort_err;
                session.suppress_abort_err = false;
            }

            match result {
                Ok(response) => Ok(response),
                Err(err) => {
                    if err.code == acp::ErrorCode::AuthRequired {
                        return Err(anyhow!(acp::Error::auth_required()));
                    }

                    if err.code != ErrorCode::InternalError {
                        anyhow::bail!(err)
                    }

                    let Some(data) = &err.data else {
                        anyhow::bail!(err)
                    };

                    // Temporary workaround until the following PR is generally available:
                    // https://github.com/google-gemini/gemini-cli/pull/6656

                    #[derive(Deserialize)]
                    #[serde(deny_unknown_fields)]
                    struct ErrorDetails {
                        details: Box<str>,
                    }

                    match serde_json::from_value(data.clone()) {
                        Ok(ErrorDetails { details }) => {
                            if suppress_abort_err
                                && (details.contains("This operation was aborted")
                                    || details.contains("The user aborted a request"))
                            {
                                Ok(acp::PromptResponse::new(acp::StopReason::Cancelled))
                            } else {
                                Err(anyhow!(details))
                            }
                        }
                        Err(_) => Err(anyhow!(err)),
                    }
                }
            }
        })
    }

    fn cancel(&self, session_id: &acp::SessionId, cx: &mut App) {
        if let Some(session) = self.sessions.borrow_mut().get_mut(session_id) {
            session.suppress_abort_err = true;
        }
        let conn = self.connection.clone();
        let params = acp::CancelNotification::new(session_id.clone());
        cx.foreground_executor()
            .spawn(async move { conn.cancel(params).await })
            .detach();
    }

    fn session_modes(
        &self,
        session_id: &acp::SessionId,
        _cx: &App,
    ) -> Option<Rc<dyn acp_thread::AgentSessionModes>> {
        let sessions = self.sessions.clone();
        let sessions_ref = sessions.borrow();
        let Some(session) = sessions_ref.get(session_id) else {
            return None;
        };

        if let Some(modes) = session.session_modes.as_ref() {
            Some(Rc::new(AcpSessionModes {
                connection: self.connection.clone(),
                session_id: session_id.clone(),
                state: modes.clone(),
            }) as _)
        } else {
            None
        }
    }

    fn model_selector(
        &self,
        session_id: &acp::SessionId,
    ) -> Option<Rc<dyn acp_thread::AgentModelSelector>> {
        let sessions = self.sessions.clone();
        let sessions_ref = sessions.borrow();
        let Some(session) = sessions_ref.get(session_id) else {
            return None;
        };

        if let Some(models) = session.models.as_ref() {
            Some(Rc::new(AcpModelSelector::new(
                session_id.clone(),
                self.connection.clone(),
                models.clone(),
            )) as _)
        } else {
            None
        }
    }

    fn session_config_options(
        &self,
        session_id: &acp::SessionId,
        _cx: &App,
    ) -> Option<Rc<dyn acp_thread::AgentSessionConfigOptions>> {
        let sessions = self.sessions.borrow();
        let session = sessions.get(session_id)?;

        let config_opts = session.config_options.as_ref()?;

        Some(Rc::new(AcpSessionConfigOptions {
            session_id: session_id.clone(),
            connection: self.connection.clone(),
            state: config_opts.config_options.clone(),
            watch_tx: config_opts.tx.clone(),
            watch_rx: config_opts.rx.clone(),
        }) as _)
    }

    fn session_list(&self, _cx: &mut App) -> Option<Rc<dyn AgentSessionList>> {
        self.session_list.clone().map(|s| s as _)
    }

    fn into_any(self: Rc<Self>) -> Rc<dyn Any> {
        self
    }
}

fn map_acp_error(err: acp::Error) -> anyhow::Error {
    if err.code == acp::ErrorCode::AuthRequired {
        let mut error = AuthRequired::new();

        if err.message != acp::ErrorCode::AuthRequired.to_string() {
            error = error.with_description(err.message);
        }

        anyhow!(error)
    } else {
        anyhow!(err)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    struct FakeAcpAgent {
        load_session_count: Arc<AtomicUsize>,
        close_session_count: Arc<AtomicUsize>,
        load_session_updates: Rc<RefCell<Vec<acp::SessionUpdate>>>,
        load_session_gate: Rc<RefCell<Option<smol::channel::Receiver<()>>>>,
        client: Rc<RefCell<Option<Rc<acp::AgentSideConnection>>>>,
    }

    #[async_trait::async_trait(?Send)]
    impl acp::Agent for FakeAcpAgent {
        async fn initialize(
            &self,
            args: acp::InitializeRequest,
        ) -> acp::Result<acp::InitializeResponse> {
            Ok(
                acp::InitializeResponse::new(args.protocol_version).agent_capabilities(
                    acp::AgentCapabilities::default()
                        .load_session(true)
                        .session_capabilities(
                            acp::SessionCapabilities::default()
                                .close(acp::SessionCloseCapabilities::new()),
                        ),
                ),
            )
        }

        async fn authenticate(
            &self,
            _: acp::AuthenticateRequest,
        ) -> acp::Result<acp::AuthenticateResponse> {
            Ok(Default::default())
        }

        async fn new_session(
            &self,
            _: acp::NewSessionRequest,
        ) -> acp::Result<acp::NewSessionResponse> {
            Ok(acp::NewSessionResponse::new(acp::SessionId::new("unused")))
        }

        async fn prompt(&self, _: acp::PromptRequest) -> acp::Result<acp::PromptResponse> {
            Ok(acp::PromptResponse::new(acp::StopReason::EndTurn))
        }

        async fn cancel(&self, _: acp::CancelNotification) -> acp::Result<()> {
            Ok(())
        }

        async fn load_session(
            &self,
            args: acp::LoadSessionRequest,
        ) -> acp::Result<acp::LoadSessionResponse> {
            self.load_session_count.fetch_add(1, Ordering::SeqCst);

            let updates = std::mem::take(&mut *self.load_session_updates.borrow_mut());
            if !updates.is_empty() {
                let client = self
                    .client
                    .borrow()
                    .clone()
                    .expect("client should be set before load_session is called");
                for update in updates {
                    use acp::Client as _;
                    client
                        .session_notification(acp::SessionNotification::new(
                            args.session_id.clone(),
                            update,
                        ))
                        .await?;
                }
            }

            let gate = self.load_session_gate.borrow_mut().take();
            if let Some(gate) = gate {
                gate.recv().await.ok();
            }

            Ok(acp::LoadSessionResponse::new())
        }

        async fn close_session(
            &self,
            _: acp::CloseSessionRequest,
        ) -> acp::Result<acp::CloseSessionResponse> {
            self.close_session_count.fetch_add(1, Ordering::SeqCst);
            Ok(acp::CloseSessionResponse::new())
        }
    }

    async fn connect_fake_agent(
        cx: &mut gpui::TestAppContext,
    ) -> (
        Rc<AcpConnection>,
        Entity<Project>,
        Arc<AtomicUsize>,
        Arc<AtomicUsize>,
        Rc<RefCell<Vec<acp::SessionUpdate>>>,
        Rc<RefCell<Option<smol::channel::Receiver<()>>>>,
        Task<anyhow::Result<()>>,
    ) {
        cx.update(|cx| {
            let store = settings::SettingsStore::test(cx);
            cx.set_global(store);
        });

        let fs = fs::FakeFs::new(cx.executor());
        fs.insert_tree("/", serde_json::json!({ "a": {} })).await;
        let project = Project::test(fs, [std::path::Path::new("/a")], cx).await;

        let load_count = Arc::new(AtomicUsize::new(0));
        let close_count = Arc::new(AtomicUsize::new(0));
        let load_session_updates = Rc::new(RefCell::new(Vec::new()));
        let load_session_gate = Rc::new(RefCell::new(None));
        let agent_client = Rc::new(RefCell::new(None));

        let (client_to_agent_writer, client_to_agent_reader) = async_pipe::pipe();
        let (agent_to_client_writer, agent_to_client_reader) = async_pipe::pipe();

        let sessions = Rc::new(RefCell::new(HashMap::default()));
        let session_list_container = Rc::new(RefCell::new(None));
        let foreground = cx.foreground_executor().clone();

        let client_delegate = ClientDelegate {
            sessions: sessions.clone(),
            session_list: session_list_container,
            cx: cx.to_async(),
        };

        let (client_connection, client_io_task) = acp::ClientSideConnection::new(
            client_delegate,
            client_to_agent_writer,
            agent_to_client_reader,
            {
                let foreground = foreground.clone();
                move |future| {
                    foreground.spawn(future).detach();
                }
            },
        );

        let fake_agent = FakeAcpAgent {
            load_session_count: load_count.clone(),
            close_session_count: close_count.clone(),
            load_session_updates: load_session_updates.clone(),
            load_session_gate: load_session_gate.clone(),
            client: agent_client.clone(),
        };

        let (agent_connection, agent_io_task) = acp::AgentSideConnection::new(
            fake_agent,
            agent_to_client_writer,
            client_to_agent_reader,
            {
                let foreground = foreground.clone();
                move |future| {
                    foreground.spawn(future).detach();
                }
            },
        );
        *agent_client.borrow_mut() = Some(Rc::new(agent_connection));

        let client_io_task = cx.background_spawn(client_io_task);
        let agent_io_task = cx.background_spawn(agent_io_task);

        let response = client_connection
            .initialize(acp::InitializeRequest::new(acp::ProtocolVersion::V1))
            .await
            .expect("failed to initialize ACP connection");

        let connection = AcpConnection {
            id: AgentId::new("test"),
            display_name: "Test".into(),
            telemetry_id: "test".into(),
            connection: Rc::new(client_connection),
            sessions,
            pending_sessions: Rc::new(RefCell::new(HashMap::default())),
            auth_methods: vec![],
            agent_capabilities: response.agent_capabilities,
            default_mode: None,
            default_model: None,
            default_config_options: HashMap::default(),
            child: None,
            session_list: None,
            _io_task: client_io_task,
            _wait_task: Task::ready(Ok(())),
            _stderr_task: Task::ready(Ok(())),
        };

        let keep_agent_alive = cx.background_spawn(async move {
            agent_io_task.await.ok();
            anyhow::Ok(())
        });

        (
            Rc::new(connection),
            project,
            load_count,
            close_count,
            load_session_updates,
            load_session_gate,
            keep_agent_alive,
        )
    }

    #[gpui::test]
    async fn test_loaded_sessions_keep_state_until_last_close(cx: &mut gpui::TestAppContext) {
        let (
            connection,
            project,
            load_count,
            close_count,
            _load_session_updates,
            _load_session_gate,
            _keep_agent_alive,
        ) = connect_fake_agent(cx).await;

        let session_id = acp::SessionId::new("session-1");
        let work_dirs = PathList::new(&[std::path::Path::new("/a")]);

        let first_load = cx.update(|cx| {
            connection.clone().load_session(
                session_id.clone(),
                project.clone(),
                work_dirs.clone(),
                None,
                cx,
            )
        });
        let second_load = cx.update(|cx| {
            connection.clone().load_session(
                session_id.clone(),
                project.clone(),
                work_dirs.clone(),
                None,
                cx,
            )
        });

        let first_thread = first_load.await.expect("first load failed");
        let second_thread = second_load.await.expect("second load failed");
        cx.run_until_parked();

        assert_eq!(first_thread.entity_id(), second_thread.entity_id());
        assert_eq!(load_count.load(Ordering::SeqCst), 1);

        cx.update(|cx| connection.clone().close_session(&session_id, cx))
            .await
            .expect("first close failed");

        assert_eq!(close_count.load(Ordering::SeqCst), 0);
        assert!(connection.sessions.borrow().contains_key(&session_id));

        cx.update(|cx| connection.clone().close_session(&session_id, cx))
            .await
            .expect("second close failed");
        cx.run_until_parked();

        assert_eq!(close_count.load(Ordering::SeqCst), 1);
        assert!(!connection.sessions.borrow().contains_key(&session_id));
    }

    #[gpui::test]
    async fn test_load_session_replays_notifications_sent_before_response(
        cx: &mut gpui::TestAppContext,
    ) {
        let (
            connection,
            project,
            _load_count,
            _close_count,
            load_session_updates,
            _load_session_gate,
            _keep_agent_alive,
        ) = connect_fake_agent(cx).await;

        *load_session_updates.borrow_mut() = vec![
            acp::SessionUpdate::UserMessageChunk(acp::ContentChunk::new("hello agent".into())),
            acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new("hi user".into())),
        ];

        let session_id = acp::SessionId::new("session-replay");
        let work_dirs = PathList::new(&[std::path::Path::new("/a")]);

        let thread = cx
            .update(|cx| {
                connection.clone().load_session(
                    session_id.clone(),
                    project.clone(),
                    work_dirs,
                    None,
                    cx,
                )
            })
            .await
            .expect("load_session failed");
        cx.run_until_parked();

        let entries = thread.read_with(cx, |thread, _| {
            thread
                .entries()
                .iter()
                .map(|entry| match entry {
                    acp_thread::AgentThreadEntry::UserMessage(_) => "user",
                    acp_thread::AgentThreadEntry::AssistantMessage(_) => "assistant",
                    acp_thread::AgentThreadEntry::ToolCall(_) => "tool_call",
                })
                .collect::<Vec<_>>()
        });

        assert_eq!(entries, vec!["user", "assistant"]);
    }

    #[gpui::test]
    async fn test_close_session_during_in_flight_load(cx: &mut gpui::TestAppContext) {
        let (
            connection,
            project,
            load_count,
            close_count,
            _load_session_updates,
            load_session_gate,
            _keep_agent_alive,
        ) = connect_fake_agent(cx).await;

        let (gate_tx, gate_rx) = smol::channel::bounded::<()>(1);
        *load_session_gate.borrow_mut() = Some(gate_rx);

        let session_id = acp::SessionId::new("session-close-during-load");
        let work_dirs = PathList::new(&[std::path::Path::new("/a")]);

        let load_task = cx.update(|cx| {
            connection.clone().load_session(
                session_id.clone(),
                project.clone(),
                work_dirs,
                None,
                cx,
            )
        });

        cx.run_until_parked();
        assert_eq!(load_count.load(Ordering::SeqCst), 1);
        assert!(
            connection
                .pending_sessions
                .borrow()
                .contains_key(&session_id)
        );
        assert!(connection.sessions.borrow().contains_key(&session_id));

        let close_task = cx.update(|cx| connection.clone().close_session(&session_id, cx));
        gate_tx.send(()).await.expect("gate send failed");
        drop(gate_tx);

        let load_result = load_task.await;
        close_task.await.expect("close failed");
        cx.run_until_parked();

        let err = load_result.expect_err("load should fail after close-during-load");
        assert!(
            err.to_string()
                .contains("session was closed before load completed"),
            "expected close-during-load error, got: {err}"
        );
        assert_eq!(close_count.load(Ordering::SeqCst), 1);
        assert!(!connection.sessions.borrow().contains_key(&session_id));
        assert!(
            !connection
                .pending_sessions
                .borrow()
                .contains_key(&session_id)
        );
    }

    #[gpui::test]
    async fn test_close_during_load_preserves_other_concurrent_loader(
        cx: &mut gpui::TestAppContext,
    ) {
        let (
            connection,
            project,
            load_count,
            close_count,
            _load_session_updates,
            load_session_gate,
            _keep_agent_alive,
        ) = connect_fake_agent(cx).await;

        let (gate_tx, gate_rx) = smol::channel::bounded::<()>(1);
        *load_session_gate.borrow_mut() = Some(gate_rx);

        let session_id = acp::SessionId::new("session-concurrent-close");
        let work_dirs = PathList::new(&[std::path::Path::new("/a")]);

        let first_load = cx.update(|cx| {
            connection.clone().load_session(
                session_id.clone(),
                project.clone(),
                work_dirs.clone(),
                None,
                cx,
            )
        });
        let second_load = cx.update(|cx| {
            connection.clone().load_session(
                session_id.clone(),
                project.clone(),
                work_dirs.clone(),
                None,
                cx,
            )
        });

        cx.run_until_parked();
        assert_eq!(load_count.load(Ordering::SeqCst), 1);

        cx.update(|cx| connection.clone().close_session(&session_id, cx))
            .await
            .expect("close during load failed");
        assert_eq!(close_count.load(Ordering::SeqCst), 0);

        gate_tx.send(()).await.expect("gate send failed");
        drop(gate_tx);

        let first_thread = first_load.await.expect("first load should still succeed");
        let second_thread = second_load.await.expect("second load should still succeed");
        cx.run_until_parked();

        assert_eq!(first_thread.entity_id(), second_thread.entity_id());
        assert!(connection.sessions.borrow().contains_key(&session_id));
        assert!(
            !connection
                .pending_sessions
                .borrow()
                .contains_key(&session_id)
        );

        cx.update(|cx| connection.clone().close_session(&session_id, cx))
            .await
            .expect("final close failed");
        cx.run_until_parked();

        assert_eq!(close_count.load(Ordering::SeqCst), 1);
        assert!(!connection.sessions.borrow().contains_key(&session_id));
    }
}

fn mcp_servers_for_project(project: &Entity<Project>, cx: &App) -> Vec<acp::McpServer> {
    let context_server_store = project.read(cx).context_server_store().read(cx);
    let is_local = project.read(cx).is_local();
    context_server_store
        .configured_server_ids()
        .iter()
        .filter_map(|id| {
            let configuration = context_server_store.configuration_for_server(id)?;
            match &*configuration {
                project::context_server_store::ContextServerConfiguration::Custom {
                    command,
                    remote,
                    ..
                }
                | project::context_server_store::ContextServerConfiguration::Extension {
                    command,
                    remote,
                    ..
                } if is_local || *remote => Some(acp::McpServer::Stdio(
                    acp::McpServerStdio::new(id.0.to_string(), &command.path)
                        .args(command.args.clone())
                        .env(if let Some(env) = command.env.as_ref() {
                            env.iter()
                                .map(|(name, value)| acp::EnvVariable::new(name, value))
                                .collect()
                        } else {
                            vec![]
                        }),
                )),
                project::context_server_store::ContextServerConfiguration::Http {
                    url,
                    headers,
                    timeout: _,
                } => Some(acp::McpServer::Http(
                    acp::McpServerHttp::new(id.0.to_string(), url.to_string()).headers(
                        headers
                            .iter()
                            .map(|(name, value)| acp::HttpHeader::new(name, value))
                            .collect(),
                    ),
                )),
                _ => None,
            }
        })
        .collect()
}

fn config_state(
    modes: Option<acp::SessionModeState>,
    models: Option<acp::SessionModelState>,
    config_options: Option<Vec<acp::SessionConfigOption>>,
) -> (
    Option<Rc<RefCell<acp::SessionModeState>>>,
    Option<Rc<RefCell<acp::SessionModelState>>>,
    Option<Rc<RefCell<Vec<acp::SessionConfigOption>>>>,
) {
    if let Some(opts) = config_options {
        return (None, None, Some(Rc::new(RefCell::new(opts))));
    }

    let modes = modes.map(|modes| Rc::new(RefCell::new(modes)));
    let models = models.map(|models| Rc::new(RefCell::new(models)));
    (modes, models, None)
}

struct AcpSessionModes {
    session_id: acp::SessionId,
    connection: Rc<acp::ClientSideConnection>,
    state: Rc<RefCell<acp::SessionModeState>>,
}

impl acp_thread::AgentSessionModes for AcpSessionModes {
    fn current_mode(&self) -> acp::SessionModeId {
        self.state.borrow().current_mode_id.clone()
    }

    fn all_modes(&self) -> Vec<acp::SessionMode> {
        self.state.borrow().available_modes.clone()
    }

    fn set_mode(&self, mode_id: acp::SessionModeId, cx: &mut App) -> Task<Result<()>> {
        let connection = self.connection.clone();
        let session_id = self.session_id.clone();
        let old_mode_id;
        {
            let mut state = self.state.borrow_mut();
            old_mode_id = state.current_mode_id.clone();
            state.current_mode_id = mode_id.clone();
        };
        let state = self.state.clone();
        cx.foreground_executor().spawn(async move {
            let result = connection
                .set_session_mode(acp::SetSessionModeRequest::new(session_id, mode_id))
                .await;

            if result.is_err() {
                state.borrow_mut().current_mode_id = old_mode_id;
            }

            result?;

            Ok(())
        })
    }
}

struct AcpModelSelector {
    session_id: acp::SessionId,
    connection: Rc<acp::ClientSideConnection>,
    state: Rc<RefCell<acp::SessionModelState>>,
}

impl AcpModelSelector {
    fn new(
        session_id: acp::SessionId,
        connection: Rc<acp::ClientSideConnection>,
        state: Rc<RefCell<acp::SessionModelState>>,
    ) -> Self {
        Self {
            session_id,
            connection,
            state,
        }
    }
}

impl acp_thread::AgentModelSelector for AcpModelSelector {
    fn list_models(&self, _cx: &mut App) -> Task<Result<acp_thread::AgentModelList>> {
        Task::ready(Ok(acp_thread::AgentModelList::Flat(
            self.state
                .borrow()
                .available_models
                .clone()
                .into_iter()
                .map(acp_thread::AgentModelInfo::from)
                .collect(),
        )))
    }

    fn select_model(&self, model_id: acp::ModelId, cx: &mut App) -> Task<Result<()>> {
        let connection = self.connection.clone();
        let session_id = self.session_id.clone();
        let old_model_id;
        {
            let mut state = self.state.borrow_mut();
            old_model_id = state.current_model_id.clone();
            state.current_model_id = model_id.clone();
        };
        let state = self.state.clone();
        cx.foreground_executor().spawn(async move {
            let result = connection
                .set_session_model(acp::SetSessionModelRequest::new(session_id, model_id))
                .await;

            if result.is_err() {
                state.borrow_mut().current_model_id = old_model_id;
            }

            result?;

            Ok(())
        })
    }

    fn selected_model(&self, _cx: &mut App) -> Task<Result<acp_thread::AgentModelInfo>> {
        let state = self.state.borrow();
        Task::ready(
            state
                .available_models
                .iter()
                .find(|m| m.model_id == state.current_model_id)
                .cloned()
                .map(acp_thread::AgentModelInfo::from)
                .ok_or_else(|| anyhow::anyhow!("Model not found")),
        )
    }
}

struct AcpSessionConfigOptions {
    session_id: acp::SessionId,
    connection: Rc<acp::ClientSideConnection>,
    state: Rc<RefCell<Vec<acp::SessionConfigOption>>>,
    watch_tx: Rc<RefCell<watch::Sender<()>>>,
    watch_rx: watch::Receiver<()>,
}

impl acp_thread::AgentSessionConfigOptions for AcpSessionConfigOptions {
    fn config_options(&self) -> Vec<acp::SessionConfigOption> {
        self.state.borrow().clone()
    }

    fn set_config_option(
        &self,
        config_id: acp::SessionConfigId,
        value: acp::SessionConfigValueId,
        cx: &mut App,
    ) -> Task<Result<Vec<acp::SessionConfigOption>>> {
        let connection = self.connection.clone();
        let session_id = self.session_id.clone();
        let state = self.state.clone();

        let watch_tx = self.watch_tx.clone();

        cx.foreground_executor().spawn(async move {
            let response = connection
                .set_session_config_option(acp::SetSessionConfigOptionRequest::new(
                    session_id, config_id, value,
                ))
                .await?;

            *state.borrow_mut() = response.config_options.clone();
            watch_tx.borrow_mut().send(()).ok();
            Ok(response.config_options)
        })
    }

    fn watch(&self, _cx: &mut App) -> Option<watch::Receiver<()>> {
        Some(self.watch_rx.clone())
    }
}

struct ClientDelegate {
    sessions: Rc<RefCell<HashMap<acp::SessionId, AcpSession>>>,
    session_list: Rc<RefCell<Option<Rc<AcpSessionList>>>>,
    cx: AsyncApp,
}

#[async_trait::async_trait(?Send)]
impl acp::Client for ClientDelegate {
    async fn request_permission(
        &self,
        arguments: acp::RequestPermissionRequest,
    ) -> Result<acp::RequestPermissionResponse, acp::Error> {
        let thread;
        {
            let sessions_ref = self.sessions.borrow();
            let session = sessions_ref
                .get(&arguments.session_id)
                .context("Failed to get session")?;
            thread = session.thread.clone();
        }

        let cx = &mut self.cx.clone();

        let task = thread.update(cx, |thread, cx| {
            thread.request_tool_call_authorization(
                arguments.tool_call,
                acp_thread::PermissionOptions::Flat(arguments.options),
                cx,
            )
        })??;

        let outcome = task.await;

        Ok(acp::RequestPermissionResponse::new(outcome))
    }

    async fn write_text_file(
        &self,
        arguments: acp::WriteTextFileRequest,
    ) -> Result<acp::WriteTextFileResponse, acp::Error> {
        let cx = &mut self.cx.clone();
        let task = self
            .session_thread(&arguments.session_id)?
            .update(cx, |thread, cx| {
                thread.write_text_file(arguments.path, arguments.content, cx)
            })?;

        task.await?;

        Ok(Default::default())
    }

    async fn read_text_file(
        &self,
        arguments: acp::ReadTextFileRequest,
    ) -> Result<acp::ReadTextFileResponse, acp::Error> {
        let task = self.session_thread(&arguments.session_id)?.update(
            &mut self.cx.clone(),
            |thread, cx| {
                thread.read_text_file(arguments.path, arguments.line, arguments.limit, false, cx)
            },
        )?;

        let content = task.await?;

        Ok(acp::ReadTextFileResponse::new(content))
    }

    async fn session_notification(
        &self,
        notification: acp::SessionNotification,
    ) -> Result<(), acp::Error> {
        let (thread, session_modes, session_config_options) = {
            let sessions = self.sessions.borrow();
            let session = sessions
                .get(&notification.session_id)
                .context("Failed to get session")?;
            (
                session.thread.clone(),
                session.session_modes.clone(),
                session.config_options.clone(),
            )
        };

        if let acp::SessionUpdate::CurrentModeUpdate(acp::CurrentModeUpdate {
            current_mode_id,
            ..
        }) = &notification.update
        {
            if let Some(session_modes) = &session_modes {
                session_modes.borrow_mut().current_mode_id = current_mode_id.clone();
            }
        }

        if let acp::SessionUpdate::ConfigOptionUpdate(acp::ConfigOptionUpdate {
            config_options,
            ..
        }) = &notification.update
        {
            if let Some(opts) = &session_config_options {
                *opts.config_options.borrow_mut() = config_options.clone();
                opts.tx.borrow_mut().send(()).ok();
            }
        }

        if let acp::SessionUpdate::SessionInfoUpdate(info_update) = &notification.update
            && let Some(session_list) = self.session_list.borrow().as_ref()
        {
            session_list.send_info_update(notification.session_id.clone(), info_update.clone());
        }

        // Clone so we can inspect meta both before and after handing off to the thread
        let update_clone = notification.update.clone();

        // Pre-handle: if a ToolCall carries terminal_info, create/register a display-only terminal.
        if let acp::SessionUpdate::ToolCall(tc) = &update_clone {
            if let Some(meta) = &tc.meta {
                if let Some(terminal_info) = meta.get("terminal_info") {
                    if let Some(id_str) = terminal_info.get("terminal_id").and_then(|v| v.as_str())
                    {
                        let terminal_id = acp::TerminalId::new(id_str);
                        let cwd = terminal_info
                            .get("cwd")
                            .and_then(|v| v.as_str().map(PathBuf::from));

                        // Create a minimal display-only lower-level terminal and register it.
                        let _ = thread.update(&mut self.cx.clone(), |thread, cx| {
                            let builder = TerminalBuilder::new_display_only(
                                CursorShape::default(),
                                AlternateScroll::On,
                                None,
                                0,
                                cx.background_executor(),
                                thread.project().read(cx).path_style(cx),
                            )?;
                            let lower = cx.new(|cx| builder.subscribe(cx));
                            thread.on_terminal_provider_event(
                                TerminalProviderEvent::Created {
                                    terminal_id,
                                    label: tc.title.clone(),
                                    cwd,
                                    output_byte_limit: None,
                                    terminal: lower,
                                },
                                cx,
                            );
                            anyhow::Ok(())
                        });
                    }
                }
            }
        }

        // Forward the update to the acp_thread as usual.
        thread.update(&mut self.cx.clone(), |thread, cx| {
            thread.handle_session_update(notification.update.clone(), cx)
        })??;

        // Post-handle: stream terminal output/exit if present on ToolCallUpdate meta.
        if let acp::SessionUpdate::ToolCallUpdate(tcu) = &update_clone {
            if let Some(meta) = &tcu.meta {
                if let Some(term_out) = meta.get("terminal_output") {
                    if let Some(id_str) = term_out.get("terminal_id").and_then(|v| v.as_str()) {
                        let terminal_id = acp::TerminalId::new(id_str);
                        if let Some(s) = term_out.get("data").and_then(|v| v.as_str()) {
                            let data = s.as_bytes().to_vec();
                            let _ = thread.update(&mut self.cx.clone(), |thread, cx| {
                                thread.on_terminal_provider_event(
                                    TerminalProviderEvent::Output { terminal_id, data },
                                    cx,
                                );
                            });
                        }
                    }
                }

                // terminal_exit
                if let Some(term_exit) = meta.get("terminal_exit") {
                    if let Some(id_str) = term_exit.get("terminal_id").and_then(|v| v.as_str()) {
                        let terminal_id = acp::TerminalId::new(id_str);
                        let status = acp::TerminalExitStatus::new()
                            .exit_code(
                                term_exit
                                    .get("exit_code")
                                    .and_then(|v| v.as_u64())
                                    .map(|i| i as u32),
                            )
                            .signal(
                                term_exit
                                    .get("signal")
                                    .and_then(|v| v.as_str().map(|s| s.to_string())),
                            );

                        let _ = thread.update(&mut self.cx.clone(), |thread, cx| {
                            thread.on_terminal_provider_event(
                                TerminalProviderEvent::Exit {
                                    terminal_id,
                                    status,
                                },
                                cx,
                            );
                        });
                    }
                }
            }
        }

        Ok(())
    }

    async fn create_terminal(
        &self,
        args: acp::CreateTerminalRequest,
    ) -> Result<acp::CreateTerminalResponse, acp::Error> {
        let thread = self.session_thread(&args.session_id)?;
        let project = thread.read_with(&self.cx, |thread, _cx| thread.project().clone())?;

        let terminal_entity = acp_thread::create_terminal_entity(
            args.command.clone(),
            &args.args,
            args.env
                .into_iter()
                .map(|env| (env.name, env.value))
                .collect(),
            args.cwd.clone(),
            &project,
            &mut self.cx.clone(),
        )
        .await?;

        // Register with renderer
        let terminal_entity = thread.update(&mut self.cx.clone(), |thread, cx| {
            thread.register_terminal_created(
                acp::TerminalId::new(uuid::Uuid::new_v4().to_string()),
                format!("{} {}", args.command, args.args.join(" ")),
                args.cwd.clone(),
                args.output_byte_limit,
                terminal_entity,
                cx,
            )
        })?;
        let terminal_id = terminal_entity.read_with(&self.cx, |terminal, _| terminal.id().clone());
        Ok(acp::CreateTerminalResponse::new(terminal_id))
    }

    async fn kill_terminal(
        &self,
        args: acp::KillTerminalRequest,
    ) -> Result<acp::KillTerminalResponse, acp::Error> {
        self.session_thread(&args.session_id)?
            .update(&mut self.cx.clone(), |thread, cx| {
                thread.kill_terminal(args.terminal_id, cx)
            })??;

        Ok(Default::default())
    }

    async fn ext_method(&self, _args: acp::ExtRequest) -> Result<acp::ExtResponse, acp::Error> {
        Err(acp::Error::method_not_found())
    }

    async fn ext_notification(&self, _args: acp::ExtNotification) -> Result<(), acp::Error> {
        Err(acp::Error::method_not_found())
    }

    async fn release_terminal(
        &self,
        args: acp::ReleaseTerminalRequest,
    ) -> Result<acp::ReleaseTerminalResponse, acp::Error> {
        self.session_thread(&args.session_id)?
            .update(&mut self.cx.clone(), |thread, cx| {
                thread.release_terminal(args.terminal_id, cx)
            })??;

        Ok(Default::default())
    }

    async fn terminal_output(
        &self,
        args: acp::TerminalOutputRequest,
    ) -> Result<acp::TerminalOutputResponse, acp::Error> {
        self.session_thread(&args.session_id)?
            .read_with(&mut self.cx.clone(), |thread, cx| {
                let out = thread
                    .terminal(args.terminal_id)?
                    .read(cx)
                    .current_output(cx);

                Ok(out)
            })?
    }

    async fn wait_for_terminal_exit(
        &self,
        args: acp::WaitForTerminalExitRequest,
    ) -> Result<acp::WaitForTerminalExitResponse, acp::Error> {
        let exit_status = self
            .session_thread(&args.session_id)?
            .update(&mut self.cx.clone(), |thread, cx| {
                anyhow::Ok(thread.terminal(args.terminal_id)?.read(cx).wait_for_exit())
            })??
            .await;

        Ok(acp::WaitForTerminalExitResponse::new(exit_status))
    }
}

impl ClientDelegate {
    fn session_thread(&self, session_id: &acp::SessionId) -> Result<WeakEntity<AcpThread>> {
        let sessions = self.sessions.borrow();
        sessions
            .get(session_id)
            .context("Failed to get session")
            .map(|session| session.thread.clone())
    }
}
