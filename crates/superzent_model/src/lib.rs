use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use gpui::{App, AppContext, Context, Entity, Global};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};
use uuid::Uuid;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    #[default]
    Idle,
    Starting,
    Running,
    NeedsAttention,
    Completed,
    Failed,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceKind {
    #[default]
    Primary,
    Worktree,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceAttentionStatus {
    #[default]
    Idle,
    Working,
    Permission,
    Review,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PresetLaunchMode {
    #[default]
    Terminal,
    Acp,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitChangeSummary {
    #[serde(default)]
    pub changed_files: usize,
    #[serde(default)]
    pub staged_files: usize,
    #[serde(default)]
    pub untracked_files: usize,
    #[serde(default)]
    pub added_lines: usize,
    #[serde(default)]
    pub deleted_lines: usize,
    #[serde(default)]
    pub ahead_commits: usize,
    #[serde(default)]
    pub behind_commits: usize,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceGitStatus {
    #[default]
    Available,
    Unavailable,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentPreset {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub launch_mode: PresetLaunchMode,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub acp_agent_name: Option<String>,
    #[serde(default)]
    pub attention_patterns: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentPresetDraft {
    pub label: String,
    pub launch_mode: PresetLaunchMode,
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub acp_agent_name: Option<String>,
    pub attention_patterns: Vec<String>,
}

impl From<&AgentPreset> for AgentPresetDraft {
    fn from(value: &AgentPreset) -> Self {
        Self {
            label: value.label.clone(),
            launch_mode: value.launch_mode,
            command: value.command.clone(),
            args: value.args.clone(),
            env: value.env.clone(),
            acp_agent_name: value.acp_agent_name.clone(),
            attention_patterns: value.attention_patterns.clone(),
        }
    }
}

impl AgentPreset {
    pub fn resolved_acp_agent_name(&self) -> Option<String> {
        normalize_optional_text(self.acp_agent_name.clone())
            .or_else(|| suggested_acp_agent_name(&self.command))
    }
}

pub fn suggested_acp_agent_name(command: &str) -> Option<String> {
    match command.trim() {
        "codex" | "codex-acp" => Some("codex-acp".to_string()),
        "claude" | "claude-code" | "claude-acp" => Some("claude-acp".to_string()),
        "gemini" => Some("gemini".to_string()),
        _ => None,
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct StoredSshPortForward {
    pub local_host: Option<String>,
    pub local_port: u16,
    pub remote_host: Option<String>,
    pub remote_port: u16,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct StoredSshConnection {
    pub host: String,
    pub username: Option<String>,
    pub port: Option<u16>,
    #[serde(default)]
    pub args: Vec<String>,
    pub nickname: Option<String>,
    #[serde(default)]
    pub upload_binary_over_ssh: bool,
    #[serde(default)]
    pub port_forwards: Vec<StoredSshPortForward>,
    pub connection_timeout: Option<u16>,
}

impl StoredSshConnection {
    pub fn matches(&self, other: &StoredSshConnection) -> bool {
        self.host == other.host
            && self.username == other.username
            && self.port == other.port
            && self.args == other.args
            && self.nickname == other.nickname
            && self.upload_binary_over_ssh == other.upload_binary_over_ssh
            && self.port_forwards == other.port_forwards
            && self.connection_timeout == other.connection_timeout
    }

    pub fn display_target(&self) -> String {
        let mut target = String::new();
        if let Some(username) = &self.username {
            target.push_str(username);
            target.push('@');
        }
        target.push_str(&self.host);
        if let Some(port) = self.port {
            target.push(':');
            target.push_str(&port.to_string());
        }
        target
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "transport", rename_all = "snake_case")]
pub enum ProjectLocation {
    Local {
        repo_root: PathBuf,
    },
    Ssh {
        connection: StoredSshConnection,
        repo_root: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "transport", rename_all = "snake_case")]
pub enum WorkspaceLocation {
    Local {
        worktree_path: PathBuf,
    },
    Ssh {
        connection: StoredSshConnection,
        worktree_path: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectLocator<'a> {
    Local(&'a Path),
    Ssh {
        connection: &'a StoredSshConnection,
        repo_root: &'a str,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkspaceLocator<'a> {
    Local(&'a Path),
    Ssh {
        connection: &'a StoredSshConnection,
        worktree_path: &'a str,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectEntry {
    pub id: String,
    pub name: String,
    pub location: ProjectLocation,
    #[serde(default)]
    pub collapsed: bool,
    pub created_at: DateTime<Utc>,
    pub last_opened_at: DateTime<Utc>,
}

impl ProjectEntry {
    pub fn local_repo_root(&self) -> Option<&Path> {
        match &self.location {
            ProjectLocation::Local { repo_root } => Some(repo_root.as_path()),
            ProjectLocation::Ssh { .. } => None,
        }
    }

    pub fn ssh_connection(&self) -> Option<&StoredSshConnection> {
        match &self.location {
            ProjectLocation::Local { .. } => None,
            ProjectLocation::Ssh { connection, .. } => Some(connection),
        }
    }

    pub fn ssh_repo_root(&self) -> Option<&str> {
        match &self.location {
            ProjectLocation::Local { .. } => None,
            ProjectLocation::Ssh { repo_root, .. } => Some(repo_root),
        }
    }

    pub fn locator(&self) -> ProjectLocator<'_> {
        match &self.location {
            ProjectLocation::Local { repo_root } => ProjectLocator::Local(repo_root),
            ProjectLocation::Ssh {
                connection,
                repo_root,
            } => ProjectLocator::Ssh {
                connection,
                repo_root,
            },
        }
    }

    pub fn matches_locator(&self, locator: &ProjectLocator<'_>) -> bool {
        match (&self.location, locator) {
            (ProjectLocation::Local { repo_root }, ProjectLocator::Local(target)) => {
                repo_root == *target
            }
            (
                ProjectLocation::Ssh {
                    connection,
                    repo_root,
                },
                ProjectLocator::Ssh {
                    connection: target_connection,
                    repo_root: target_repo_root,
                },
            ) => connection.matches(target_connection) && repo_root == target_repo_root,
            _ => false,
        }
    }

    pub fn display_root(&self) -> String {
        match &self.location {
            ProjectLocation::Local { repo_root } => repo_root.display().to_string(),
            ProjectLocation::Ssh {
                connection,
                repo_root,
            } => format_remote_path(connection, repo_root),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceEntry {
    pub id: String,
    pub project_id: String,
    pub kind: WorkspaceKind,
    pub name: String,
    #[serde(default)]
    pub display_name: Option<String>,
    pub branch: String,
    pub location: WorkspaceLocation,
    pub agent_preset_id: String,
    pub managed: bool,
    #[serde(default)]
    pub git_status: WorkspaceGitStatus,
    #[serde(default)]
    pub git_summary: Option<GitChangeSummary>,
    #[serde(default)]
    pub attention_status: WorkspaceAttentionStatus,
    #[serde(default)]
    pub review_pending: bool,
    #[serde(default)]
    pub last_attention_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_opened_at: DateTime<Utc>,
}

impl WorkspaceEntry {
    pub fn is_existing_path(&self) -> bool {
        match &self.location {
            WorkspaceLocation::Local { worktree_path } => worktree_path.exists(),
            WorkspaceLocation::Ssh { .. } => true,
        }
    }

    pub fn is_primary(&self) -> bool {
        self.kind == WorkspaceKind::Primary
    }

    pub fn has_git(&self) -> bool {
        self.git_status == WorkspaceGitStatus::Available
    }

    pub fn local_worktree_path(&self) -> Option<&Path> {
        match &self.location {
            WorkspaceLocation::Local { worktree_path } => Some(worktree_path.as_path()),
            WorkspaceLocation::Ssh { .. } => None,
        }
    }

    pub fn ssh_connection(&self) -> Option<&StoredSshConnection> {
        match &self.location {
            WorkspaceLocation::Local { .. } => None,
            WorkspaceLocation::Ssh { connection, .. } => Some(connection),
        }
    }

    pub fn ssh_worktree_path(&self) -> Option<&str> {
        match &self.location {
            WorkspaceLocation::Local { .. } => None,
            WorkspaceLocation::Ssh { worktree_path, .. } => Some(worktree_path),
        }
    }

    pub fn locator(&self) -> WorkspaceLocator<'_> {
        match &self.location {
            WorkspaceLocation::Local { worktree_path } => WorkspaceLocator::Local(worktree_path),
            WorkspaceLocation::Ssh {
                connection,
                worktree_path,
            } => WorkspaceLocator::Ssh {
                connection,
                worktree_path,
            },
        }
    }

    pub fn matches_locator(&self, locator: &WorkspaceLocator<'_>) -> bool {
        match (&self.location, locator) {
            (WorkspaceLocation::Local { worktree_path }, WorkspaceLocator::Local(target)) => {
                worktree_path == *target
            }
            (
                WorkspaceLocation::Ssh {
                    connection,
                    worktree_path,
                },
                WorkspaceLocator::Ssh {
                    connection: target_connection,
                    worktree_path: target_worktree_path,
                },
            ) => connection.matches(target_connection) && worktree_path == target_worktree_path,
            _ => false,
        }
    }

    pub fn display_path(&self) -> String {
        match &self.location {
            WorkspaceLocation::Local { worktree_path } => worktree_path.display().to_string(),
            WorkspaceLocation::Ssh {
                connection,
                worktree_path,
            } => format_remote_path(connection, worktree_path),
        }
    }

    pub fn cwd_path(&self) -> PathBuf {
        match &self.location {
            WorkspaceLocation::Local { worktree_path } => worktree_path.clone(),
            WorkspaceLocation::Ssh { worktree_path, .. } => PathBuf::from(worktree_path),
        }
    }

    pub fn display_name(&self) -> &str {
        self.display_name.as_deref().unwrap_or(match self.kind {
            WorkspaceKind::Primary => match &self.location {
                WorkspaceLocation::Local { .. } => "local",
                WorkspaceLocation::Ssh { .. } => "remote",
            },
            WorkspaceKind::Worktree => &self.name,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentSession {
    pub id: String,
    pub workspace_id: String,
    pub preset_id: String,
    pub label: String,
    pub status: TaskStatus,
    pub started_at: DateTime<Utc>,
    #[serde(default)]
    pub exited_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_attention_reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SuperzentState {
    pub active_project_id: Option<String>,
    pub active_workspace_id: Option<String>,
    pub projects: Vec<ProjectEntry>,
    pub workspaces: Vec<WorkspaceEntry>,
    pub sessions: Vec<AgentSession>,
    pub presets: Vec<AgentPreset>,
}

impl Default for SuperzentState {
    fn default() -> Self {
        Self {
            active_project_id: None,
            active_workspace_id: None,
            projects: Vec::new(),
            workspaces: Vec::new(),
            sessions: Vec::new(),
            presets: default_presets(),
        }
    }
}

pub struct SuperzentStore {
    state_path: PathBuf,
    state: SuperzentState,
}

struct GlobalSuperzentStore(Entity<SuperzentStore>);

impl Global for GlobalSuperzentStore {}

impl SuperzentStore {
    pub fn init(cx: &mut App) {
        let store = cx.new(|_| Self::load());
        cx.set_global(GlobalSuperzentStore(store));
    }

    pub fn global(cx: &App) -> Entity<Self> {
        cx.global::<GlobalSuperzentStore>().0.clone()
    }

    pub fn try_global(cx: &App) -> Option<Entity<Self>> {
        cx.try_global::<GlobalSuperzentStore>()
            .map(|store| store.0.clone())
    }

    pub fn projects(&self) -> &[ProjectEntry] {
        &self.state.projects
    }

    pub fn workspaces(&self) -> &[WorkspaceEntry] {
        &self.state.workspaces
    }

    pub fn sessions(&self) -> &[AgentSession] {
        &self.state.sessions
    }

    pub fn active_project_id(&self) -> Option<&str> {
        self.state.active_project_id.as_deref()
    }

    pub fn active_workspace_id(&self) -> Option<&str> {
        self.state.active_workspace_id.as_deref()
    }

    pub fn active_project(&self) -> Option<&ProjectEntry> {
        self.state
            .active_project_id
            .as_deref()
            .and_then(|id| self.project(id))
    }

    pub fn active_workspace(&self) -> Option<&WorkspaceEntry> {
        self.state
            .active_workspace_id
            .as_deref()
            .and_then(|id| self.workspace(id))
    }

    pub fn project(&self, id: &str) -> Option<&ProjectEntry> {
        self.state.projects.iter().find(|project| project.id == id)
    }

    pub fn workspace(&self, id: &str) -> Option<&WorkspaceEntry> {
        self.state
            .workspaces
            .iter()
            .find(|workspace| workspace.id == id)
    }

    pub fn workspace_for_path(&self, path: &Path) -> Option<&WorkspaceEntry> {
        self.state.workspaces.iter().find(|workspace| {
            workspace
                .local_worktree_path()
                .is_some_and(|worktree_path| worktree_path == path)
        })
    }

    pub fn workspace_for_path_or_ancestor(&self, path: &Path) -> Option<&WorkspaceEntry> {
        self.state
            .workspaces
            .iter()
            .filter_map(|workspace| {
                workspace.local_worktree_path().and_then(|worktree_path| {
                    path.starts_with(worktree_path)
                        .then_some((workspace, worktree_path.components().count()))
                })
            })
            .max_by_key(|(_, depth)| *depth)
            .map(|(workspace, _)| workspace)
    }

    pub fn workspace_for_locator(&self, locator: &WorkspaceLocator<'_>) -> Option<&WorkspaceEntry> {
        self.state
            .workspaces
            .iter()
            .find(|workspace| workspace.matches_locator(locator))
    }

    pub fn workspace_for_location(&self, location: &WorkspaceLocation) -> Option<&WorkspaceEntry> {
        match location {
            WorkspaceLocation::Local { worktree_path } => self.workspace_for_path(worktree_path),
            WorkspaceLocation::Ssh {
                connection,
                worktree_path,
            } => self.workspace_for_locator(&WorkspaceLocator::Ssh {
                connection,
                worktree_path,
            }),
        }
    }

    pub fn project_for_workspace(&self, workspace_id: &str) -> Option<&ProjectEntry> {
        self.workspace(workspace_id)
            .and_then(|workspace| self.project(&workspace.project_id))
    }

    pub fn project_for_repo_root(&self, repo_root: &Path) -> Option<&ProjectEntry> {
        self.state
            .projects
            .iter()
            .find(|project| project.local_repo_root() == Some(repo_root))
    }

    pub fn project_for_locator(&self, locator: &ProjectLocator<'_>) -> Option<&ProjectEntry> {
        self.state
            .projects
            .iter()
            .find(|project| project.matches_locator(locator))
    }

    pub fn project_for_location(&self, location: &ProjectLocation) -> Option<&ProjectEntry> {
        match location {
            ProjectLocation::Local { repo_root } => self.project_for_repo_root(repo_root),
            ProjectLocation::Ssh {
                connection,
                repo_root,
            } => self.project_for_locator(&ProjectLocator::Ssh {
                connection,
                repo_root,
            }),
        }
    }

    pub fn project_for_workspace_sync(
        &self,
        existing_workspace: Option<&WorkspaceEntry>,
        project_location: &ProjectLocation,
    ) -> Option<&ProjectEntry> {
        existing_workspace
            .and_then(|workspace| self.project(&workspace.project_id))
            .or_else(|| self.project_for_location(project_location))
    }

    pub fn primary_workspace_for_project(&self, project_id: &str) -> Option<&WorkspaceEntry> {
        self.state
            .workspaces
            .iter()
            .find(|workspace| workspace.project_id == project_id && workspace.is_primary())
    }

    pub fn startup_workspace(&self) -> Option<&WorkspaceEntry> {
        self.active_workspace()
            .or_else(|| self.default_startup_workspace())
    }

    pub fn workspaces_for_project(&self, project_id: &str) -> Vec<&WorkspaceEntry> {
        self.state
            .workspaces
            .iter()
            .filter(|workspace| workspace.project_id == project_id)
            .collect::<Vec<_>>()
    }

    pub fn presets(&self) -> &[AgentPreset] {
        &self.state.presets
    }

    pub fn default_preset(&self) -> &AgentPreset {
        self.state
            .presets
            .first()
            .expect("Superzent requires at least one agent preset")
    }

    pub fn preset(&self, id: &str) -> Option<&AgentPreset> {
        self.state.presets.iter().find(|preset| preset.id == id)
    }

    pub fn create_preset(
        &mut self,
        draft: AgentPresetDraft,
        cx: &mut Context<Self>,
    ) -> Result<AgentPreset> {
        let base_id = preset_slug(&draft.label);
        let preset = draft.into_preset(self.unique_preset_id(&base_id))?;
        self.state.presets.push(preset.clone());
        self.persist_and_notify(cx);
        Ok(preset)
    }

    pub fn update_preset(
        &mut self,
        preset_id: &str,
        draft: AgentPresetDraft,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let Some(preset_index) = self
            .state
            .presets
            .iter()
            .position(|preset| preset.id == preset_id)
        else {
            bail!("unknown preset `{preset_id}`");
        };

        self.state.presets[preset_index] = draft.into_preset(preset_id.to_string())?;
        self.persist_and_notify(cx);
        Ok(())
    }

    pub fn delete_preset(&mut self, preset_id: &str, cx: &mut Context<Self>) -> Result<()> {
        if self.state.presets.len() <= 1 {
            bail!("at least one preset is required");
        }

        let Some(preset_index) = self
            .state
            .presets
            .iter()
            .position(|preset| preset.id == preset_id)
        else {
            bail!("unknown preset `{preset_id}`");
        };

        self.state.presets.remove(preset_index);
        let fallback_preset_id = self
            .state
            .presets
            .first()
            .map(|preset| preset.id.clone())
            .ok_or_else(|| anyhow::anyhow!("missing fallback preset"))?;

        for workspace in &mut self.state.workspaces {
            if workspace.agent_preset_id == preset_id {
                workspace.agent_preset_id = fallback_preset_id.clone();
            }
        }

        self.persist_and_notify(cx);
        Ok(())
    }

    pub fn reorder_preset(
        &mut self,
        dragged_preset_id: &str,
        target_preset_id: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        let Some(source_index) = self
            .state
            .presets
            .iter()
            .position(|preset| preset.id == dragged_preset_id)
        else {
            return;
        };

        if let Some(target_preset_id) = target_preset_id {
            let Some(target_index) = self
                .state
                .presets
                .iter()
                .position(|preset| preset.id == target_preset_id)
            else {
                return;
            };

            if source_index == target_index {
                return;
            }

            let preset = self.state.presets.remove(source_index);
            let Some(insert_index) = self
                .state
                .presets
                .iter()
                .position(|preset| preset.id == target_preset_id)
            else {
                self.state.presets.insert(source_index, preset);
                return;
            };
            self.state.presets.insert(insert_index, preset);
        } else {
            let preset = self.state.presets.remove(source_index);
            self.state.presets.push(preset);
        }

        self.persist_and_notify(cx);
    }

    pub fn set_active_workspace(
        &mut self,
        workspace_id: Option<impl Into<String>>,
        cx: &mut Context<Self>,
    ) {
        self.state.active_workspace_id = workspace_id.map(Into::into);
        if let Some(workspace_id) = self.state.active_workspace_id.clone() {
            self.clear_workspace_review_pending(&workspace_id);
        }
        self.state.active_project_id = self
            .state
            .active_workspace_id
            .as_deref()
            .and_then(|workspace_id| self.workspace(workspace_id))
            .map(|workspace| workspace.project_id.clone())
            .or_else(|| {
                self.state
                    .projects
                    .first()
                    .map(|project| project.id.clone())
            });
        self.persist_and_notify(cx);
    }

    pub fn set_active_workspace_by_path(&mut self, path: &Path, cx: &mut Context<Self>) {
        let workspace_id = self
            .workspace_for_path_or_ancestor(path)
            .map(|workspace| workspace.id.clone());
        self.set_active_workspace(workspace_id, cx);
    }

    pub fn set_project_collapsed(
        &mut self,
        project_id: &str,
        collapsed: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(project) = self
            .state
            .projects
            .iter_mut()
            .find(|project| project.id == project_id)
        {
            project.collapsed = collapsed;
            self.persist_and_notify(cx);
        }
    }

    pub fn upsert_project(&mut self, project: ProjectEntry, cx: &mut Context<Self>) {
        if let Some(existing) = self
            .state
            .projects
            .iter_mut()
            .find(|existing| existing.id == project.id)
        {
            *existing = project;
        } else {
            self.state.projects.push(project);
        }
        self.normalize();
        self.persist_and_notify(cx);
    }

    pub fn upsert_workspace(&mut self, workspace: WorkspaceEntry, cx: &mut Context<Self>) {
        if let Some(existing) = self
            .state
            .workspaces
            .iter_mut()
            .find(|existing| existing.id == workspace.id)
        {
            *existing = workspace;
        } else {
            self.state.workspaces.push(workspace);
        }
        self.normalize();
        self.persist_and_notify(cx);
    }

    pub fn upsert_project_bundle(
        &mut self,
        project: ProjectEntry,
        workspace: WorkspaceEntry,
        cx: &mut Context<Self>,
    ) {
        let project_id = project.id.clone();
        let workspace_id = workspace.id.clone();
        if self.project(&project.id).is_none() {
            self.state.projects.push(project);
        } else {
            self.state
                .projects
                .iter_mut()
                .filter(|existing| existing.id == project.id)
                .for_each(|existing| *existing = project.clone());
        }

        if self.workspace(&workspace.id).is_none() {
            self.state.workspaces.push(workspace);
        } else {
            self.state
                .workspaces
                .iter_mut()
                .filter(|existing| existing.id == workspace.id)
                .for_each(|existing| *existing = workspace.clone());
        }

        self.normalize();
        if self.state.active_workspace_id.is_none() {
            self.state.active_workspace_id = Some(workspace_id);
            self.state.active_project_id = Some(project_id);
        }
        self.persist_and_notify(cx);
    }

    pub fn record_workspace_opened(&mut self, workspace_id: &str, cx: &mut Context<Self>) {
        let now = Utc::now();
        let Some(workspace) = self
            .state
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.id == workspace_id)
        else {
            return;
        };

        workspace.last_opened_at = now;
        workspace.review_pending = false;
        if workspace.attention_status == WorkspaceAttentionStatus::Review {
            workspace.attention_status = WorkspaceAttentionStatus::Idle;
            workspace.last_attention_reason = None;
        }
        let project_id = workspace.project_id.clone();
        if let Some(project) = self
            .state
            .projects
            .iter_mut()
            .find(|project| project.id == project_id)
        {
            project.last_opened_at = now;
        }

        self.state.active_workspace_id = Some(workspace_id.to_string());
        self.state.active_project_id = Some(project_id);
        self.persist_and_notify(cx);
    }

    pub fn refresh_workspace_metadata(
        &mut self,
        workspace_id: &str,
        branch: Option<String>,
        git_status: WorkspaceGitStatus,
        git_summary: Option<GitChangeSummary>,
        persist: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(workspace) = self
            .state
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.id == workspace_id)
        {
            if !update_workspace_metadata(workspace, branch, git_status, git_summary) {
                return;
            }

            if persist {
                self.persist_and_notify(cx);
            } else {
                cx.notify();
            }
        }
    }

    pub fn set_workspace_display_name(
        &mut self,
        workspace_id: &str,
        display_name: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self
            .state
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.id == workspace_id)
        else {
            return;
        };

        let display_name = display_name
            .map(|display_name| display_name.trim().to_string())
            .filter(|display_name| !display_name.is_empty() && display_name != &workspace.name);

        if workspace.display_name != display_name {
            workspace.display_name = display_name;
            self.persist_and_notify(cx);
        }
    }

    pub fn set_workspace_agent_preset(
        &mut self,
        workspace_id: &str,
        preset_id: &str,
        cx: &mut Context<Self>,
    ) {
        if self.preset(preset_id).is_none() {
            return;
        }

        let Some(workspace) = self
            .state
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.id == workspace_id)
        else {
            return;
        };

        if workspace.agent_preset_id != preset_id {
            workspace.agent_preset_id = preset_id.to_string();
            self.persist_and_notify(cx);
        }
    }

    pub fn remove_workspace(
        &mut self,
        workspace_id: &str,
        cx: &mut Context<Self>,
    ) -> Option<WorkspaceEntry> {
        let ix = self
            .state
            .workspaces
            .iter()
            .position(|workspace| workspace.id == workspace_id)?;
        let removed = self.state.workspaces.remove(ix);
        self.state
            .sessions
            .retain(|session| session.workspace_id != removed.id);
        self.normalize();
        self.persist_and_notify(cx);
        Some(removed)
    }

    pub fn remove_project(
        &mut self,
        project_id: &str,
        cx: &mut Context<Self>,
    ) -> Option<(ProjectEntry, Vec<WorkspaceEntry>)> {
        let project_index = self
            .state
            .projects
            .iter()
            .position(|project| project.id == project_id)?;
        let removed_project = self.state.projects.remove(project_index);

        let mut removed_workspaces = Vec::new();
        let mut remaining_workspaces = Vec::with_capacity(self.state.workspaces.len());
        for workspace in std::mem::take(&mut self.state.workspaces) {
            if workspace.project_id == project_id {
                removed_workspaces.push(workspace);
            } else {
                remaining_workspaces.push(workspace);
            }
        }
        self.state.workspaces = remaining_workspaces;

        let removed_workspace_ids = removed_workspaces
            .iter()
            .map(|workspace| workspace.id.as_str())
            .collect::<BTreeSet<_>>();
        self.state
            .sessions
            .retain(|session| !removed_workspace_ids.contains(session.workspace_id.as_str()));

        self.normalize();
        self.persist_and_notify(cx);
        Some((removed_project, removed_workspaces))
    }

    pub fn reorder_workspace(
        &mut self,
        dragged_workspace_id: &str,
        target_workspace_id: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        let Some(source_ix) = self
            .state
            .workspaces
            .iter()
            .position(|workspace| workspace.id == dragged_workspace_id)
        else {
            return;
        };

        let project_id = self.state.workspaces[source_ix].project_id.clone();

        if let Some(target_workspace_id) = target_workspace_id {
            let Some(target_ix) = self
                .state
                .workspaces
                .iter()
                .position(|workspace| workspace.id == target_workspace_id)
            else {
                return;
            };

            if source_ix == target_ix || self.state.workspaces[target_ix].project_id != project_id {
                return;
            }

            let workspace = self.state.workspaces.remove(source_ix);
            let Some(insert_ix) = self
                .state
                .workspaces
                .iter()
                .position(|workspace| workspace.id == target_workspace_id)
            else {
                self.state.workspaces.insert(source_ix, workspace);
                return;
            };
            self.state.workspaces.insert(insert_ix, workspace);
        } else {
            let workspace = self.state.workspaces.remove(source_ix);
            let insert_ix = self
                .state
                .workspaces
                .iter()
                .enumerate()
                .filter(|(_, workspace)| workspace.project_id == project_id)
                .map(|(ix, _)| ix + 1)
                .next_back()
                .unwrap_or(self.state.workspaces.len());
            self.state.workspaces.insert(insert_ix, workspace);
        }

        self.persist_and_notify(cx);
    }

    pub fn reorder_project(
        &mut self,
        dragged_project_id: &str,
        target_project_id: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        let Some(source_ix) = self
            .state
            .projects
            .iter()
            .position(|project| project.id == dragged_project_id)
        else {
            return;
        };

        if let Some(target_project_id) = target_project_id {
            let Some(target_ix) = self
                .state
                .projects
                .iter()
                .position(|project| project.id == target_project_id)
            else {
                return;
            };

            if source_ix == target_ix {
                return;
            }

            let project = self.state.projects.remove(source_ix);
            let Some(insert_ix) = self
                .state
                .projects
                .iter()
                .position(|project| project.id == target_project_id)
            else {
                self.state.projects.insert(source_ix, project);
                return;
            };
            self.state.projects.insert(insert_ix, project);
        } else {
            let project = self.state.projects.remove(source_ix);
            self.state.projects.push(project);
        }

        self.persist_and_notify(cx);
    }

    pub fn start_session(
        &mut self,
        workspace_id: &str,
        preset: &AgentPreset,
        label: String,
        cx: &mut Context<Self>,
    ) -> AgentSession {
        let session = AgentSession {
            id: Uuid::new_v4().to_string(),
            workspace_id: workspace_id.to_string(),
            preset_id: preset.id.clone(),
            label,
            status: TaskStatus::Starting,
            started_at: Utc::now(),
            exited_at: None,
            last_attention_reason: None,
        };
        self.state.sessions.push(session.clone());
        self.persist_and_notify(cx);
        session
    }

    pub fn update_session_status(
        &mut self,
        session_id: &str,
        status: TaskStatus,
        reason: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if let Some(session) = self
            .state
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            let finished = matches!(
                status,
                TaskStatus::Idle
                    | TaskStatus::NeedsAttention
                    | TaskStatus::Completed
                    | TaskStatus::Failed
            );
            session.status = status;
            session.last_attention_reason = reason.clone();
            if finished {
                session.exited_at = Some(Utc::now());
            }

            if let Some(workspace) = self
                .state
                .workspaces
                .iter_mut()
                .find(|workspace| workspace.id == session.workspace_id)
            {
                workspace.last_attention_reason = reason;
                workspace.last_opened_at = Utc::now();
            }

            self.persist_and_notify(cx);
        }
    }

    pub fn latest_session_for_workspace(&self, workspace_id: &str) -> Option<&AgentSession> {
        self.state
            .sessions
            .iter()
            .filter(|session| session.workspace_id == workspace_id)
            .max_by(|left, right| left.started_at.cmp(&right.started_at))
    }

    pub fn aggregate_status_for_workspace(&self, workspace_id: &str) -> TaskStatus {
        self.latest_session_for_workspace(workspace_id)
            .map(|session| session.status.clone())
            .unwrap_or(TaskStatus::Idle)
    }

    pub fn set_workspace_attention(
        &mut self,
        workspace_id: &str,
        attention_status: WorkspaceAttentionStatus,
        review_pending: bool,
        reason: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self
            .state
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.id == workspace_id)
        else {
            return;
        };

        if workspace.attention_status == attention_status
            && workspace.review_pending == review_pending
            && workspace.last_attention_reason == reason
        {
            return;
        }

        workspace.attention_status = attention_status;
        workspace.review_pending = review_pending;
        workspace.last_attention_reason = reason;
        self.persist_and_notify(cx);
    }

    pub fn acknowledge_workspace_review(&mut self, workspace_id: &str, cx: &mut Context<Self>) {
        let Some(workspace) = self
            .state
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.id == workspace_id)
        else {
            return;
        };

        let mut changed = false;
        if workspace.review_pending {
            workspace.review_pending = false;
            changed = true;
        }
        if workspace.attention_status == WorkspaceAttentionStatus::Review {
            workspace.attention_status = WorkspaceAttentionStatus::Idle;
            workspace.last_attention_reason = None;
            changed = true;
        }

        if changed {
            self.persist_and_notify(cx);
        }
    }

    fn load() -> Self {
        let state_path = state_path();
        let mut state = fs::read_to_string(&state_path)
            .ok()
            .and_then(|contents| {
                serde_json::from_str::<SuperzentState>(&contents)
                    .ok()
                    .or_else(|| {
                        serde_json::from_str::<LegacySuperzentState>(&contents)
                            .ok()
                            .map(Into::into)
                    })
            })
            .or_else(load_legacy_state)
            .unwrap_or_default();

        state
            .projects
            .retain(|project| project.local_repo_root().is_none_or(Path::exists));
        state.workspaces.retain(WorkspaceEntry::is_existing_path);

        let workspace_ids = state
            .workspaces
            .iter()
            .map(|workspace| workspace.id.clone())
            .collect::<BTreeSet<_>>();
        state
            .sessions
            .retain(|session| workspace_ids.contains(&session.workspace_id));

        if state.presets.is_empty() {
            state.presets = default_presets();
        }

        let mut store = Self { state_path, state };
        store.normalize();
        store.clear_transient_workspace_attention();
        store
    }

    fn normalize(&mut self) {
        let project_ids = self
            .state
            .projects
            .iter()
            .map(|project| project.id.clone())
            .collect::<BTreeSet<_>>();
        self.state
            .workspaces
            .retain(|workspace| project_ids.contains(&workspace.project_id));

        let existing_workspace_ids = self
            .state
            .workspaces
            .iter()
            .map(|workspace| workspace.id.as_str())
            .collect::<BTreeSet<_>>();
        let fallback_workspace_id = self
            .default_startup_workspace()
            .map(|workspace| workspace.id.clone());
        if self
            .state
            .active_workspace_id
            .as_deref()
            .is_some_and(|id| !existing_workspace_ids.contains(id))
        {
            self.state.active_workspace_id = fallback_workspace_id.clone();
        }

        if self.state.active_workspace_id.is_none() {
            self.state.active_workspace_id = fallback_workspace_id;
        }

        let existing_project_ids = self
            .state
            .projects
            .iter()
            .map(|project| project.id.as_str())
            .collect::<BTreeSet<_>>();
        self.state.active_project_id = self
            .state
            .active_workspace_id
            .as_deref()
            .and_then(|workspace_id| self.workspace(workspace_id))
            .map(|workspace| workspace.project_id.clone())
            .or_else(|| {
                self.state
                    .active_project_id
                    .clone()
                    .filter(|project_id| existing_project_ids.contains(project_id.as_str()))
            })
            .or_else(|| {
                self.state
                    .projects
                    .first()
                    .map(|project| project.id.clone())
            });

        let mut preset_ids = BTreeSet::new();
        for preset in &mut self.state.presets {
            if preset.label.trim().is_empty() {
                preset.label = "Preset".to_string();
            }

            preset.acp_agent_name = normalize_optional_text(preset.acp_agent_name.clone())
                .or_else(|| suggested_acp_agent_name(&preset.command));

            let desired_id = if preset.id.trim().is_empty() {
                preset_slug(&preset.label)
            } else {
                preset_slug(&preset.id)
            };
            preset.id = unique_slug(&desired_id, &mut preset_ids);
        }

        if self.state.presets.is_empty() {
            self.state.presets = default_presets();
        }

        let valid_preset_ids = self
            .state
            .presets
            .iter()
            .map(|preset| preset.id.as_str())
            .collect::<BTreeSet<_>>();
        let fallback_preset_id = self
            .state
            .presets
            .first()
            .map(|preset| preset.id.clone())
            .unwrap_or_else(|| "codex".to_string());

        for workspace in &mut self.state.workspaces {
            if !valid_preset_ids.contains(workspace.agent_preset_id.as_str()) {
                workspace.agent_preset_id = fallback_preset_id.clone();
            }

            if workspace.attention_status == WorkspaceAttentionStatus::Review {
                workspace.review_pending = true;
            } else if workspace.review_pending
                && workspace.attention_status == WorkspaceAttentionStatus::Idle
            {
                workspace.attention_status = WorkspaceAttentionStatus::Review;
            }
        }
    }

    fn default_startup_workspace(&self) -> Option<&WorkspaceEntry> {
        self.state
            .projects
            .first()
            .and_then(|project| self.primary_workspace_for_project(&project.id))
            .or_else(|| self.state.workspaces.first())
    }

    fn clear_transient_workspace_attention(&mut self) {
        for workspace in &mut self.state.workspaces {
            if workspace.attention_status == WorkspaceAttentionStatus::Review {
                workspace.review_pending = true;
                continue;
            }

            if matches!(
                workspace.attention_status,
                WorkspaceAttentionStatus::Working | WorkspaceAttentionStatus::Permission
            ) {
                workspace.attention_status = if workspace.review_pending {
                    WorkspaceAttentionStatus::Review
                } else {
                    WorkspaceAttentionStatus::Idle
                };
            }
        }
    }

    fn clear_workspace_review_pending(&mut self, workspace_id: &str) {
        let Some(workspace) = self
            .state
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.id == workspace_id)
        else {
            return;
        };

        workspace.review_pending = false;
        if workspace.attention_status == WorkspaceAttentionStatus::Review {
            workspace.attention_status = WorkspaceAttentionStatus::Idle;
            workspace.last_attention_reason = None;
        }
    }

    fn persist_and_notify(&self, cx: &mut Context<Self>) {
        if let Err(error) = self.persist() {
            log::error!("failed to persist Superzent state: {error:#}");
        }
        cx.notify();
    }

    fn persist(&self) -> anyhow::Result<()> {
        if let Some(parent) = self.state_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let contents = serde_json::to_string_pretty(&self.state)?;
        fs::write(&self.state_path, contents)?;
        Ok(())
    }
}

fn update_workspace_metadata(
    workspace: &mut WorkspaceEntry,
    branch: Option<String>,
    git_status: WorkspaceGitStatus,
    git_summary: Option<GitChangeSummary>,
) -> bool {
    let mut changed = false;

    if let Some(branch) = branch
        && workspace.branch != branch
    {
        workspace.branch = branch;
        changed = true;
    }

    if workspace.git_status != git_status {
        workspace.git_status = git_status;
        changed = true;
    }

    if workspace.git_summary != git_summary {
        workspace.git_summary = git_summary;
        changed = true;
    }

    changed
}

#[derive(Deserialize)]
struct LegacySuperzentState {
    active_project_id: Option<String>,
    active_workspace_id: Option<String>,
    projects: Vec<LegacyProjectEntry>,
    workspaces: Vec<LegacyWorkspaceEntry>,
    #[serde(default)]
    sessions: Vec<AgentSession>,
    #[serde(default)]
    presets: Vec<AgentPreset>,
}

#[derive(Clone, Deserialize)]
struct LegacyProjectEntry {
    id: String,
    name: String,
    repo_root: PathBuf,
    #[serde(default)]
    collapsed: bool,
    created_at: DateTime<Utc>,
    last_opened_at: DateTime<Utc>,
}

#[derive(Clone, Deserialize)]
struct LegacyWorkspaceEntry {
    id: String,
    project_id: String,
    kind: WorkspaceKind,
    name: String,
    #[serde(default)]
    display_name: Option<String>,
    branch: String,
    worktree_path: PathBuf,
    agent_preset_id: String,
    managed: bool,
    #[serde(default)]
    git_summary: Option<GitChangeSummary>,
    #[serde(default)]
    attention_status: WorkspaceAttentionStatus,
    #[serde(default)]
    review_pending: bool,
    #[serde(default)]
    last_attention_reason: Option<String>,
    created_at: DateTime<Utc>,
    last_opened_at: DateTime<Utc>,
}

impl From<LegacySuperzentState> for SuperzentState {
    fn from(value: LegacySuperzentState) -> Self {
        Self {
            active_project_id: value.active_project_id,
            active_workspace_id: value.active_workspace_id,
            projects: value
                .projects
                .into_iter()
                .map(|project| ProjectEntry {
                    id: project.id,
                    name: project.name,
                    location: ProjectLocation::Local {
                        repo_root: project.repo_root,
                    },
                    collapsed: project.collapsed,
                    created_at: project.created_at,
                    last_opened_at: project.last_opened_at,
                })
                .collect(),
            workspaces: value
                .workspaces
                .into_iter()
                .map(|workspace| WorkspaceEntry {
                    id: workspace.id,
                    project_id: workspace.project_id,
                    kind: workspace.kind,
                    name: workspace.name,
                    display_name: workspace.display_name,
                    branch: workspace.branch,
                    location: WorkspaceLocation::Local {
                        worktree_path: workspace.worktree_path,
                    },
                    agent_preset_id: workspace.agent_preset_id,
                    managed: workspace.managed,
                    git_status: WorkspaceGitStatus::Available,
                    git_summary: workspace.git_summary,
                    attention_status: workspace.attention_status,
                    review_pending: workspace.review_pending,
                    last_attention_reason: workspace.last_attention_reason,
                    created_at: workspace.created_at,
                    last_opened_at: workspace.last_opened_at,
                })
                .collect(),
            sessions: value.sessions,
            presets: if value.presets.is_empty() {
                default_presets()
            } else {
                value.presets
            },
        }
    }
}

#[derive(Deserialize)]
struct LegacyState {
    active_task_id: Option<String>,
    #[serde(default)]
    tasks: Vec<LegacyTaskWorkspace>,
    #[serde(default)]
    presets: Vec<AgentPreset>,
}

#[derive(Clone, Deserialize)]
struct LegacyTaskWorkspace {
    id: String,
    name: String,
    repo_root: PathBuf,
    worktree_path: PathBuf,
    branch: String,
    agent_preset_id: String,
    status: TaskStatus,
    managed: bool,
    #[serde(default)]
    last_attention_reason: Option<String>,
    created_at: DateTime<Utc>,
    last_event_at: DateTime<Utc>,
}

fn load_legacy_state() -> Option<SuperzentState> {
    let legacy_path = legacy_state_path();
    let contents = fs::read_to_string(&legacy_path).ok()?;
    let legacy = serde_json::from_str::<LegacyState>(&contents).ok()?;

    let presets = if legacy.presets.is_empty() {
        default_presets()
    } else {
        legacy.presets
    };
    let default_preset_id = presets
        .first()
        .map(|preset| preset.id.clone())
        .unwrap_or_else(|| "codex".to_string());

    let mut state = SuperzentState {
        active_project_id: None,
        active_workspace_id: legacy.active_task_id.clone(),
        projects: Vec::new(),
        workspaces: Vec::new(),
        sessions: Vec::new(),
        presets,
    };

    for task in legacy.tasks {
        if !task.worktree_path.exists() {
            continue;
        }

        let repo_root = task.repo_root.clone();
        let repo_name = repo_root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Project")
            .to_string();
        let project_id = state
            .projects
            .iter()
            .find(|project| project.local_repo_root() == Some(repo_root.as_path()))
            .map(|project| project.id.clone())
            .unwrap_or_else(|| {
                let id = Uuid::new_v4().to_string();
                state.projects.push(ProjectEntry {
                    id: id.clone(),
                    name: repo_name.clone(),
                    location: ProjectLocation::Local {
                        repo_root: repo_root.clone(),
                    },
                    collapsed: false,
                    created_at: task.created_at,
                    last_opened_at: task.last_event_at,
                });
                id
            });

        if state
            .workspaces
            .iter()
            .all(|workspace| workspace.project_id != project_id || !workspace.is_primary())
        {
            state.workspaces.push(WorkspaceEntry {
                id: Uuid::new_v4().to_string(),
                project_id: project_id.clone(),
                kind: WorkspaceKind::Primary,
                name: repo_name.clone(),
                display_name: None,
                branch: "HEAD".to_string(),
                location: WorkspaceLocation::Local {
                    worktree_path: repo_root.clone(),
                },
                agent_preset_id: default_preset_id.clone(),
                managed: false,
                git_status: WorkspaceGitStatus::Available,
                git_summary: None,
                attention_status: WorkspaceAttentionStatus::Idle,
                review_pending: false,
                last_attention_reason: None,
                created_at: task.created_at,
                last_opened_at: task.last_event_at,
            });
        }

        state.workspaces.push(WorkspaceEntry {
            id: task.id.clone(),
            project_id: project_id.clone(),
            kind: WorkspaceKind::Worktree,
            name: task.name,
            display_name: None,
            branch: task.branch,
            location: WorkspaceLocation::Local {
                worktree_path: task.worktree_path,
            },
            agent_preset_id: task.agent_preset_id,
            managed: task.managed,
            git_status: WorkspaceGitStatus::Available,
            git_summary: None,
            attention_status: match task.status {
                TaskStatus::Completed | TaskStatus::Failed | TaskStatus::NeedsAttention => {
                    WorkspaceAttentionStatus::Review
                }
                _ => WorkspaceAttentionStatus::Idle,
            },
            review_pending: matches!(
                task.status,
                TaskStatus::Completed | TaskStatus::Failed | TaskStatus::NeedsAttention
            ),
            last_attention_reason: task.last_attention_reason.clone(),
            created_at: task.created_at,
            last_opened_at: task.last_event_at,
        });

        if matches!(
            task.status,
            TaskStatus::Running
                | TaskStatus::NeedsAttention
                | TaskStatus::Completed
                | TaskStatus::Failed
        ) {
            state.sessions.push(AgentSession {
                id: Uuid::new_v4().to_string(),
                workspace_id: task.id,
                preset_id: default_preset_id.clone(),
                label: "Migrated session".to_string(),
                status: task.status,
                started_at: task.created_at,
                exited_at: Some(task.last_event_at),
                last_attention_reason: task.last_attention_reason,
            });
        }
    }

    Some(state)
}

fn state_path() -> PathBuf {
    paths::data_dir().join("state.json")
}

fn legacy_state_path() -> PathBuf {
    paths::data_dir().join("tasks.json")
}

fn default_presets() -> Vec<AgentPreset> {
    vec![
        AgentPreset {
            id: "codex".into(),
            label: "Codex".into(),
            launch_mode: PresetLaunchMode::Terminal,
            command: "codex".into(),
            args: Vec::new(),
            env: BTreeMap::new(),
            acp_agent_name: Some("codex-acp".into()),
            attention_patterns: vec!["needs attention".into(), "blocked".into()],
        },
        AgentPreset {
            id: "claude-code".into(),
            label: "Claude Code".into(),
            launch_mode: PresetLaunchMode::Terminal,
            command: "claude".into(),
            args: Vec::new(),
            env: BTreeMap::new(),
            acp_agent_name: Some("claude-acp".into()),
            attention_patterns: vec!["waiting for input".into()],
        },
        AgentPreset {
            id: "gemini".into(),
            label: "Gemini CLI".into(),
            launch_mode: PresetLaunchMode::Terminal,
            command: "gemini".into(),
            args: Vec::new(),
            env: BTreeMap::new(),
            acp_agent_name: Some("gemini".into()),
            attention_patterns: vec!["press enter".into()],
        },
    ]
}

impl AgentPresetDraft {
    fn into_preset(self, id: String) -> Result<AgentPreset> {
        let label = self.label.trim().to_string();
        let command = self.command.trim().to_string();
        let acp_agent_name = normalize_optional_text(self.acp_agent_name)
            .or_else(|| suggested_acp_agent_name(&command));

        if label.is_empty() {
            bail!("preset label is required");
        }
        match self.launch_mode {
            PresetLaunchMode::Terminal if command.is_empty() => {
                bail!("preset command is required");
            }
            PresetLaunchMode::Acp if acp_agent_name.is_none() => {
                bail!("ACP agent name is required");
            }
            _ => {}
        }

        let args = self
            .args
            .into_iter()
            .map(|argument| argument.trim().to_string())
            .filter(|argument| !argument.is_empty())
            .collect();
        let env = self
            .env
            .into_iter()
            .filter_map(|(key, value)| {
                let key = key.trim().to_string();
                if key.is_empty() {
                    return None;
                }

                Some((key, value.trim().to_string()))
            })
            .collect();
        let attention_patterns = self
            .attention_patterns
            .into_iter()
            .map(|pattern| pattern.trim().to_string())
            .filter(|pattern| !pattern.is_empty())
            .collect();

        Ok(AgentPreset {
            id,
            label,
            launch_mode: self.launch_mode,
            command,
            args,
            env,
            acp_agent_name,
            attention_patterns,
        })
    }
}

impl SuperzentStore {
    fn unique_preset_id(&self, base_id: &str) -> String {
        let existing_ids = self
            .state
            .presets
            .iter()
            .map(|preset| preset.id.as_str())
            .collect::<BTreeSet<_>>();
        let mut used_ids = existing_ids
            .into_iter()
            .map(str::to_string)
            .collect::<BTreeSet<_>>();
        unique_slug(base_id, &mut used_ids)
    }
}

fn preset_slug(value: &str) -> String {
    let mut slug = String::new();
    let mut previous_was_separator = false;

    for character in value.chars() {
        let next_character = if character.is_ascii_alphanumeric() {
            previous_was_separator = false;
            character.to_ascii_lowercase()
        } else {
            if previous_was_separator || slug.is_empty() {
                continue;
            }
            previous_was_separator = true;
            '-'
        };
        slug.push(next_character);
    }

    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        "preset".to_string()
    } else {
        slug
    }
}

fn unique_slug(base_id: &str, used_ids: &mut BTreeSet<String>) -> String {
    let base_id = preset_slug(base_id);
    let mut next_id = base_id.clone();
    let mut suffix = 2usize;

    while used_ids.contains(&next_id) {
        next_id = format!("{base_id}-{suffix}");
        suffix += 1;
    }

    used_ids.insert(next_id.clone());
    next_id
}

fn normalize_optional_text(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn format_remote_path(connection: &StoredSshConnection, remote_path: &str) -> String {
    let separator = if remote_path.starts_with('/') {
        ""
    } else {
        "/"
    };
    format!(
        "ssh://{}{separator}{remote_path}",
        connection.display_target()
    )
}

pub fn ensure_parent_dir(path: &Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

pub fn aggregate_workspace_attention_status(
    live_attention_status: Option<WorkspaceAttentionStatus>,
    review_pending: bool,
) -> WorkspaceAttentionStatus {
    match live_attention_status {
        Some(WorkspaceAttentionStatus::Permission) => WorkspaceAttentionStatus::Permission,
        Some(WorkspaceAttentionStatus::Working) => WorkspaceAttentionStatus::Working,
        Some(WorkspaceAttentionStatus::Review) => WorkspaceAttentionStatus::Review,
        Some(WorkspaceAttentionStatus::Idle) | None => {
            if review_pending {
                WorkspaceAttentionStatus::Review
            } else {
                WorkspaceAttentionStatus::Idle
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project_entry(id: &str, repo_root: &str) -> ProjectEntry {
        ProjectEntry {
            id: id.to_string(),
            name: id.to_string(),
            location: ProjectLocation::Local {
                repo_root: PathBuf::from(repo_root),
            },
            collapsed: false,
            created_at: Utc::now(),
            last_opened_at: Utc::now(),
        }
    }

    fn workspace_entry(
        id: &str,
        project_id: &str,
        kind: WorkspaceKind,
        worktree_path: &str,
    ) -> WorkspaceEntry {
        WorkspaceEntry {
            id: id.to_string(),
            project_id: project_id.to_string(),
            kind,
            name: id.to_string(),
            display_name: None,
            branch: "main".to_string(),
            location: WorkspaceLocation::Local {
                worktree_path: PathBuf::from(worktree_path),
            },
            agent_preset_id: "codex".to_string(),
            managed: false,
            git_status: WorkspaceGitStatus::Available,
            git_summary: None,
            attention_status: WorkspaceAttentionStatus::Idle,
            review_pending: false,
            last_attention_reason: None,
            created_at: Utc::now(),
            last_opened_at: Utc::now(),
        }
    }

    fn remote_project_entry(
        id: &str,
        connection: StoredSshConnection,
        repo_root: &str,
    ) -> ProjectEntry {
        ProjectEntry {
            id: id.to_string(),
            name: id.to_string(),
            location: ProjectLocation::Ssh {
                connection,
                repo_root: repo_root.to_string(),
            },
            collapsed: false,
            created_at: Utc::now(),
            last_opened_at: Utc::now(),
        }
    }

    fn remote_workspace_entry(
        id: &str,
        project_id: &str,
        kind: WorkspaceKind,
        connection: StoredSshConnection,
        worktree_path: &str,
    ) -> WorkspaceEntry {
        WorkspaceEntry {
            id: id.to_string(),
            project_id: project_id.to_string(),
            kind,
            name: id.to_string(),
            display_name: None,
            branch: "main".to_string(),
            location: WorkspaceLocation::Ssh {
                connection,
                worktree_path: worktree_path.to_string(),
            },
            agent_preset_id: "codex".to_string(),
            managed: false,
            git_status: WorkspaceGitStatus::Available,
            git_summary: None,
            attention_status: WorkspaceAttentionStatus::Idle,
            review_pending: false,
            last_attention_reason: None,
            created_at: Utc::now(),
            last_opened_at: Utc::now(),
        }
    }

    #[test]
    fn update_workspace_metadata_ignores_missing_branch_when_summary_is_unchanged() {
        let mut workspace = workspace_entry(
            "worktree",
            "project",
            WorkspaceKind::Worktree,
            "/tmp/repo/feature",
        );

        assert!(!update_workspace_metadata(
            &mut workspace,
            None,
            WorkspaceGitStatus::Available,
            None,
        ));
        assert_eq!(workspace.branch, "main");
        assert_eq!(workspace.git_summary, None);
    }

    #[test]
    fn update_workspace_metadata_reports_branch_and_summary_changes() {
        let mut workspace = workspace_entry(
            "worktree",
            "project",
            WorkspaceKind::Worktree,
            "/tmp/repo/feature",
        );
        let summary = GitChangeSummary {
            changed_files: 3,
            staged_files: 1,
            untracked_files: 2,
            added_lines: 19,
            deleted_lines: 7,
            ahead_commits: 2,
            behind_commits: 1,
        };

        assert!(update_workspace_metadata(
            &mut workspace,
            Some("feature/fast".to_string()),
            WorkspaceGitStatus::Available,
            Some(summary.clone()),
        ));
        assert_eq!(workspace.branch, "feature/fast");
        assert_eq!(workspace.git_summary, Some(summary));
    }

    #[test]
    fn git_change_summary_defaults_missing_extended_fields() {
        let summary: GitChangeSummary = serde_json::from_value(serde_json::json!({
            "changed_files": 3,
            "staged_files": 1,
            "untracked_files": 2
        }))
        .unwrap();

        assert_eq!(
            summary,
            GitChangeSummary {
                changed_files: 3,
                staged_files: 1,
                untracked_files: 2,
                added_lines: 0,
                deleted_lines: 0,
                ahead_commits: 0,
                behind_commits: 0,
            }
        );
    }

    #[test]
    fn aggregates_live_attention_over_review_pending() {
        assert_eq!(
            aggregate_workspace_attention_status(Some(WorkspaceAttentionStatus::Permission), true,),
            WorkspaceAttentionStatus::Permission
        );
        assert_eq!(
            aggregate_workspace_attention_status(Some(WorkspaceAttentionStatus::Working), true),
            WorkspaceAttentionStatus::Working
        );
        assert_eq!(
            aggregate_workspace_attention_status(None, true),
            WorkspaceAttentionStatus::Review
        );
        assert_eq!(
            aggregate_workspace_attention_status(None, false),
            WorkspaceAttentionStatus::Idle
        );
    }

    #[test]
    fn finds_deepest_workspace_ancestor() {
        let workspaces = vec![
            workspace_entry("primary", "project", WorkspaceKind::Primary, "/tmp/repo"),
            workspace_entry(
                "worktree",
                "project",
                WorkspaceKind::Worktree,
                "/tmp/repo/feature",
            ),
        ];

        let store = SuperzentStore {
            state_path: PathBuf::from("/tmp/state.json"),
            state: SuperzentState {
                active_project_id: None,
                active_workspace_id: None,
                projects: Vec::new(),
                workspaces,
                sessions: Vec::new(),
                presets: default_presets(),
            },
        };

        let workspace = store
            .workspace_for_path_or_ancestor(Path::new("/tmp/repo/feature/src/main.rs"))
            .expect("workspace should resolve");
        assert_eq!(workspace.id, "worktree");
    }

    #[test]
    fn existing_local_workspace_keeps_its_project_during_sync() {
        let project_a = project_entry("project-a", "/tmp/repo");
        let project_b = project_entry("project-b", "/tmp/repo");
        let workspace = workspace_entry(
            "workspace",
            "project-b",
            WorkspaceKind::Worktree,
            "/tmp/repo-worktrees/feature",
        );

        let store = SuperzentStore {
            state_path: PathBuf::from("/tmp/state.json"),
            state: SuperzentState {
                projects: vec![project_a, project_b.clone()],
                workspaces: vec![workspace.clone()],
                ..Default::default()
            },
        };

        let resolved = store
            .project_for_workspace_sync(
                Some(&workspace),
                &ProjectLocation::Local {
                    repo_root: PathBuf::from("/tmp/repo"),
                },
            )
            .expect("workspace project should resolve");

        assert_eq!(resolved.id, project_b.id);
    }

    #[test]
    fn existing_remote_workspace_keeps_its_project_during_sync() {
        let connection = StoredSshConnection {
            host: "example.com".to_string(),
            username: Some("jun".to_string()),
            port: Some(2222),
            ..Default::default()
        };
        let project_a = remote_project_entry("project-a", connection.clone(), "/srv/repo");
        let project_b = remote_project_entry("project-b", connection.clone(), "/srv/repo");
        let workspace = remote_workspace_entry(
            "workspace",
            "project-b",
            WorkspaceKind::Worktree,
            connection.clone(),
            "/srv/worktrees/feature",
        );

        let store = SuperzentStore {
            state_path: PathBuf::from("/tmp/state.json"),
            state: SuperzentState {
                projects: vec![project_a, project_b.clone()],
                workspaces: vec![workspace.clone()],
                ..Default::default()
            },
        };

        let resolved = store
            .project_for_workspace_sync(
                Some(&workspace),
                &ProjectLocation::Ssh {
                    connection,
                    repo_root: "/srv/repo".to_string(),
                },
            )
            .expect("workspace project should resolve");

        assert_eq!(resolved.id, project_b.id);
    }

    #[test]
    fn new_workspace_falls_back_to_matching_project_location_during_sync() {
        let project = project_entry("project-a", "/tmp/repo");

        let store = SuperzentStore {
            state_path: PathBuf::from("/tmp/state.json"),
            state: SuperzentState {
                projects: vec![project.clone()],
                ..Default::default()
            },
        };

        let resolved = store
            .project_for_workspace_sync(
                None,
                &ProjectLocation::Local {
                    repo_root: PathBuf::from("/tmp/repo"),
                },
            )
            .expect("matching project should resolve");

        assert_eq!(resolved.id, project.id);
    }

    #[test]
    fn startup_workspace_prefers_active_workspace() {
        let store = SuperzentStore {
            state_path: PathBuf::from("/tmp/state.json"),
            state: SuperzentState {
                active_project_id: Some("project".to_string()),
                active_workspace_id: Some("worktree".to_string()),
                projects: vec![project_entry("project", "/tmp/repo")],
                workspaces: vec![
                    workspace_entry("primary", "project", WorkspaceKind::Primary, "/tmp/repo"),
                    workspace_entry(
                        "worktree",
                        "project",
                        WorkspaceKind::Worktree,
                        "/tmp/repo/feature",
                    ),
                ],
                sessions: Vec::new(),
                presets: default_presets(),
            },
        };

        let workspace = store.startup_workspace().expect("workspace should resolve");
        assert_eq!(workspace.id, "worktree");
    }

    #[test]
    fn built_in_presets_resolve_default_acp_agent_names() {
        let preset = AgentPreset {
            id: "codex".to_string(),
            label: "Codex".to_string(),
            launch_mode: PresetLaunchMode::Acp,
            command: "codex".to_string(),
            args: Vec::new(),
            env: BTreeMap::new(),
            acp_agent_name: None,
            attention_patterns: Vec::new(),
        };

        assert_eq!(
            preset.resolved_acp_agent_name().as_deref(),
            Some("codex-acp")
        );
    }

    #[test]
    fn acp_presets_require_an_agent_name_for_custom_commands() {
        let error = AgentPresetDraft {
            label: "Custom".to_string(),
            launch_mode: PresetLaunchMode::Acp,
            command: "my-agent".to_string(),
            args: Vec::new(),
            env: BTreeMap::new(),
            acp_agent_name: None,
            attention_patterns: Vec::new(),
        }
        .into_preset("custom".to_string())
        .expect_err("custom ACP presets should require an explicit agent name");

        assert_eq!(error.to_string(), "ACP agent name is required");
    }

    #[test]
    fn normalize_falls_back_to_first_project_primary_workspace() {
        let mut store = SuperzentStore {
            state_path: PathBuf::from("/tmp/state.json"),
            state: SuperzentState {
                active_project_id: Some("missing".to_string()),
                active_workspace_id: Some("missing".to_string()),
                projects: vec![
                    project_entry("project-one", "/tmp/project-one"),
                    project_entry("project-two", "/tmp/project-two"),
                ],
                workspaces: vec![
                    workspace_entry(
                        "project-two-worktree",
                        "project-two",
                        WorkspaceKind::Worktree,
                        "/tmp/project-two/feature",
                    ),
                    workspace_entry(
                        "project-one-primary",
                        "project-one",
                        WorkspaceKind::Primary,
                        "/tmp/project-one",
                    ),
                    workspace_entry(
                        "project-one-worktree",
                        "project-one",
                        WorkspaceKind::Worktree,
                        "/tmp/project-one/feature",
                    ),
                ],
                sessions: Vec::new(),
                presets: default_presets(),
            },
        };

        store.normalize();

        assert_eq!(store.active_workspace_id(), Some("project-one-primary"));
        assert_eq!(store.active_project_id(), Some("project-one"));
        assert_eq!(
            store
                .startup_workspace()
                .map(|workspace| workspace.id.as_str()),
            Some("project-one-primary")
        );
    }

    #[test]
    fn matches_remote_workspace_by_connection_and_path() {
        let connection = StoredSshConnection {
            host: "example.com".to_string(),
            username: Some("jun".to_string()),
            port: Some(2222),
            ..Default::default()
        };

        let workspace = WorkspaceEntry {
            id: "remote".to_string(),
            project_id: "project".to_string(),
            kind: WorkspaceKind::Primary,
            name: "remote".to_string(),
            display_name: None,
            branch: "main".to_string(),
            location: WorkspaceLocation::Ssh {
                connection: connection.clone(),
                worktree_path: "/srv/repo".to_string(),
            },
            agent_preset_id: "codex".to_string(),
            managed: false,
            git_status: WorkspaceGitStatus::Available,
            git_summary: None,
            attention_status: WorkspaceAttentionStatus::Idle,
            review_pending: false,
            last_attention_reason: None,
            created_at: Utc::now(),
            last_opened_at: Utc::now(),
        };

        let store = SuperzentStore {
            state_path: PathBuf::from("/tmp/state.json"),
            state: SuperzentState {
                active_project_id: None,
                active_workspace_id: None,
                projects: Vec::new(),
                workspaces: vec![workspace],
                sessions: Vec::new(),
                presets: default_presets(),
            },
        };

        let locator = WorkspaceLocator::Ssh {
            connection: &connection,
            worktree_path: "/srv/repo",
        };

        assert_eq!(
            store
                .workspace_for_locator(&locator)
                .map(|workspace| workspace.id.as_str()),
            Some("remote")
        );
    }
}
