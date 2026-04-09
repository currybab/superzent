use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};
use superzent_model::{
    GitChangeSummary, ProjectEntry, ProjectLocation, WorkspaceAttentionStatus, WorkspaceEntry,
    WorkspaceGitStatus, WorkspaceKind, WorkspaceLocation,
};
use uuid::Uuid;

pub const NO_GIT_BRANCH_LABEL: &str = "No Git";

#[derive(Debug)]
pub struct ProjectRegistration {
    pub project: ProjectEntry,
    pub primary_workspace: WorkspaceEntry,
}

#[derive(Debug)]
pub struct WorkspaceCreateOutcome {
    pub workspace: WorkspaceEntry,
    pub notice: Option<String>,
    pub setup_failure: Option<WorkspaceLifecycleFailure>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceBaseBranchResolution {
    pub effective_base_branch: String,
    pub notice: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct WorkspaceLifecycleDefaults {
    pub setup_script: Option<String>,
    pub teardown_script: Option<String>,
}

#[derive(Clone, Debug)]
pub struct WorkspaceLifecycleFailure {
    pub phase: WorkspaceLifecyclePhase,
    pub command: String,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl WorkspaceLifecycleFailure {
    pub fn summary(&self) -> String {
        match self.exit_code {
            Some(code) => format!(
                "{} failed while running `{}` (exit code {code}).",
                self.phase.label(),
                self.command
            ),
            None => format!(
                "{} failed while running `{}`.",
                self.phase.label(),
                self.command
            ),
        }
    }

    pub fn details(&self) -> String {
        let mut parts = vec![self.summary()];

        if !self.stdout.trim().is_empty() {
            parts.push(format!("Stdout:\n{}", self.stdout.trim()));
        }

        if !self.stderr.trim().is_empty() {
            parts.push(format!("Stderr:\n{}", self.stderr.trim()));
        }

        parts.join("\n\n")
    }
}

#[derive(Clone, Debug)]
pub enum WorkspaceDeleteOutcome {
    Deleted,
    BlockedByTeardown(WorkspaceLifecycleFailure),
}

#[derive(Debug)]
pub struct WorkspaceRefresh {
    pub branch: String,
    pub git_status: WorkspaceGitStatus,
    pub git_summary: Option<GitChangeSummary>,
}

#[derive(Clone, Debug)]
pub struct DiscoveredWorktree {
    pub path: PathBuf,
    pub branch: String,
    pub git_status: WorkspaceGitStatus,
    pub git_summary: Option<GitChangeSummary>,
}

#[derive(Debug)]
struct LocalProjectMetadata {
    project_root: PathBuf,
    branch: String,
    git_status: WorkspaceGitStatus,
    git_summary: Option<GitChangeSummary>,
}

#[derive(Clone, Debug, Default)]
pub struct CreateWorkspaceOptions {
    pub branch_name: String,
    pub base_branch_override: Option<String>,
    pub base_workspace_path: Option<PathBuf>,
    pub setup_script: Option<String>,
    pub teardown_script: Option<String>,
    pub persist_lifecycle_scripts: bool,
    pub allow_dirty: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceLifecyclePhase {
    Setup,
    Teardown,
}

impl WorkspaceLifecyclePhase {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Setup => "Setup",
            Self::Teardown => "Teardown",
        }
    }
}

#[derive(Default, Deserialize, Serialize, Clone)]
#[serde(default)]
struct SuperzentConfig {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    setup: Vec<String>,
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    teardown: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    base_branch: Option<String>,
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    copy: Vec<String>,
}

pub fn register_project(repo_hint: &Path, preset_id: &str) -> Result<ProjectRegistration> {
    let metadata = inspect_local_project(repo_hint)?;
    let name = metadata
        .project_root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("Project")
        .to_string();
    let now = Utc::now();
    let primary_workspace = WorkspaceEntry {
        id: Uuid::new_v4().to_string(),
        project_id: Uuid::new_v4().to_string(),
        kind: WorkspaceKind::Primary,
        name: name.clone(),
        display_name: None,
        branch: metadata.branch,
        location: WorkspaceLocation::Local {
            worktree_path: metadata.project_root.clone(),
        },
        agent_preset_id: preset_id.to_string(),
        managed: false,
        git_status: metadata.git_status,
        git_summary: metadata.git_summary,
        attention_status: WorkspaceAttentionStatus::Idle,
        review_pending: false,
        last_attention_reason: None,
        lifecycle_teardown_script: None,
        created_at: now,
        last_opened_at: now,
    };

    let project = ProjectEntry {
        id: primary_workspace.project_id.clone(),
        name,
        location: ProjectLocation::Local {
            repo_root: metadata.project_root,
        },
        collapsed: false,
        created_at: now,
        last_opened_at: now,
    };

    Ok(ProjectRegistration {
        project,
        primary_workspace,
    })
}

pub fn create_workspace(
    project: &ProjectEntry,
    preset_id: &str,
    options: CreateWorkspaceOptions,
) -> Result<WorkspaceCreateOutcome> {
    create_workspace_internal(project, preset_id, options, true)
}

pub fn create_workspace_without_setup(
    project: &ProjectEntry,
    preset_id: &str,
    options: CreateWorkspaceOptions,
) -> Result<WorkspaceCreateOutcome> {
    create_workspace_internal(project, preset_id, options, false)
}

fn create_workspace_internal(
    project: &ProjectEntry,
    preset_id: &str,
    options: CreateWorkspaceOptions,
    run_setup_now: bool,
) -> Result<WorkspaceCreateOutcome> {
    let Some(project_repo_root) = project.local_repo_root() else {
        bail!("cannot create a local workspace for a remote project");
    };
    let repo_root = discover_repo_root(project_repo_root)
        .context("initialize Git before creating a managed workspace")?;
    let base_workspace_path = options
        .base_workspace_path
        .as_deref()
        .unwrap_or(project_repo_root);
    if !options.allow_dirty {
        ensure_clean_worktree(base_workspace_path)?;
    }
    let repo_name = repo_root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("project");
    let configured_setup_script = normalize_command(options.setup_script.as_deref());
    let configured_teardown_script = normalize_command(options.teardown_script.as_deref());
    let config = prepare_superzent_config_for_create(
        &repo_root,
        configured_setup_script.clone(),
        configured_teardown_script.clone(),
        options.persist_lifecycle_scripts,
    )?;
    let base_branch_resolution = resolve_workspace_base_branch_for_repo(
        &repo_root,
        options.base_workspace_path.as_deref(),
        &config,
        options.base_branch_override.as_deref(),
    )?;
    let branch_name = options.branch_name.trim();
    if branch_name.is_empty() {
        bail!("branch name is required");
    }
    let branch_name = branch_name.to_string();

    let parent = repo_root.parent().unwrap_or(repo_root.as_path());
    let worktree_root = parent.join(".superzent-worktrees").join(repo_name);
    fs::create_dir_all(&worktree_root)?;

    let worktree_directory_name = unique_worktree_directory_name(&worktree_root, &branch_name);
    let worktree_path = worktree_root.join(&worktree_directory_name);

    run_git(
        &repo_root,
        &[
            "worktree",
            "add",
            "-b",
            &branch_name,
            worktree_path.to_string_lossy().as_ref(),
            &base_branch_resolution.effective_base_branch,
        ],
    )?;
    let should_persist_lifecycle_scripts = options.persist_lifecycle_scripts
        && (configured_setup_script.is_some() || configured_teardown_script.is_some());
    if should_persist_lifecycle_scripts {
        if let Err(error) = write_superzent_config(&repo_root, &config) {
            cleanup_created_worktree(&repo_root, &worktree_path, &branch_name);
            return Err(error);
        }
    }

    let setup_commands = lifecycle_commands(
        &config,
        configured_setup_script.as_deref(),
        WorkspaceLifecyclePhase::Setup,
    );
    let setup_failure = run_setup_now
        .then(|| {
            run_lifecycle_commands(
                &setup_commands,
                &repo_root,
                &worktree_path,
                options.base_workspace_path.as_deref(),
                &branch_name,
                WorkspaceLifecyclePhase::Setup,
            )
            .err()
        })
        .flatten();

    let refresh = refresh_workspace_path(&worktree_path).unwrap_or(WorkspaceRefresh {
        branch: branch_name.clone(),
        git_status: WorkspaceGitStatus::Available,
        git_summary: None,
    });

    Ok(WorkspaceCreateOutcome {
        workspace: WorkspaceEntry {
            id: Uuid::new_v4().to_string(),
            project_id: project.id.clone(),
            kind: WorkspaceKind::Worktree,
            name: branch_name,
            display_name: None,
            branch: refresh.branch,
            location: WorkspaceLocation::Local { worktree_path },
            agent_preset_id: preset_id.to_string(),
            managed: true,
            git_status: refresh.git_status,
            git_summary: refresh.git_summary,
            attention_status: WorkspaceAttentionStatus::Idle,
            review_pending: false,
            last_attention_reason: setup_failure
                .as_ref()
                .map(WorkspaceLifecycleFailure::summary)
                .or_else(|| base_branch_resolution.notice.clone()),
            lifecycle_teardown_script: if options.persist_lifecycle_scripts {
                None
            } else {
                configured_teardown_script
            },
            created_at: Utc::now(),
            last_opened_at: Utc::now(),
        },
        notice: base_branch_resolution.notice,
        setup_failure,
    })
}

pub fn run_workspace_setup(
    project: &ProjectEntry,
    workspace: &WorkspaceEntry,
    base_workspace_path: Option<&Path>,
    setup_script: Option<&str>,
) -> std::result::Result<(), WorkspaceLifecycleFailure> {
    let Some(project_repo_root) = project.local_repo_root() else {
        return Err(WorkspaceLifecycleFailure {
            phase: WorkspaceLifecyclePhase::Setup,
            command: "resolve local project".to_string(),
            exit_code: None,
            stdout: String::new(),
            stderr: "cannot run local workspace setup for a remote project".to_string(),
        });
    };
    let repo_root =
        discover_repo_root(project_repo_root).map_err(|error| WorkspaceLifecycleFailure {
            phase: WorkspaceLifecyclePhase::Setup,
            command: "resolve repo root".to_string(),
            exit_code: None,
            stdout: String::new(),
            stderr: error.to_string(),
        })?;
    let config = load_superzent_config(&repo_root).map_err(|error| WorkspaceLifecycleFailure {
        phase: WorkspaceLifecyclePhase::Setup,
        command: "load .superzent/config.json".to_string(),
        exit_code: None,
        stdout: String::new(),
        stderr: error.to_string(),
    })?;
    let setup_commands = lifecycle_commands(&config, setup_script, WorkspaceLifecyclePhase::Setup);
    if setup_commands.is_empty() {
        return Ok(());
    }
    let Some(worktree_path) = workspace.local_worktree_path() else {
        return Err(WorkspaceLifecycleFailure {
            phase: WorkspaceLifecyclePhase::Setup,
            command: "resolve local worktree".to_string(),
            exit_code: None,
            stdout: String::new(),
            stderr: "cannot run local workspace setup for a remote workspace".to_string(),
        });
    };

    run_lifecycle_commands(
        &setup_commands,
        &repo_root,
        worktree_path,
        base_workspace_path,
        &workspace.name,
        WorkspaceLifecyclePhase::Setup,
    )
}

pub fn delete_workspace(
    workspace: &WorkspaceEntry,
    repo_root: &Path,
    force: bool,
) -> Result<WorkspaceDeleteOutcome> {
    if !workspace.managed || workspace.kind == WorkspaceKind::Primary {
        return Ok(WorkspaceDeleteOutcome::Deleted);
    }
    let Some(worktree_path) = workspace.local_worktree_path() else {
        bail!("cannot delete a local workspace for a remote project");
    };
    if !worktree_path.exists() {
        return Ok(WorkspaceDeleteOutcome::Deleted);
    }

    if !force {
        let config = match load_superzent_config(repo_root) {
            Ok(config) => config,
            Err(error) if workspace.lifecycle_teardown_script.is_some() => {
                log::warn!(
                    "failed to load lifecycle config during delete; using cached teardown script: {error:#}"
                );
                SuperzentConfig::default()
            }
            Err(error) => {
                return Ok(WorkspaceDeleteOutcome::BlockedByTeardown(
                    workspace_lifecycle_failure_from_error(
                        WorkspaceLifecyclePhase::Teardown,
                        "load .superzent/config.json",
                        error,
                    ),
                ));
            }
        };
        let teardown_commands = lifecycle_commands(
            &config,
            workspace.lifecycle_teardown_script.as_deref(),
            WorkspaceLifecyclePhase::Teardown,
        );
        if let Err(failure) = run_lifecycle_commands(
            &teardown_commands,
            repo_root,
            worktree_path,
            None,
            &workspace.name,
            WorkspaceLifecyclePhase::Teardown,
        ) {
            return Ok(WorkspaceDeleteOutcome::BlockedByTeardown(failure));
        }
    }

    let mut args = vec!["worktree", "remove"];
    let force = force || workspace.git_status == WorkspaceGitStatus::Unavailable;
    if force {
        args.push("--force");
    }
    let worktree_path = worktree_path.to_string_lossy().to_string();
    args.push(worktree_path.as_str());

    match run_git(repo_root, &args) {
        Ok(()) => Ok(WorkspaceDeleteOutcome::Deleted),
        Err(error)
            if workspace.git_status == WorkspaceGitStatus::Unavailable
                || should_remove_workspace_path_after_git_failure(&error) =>
        {
            remove_workspace_path(Path::new(&worktree_path)).with_context(|| {
                format!(
                    "failed to remove workspace path after git worktree remove failed: {error:#}"
                )
            })?;
            Ok(WorkspaceDeleteOutcome::Deleted)
        }
        Err(error) => Err(error),
    }
}

pub fn resolve_workspace_base_branch(
    project: &ProjectEntry,
    base_branch_override: Option<&str>,
) -> Result<WorkspaceBaseBranchResolution> {
    resolve_workspace_base_branch_from_workspace(project, None, base_branch_override)
}

pub fn resolve_workspace_base_branch_from_workspace(
    project: &ProjectEntry,
    base_workspace_path: Option<&Path>,
    base_branch_override: Option<&str>,
) -> Result<WorkspaceBaseBranchResolution> {
    let Some(project_repo_root) = project.local_repo_root() else {
        bail!("cannot resolve a local workspace base branch for a remote project");
    };
    let repo_root = discover_repo_root(project_repo_root)
        .context("initialize Git before creating a managed workspace")?;
    let config = load_superzent_config(&repo_root)?;
    resolve_workspace_base_branch_for_repo(
        &repo_root,
        base_workspace_path,
        &config,
        base_branch_override,
    )
}

pub fn workspace_lifecycle_defaults(project: &ProjectEntry) -> Result<WorkspaceLifecycleDefaults> {
    let Some(project_repo_root) = project.local_repo_root() else {
        bail!("cannot resolve workspace lifecycle defaults for a remote project");
    };
    let repo_root = discover_repo_root(project_repo_root)
        .context("initialize Git before reading workspace lifecycle defaults")?;
    let config = load_superzent_config(&repo_root)?;
    Ok(WorkspaceLifecycleDefaults {
        setup_script: (!config.setup.is_empty()).then(|| config.setup.join("\n")),
        teardown_script: (!config.teardown.is_empty()).then(|| config.teardown.join("\n")),
    })
}

pub fn move_changes_to_workspace(
    source_repo_root: &Path,
    target_worktree_path: &Path,
) -> Result<()> {
    let stash_output = git_output(
        source_repo_root,
        &[
            "stash",
            "push",
            "--include-untracked",
            "-m",
            "superzent-move-changes",
        ],
    )?;

    if stash_output.contains("No local changes to save") {
        return Ok(());
    }

    if let Err(error) = run_git(
        target_worktree_path,
        &["stash", "pop", "--index", "stash@{0}"],
    ) {
        bail!(
            "created the workspace, but moving changes failed. Your changes were stashed as `stash@{{0}}` on the source workspace.\n{error}"
        );
    }

    Ok(())
}

pub fn refresh_workspace_path(worktree_path: &Path) -> Result<WorkspaceRefresh> {
    if discover_repo_root(worktree_path).is_ok() {
        return Ok(WorkspaceRefresh {
            branch: current_branch(worktree_path).unwrap_or_else(|| "HEAD".to_string()),
            git_status: WorkspaceGitStatus::Available,
            git_summary: git_change_summary(worktree_path).ok(),
        });
    }

    Ok(WorkspaceRefresh {
        branch: NO_GIT_BRANCH_LABEL.to_string(),
        git_status: WorkspaceGitStatus::Unavailable,
        git_summary: None,
    })
}

pub fn discover_worktrees(repo_hint: &Path) -> Result<Vec<DiscoveredWorktree>> {
    let repo_root = discover_repo_root(repo_hint)?;
    let output = git_output(&repo_root, &["worktree", "list", "--porcelain"])?;

    Ok(parse_worktree_paths(&output)
        .into_iter()
        .filter(|worktree_path| worktree_path.exists())
        .map(|worktree_path| {
            let refresh =
                refresh_workspace_path(&worktree_path).unwrap_or_else(|_| WorkspaceRefresh {
                    branch: NO_GIT_BRANCH_LABEL.to_string(),
                    git_status: WorkspaceGitStatus::Unavailable,
                    git_summary: None,
                });

            DiscoveredWorktree {
                path: worktree_path,
                branch: refresh.branch,
                git_status: refresh.git_status,
                git_summary: refresh.git_summary,
            }
        })
        .collect())
}

pub fn initialize_git_repository(project_root: &Path) -> Result<WorkspaceRefresh> {
    run_git(project_root, &["init"])?;
    refresh_workspace_path(project_root)
}

pub fn discover_repo_root(repo_hint: &Path) -> Result<PathBuf> {
    let common_dir = git_common_dir(repo_hint)?;
    if common_dir.file_name().and_then(|name| name.to_str()) == Some(".git")
        && let Some(repo_root) = common_dir.parent()
    {
        return Ok(repo_root.to_path_buf());
    }

    let output = git_output(repo_hint, &["rev-parse", "--show-toplevel"])?;
    Ok(PathBuf::from(output.trim()))
}

pub fn current_branch(repo_hint: &Path) -> Option<String> {
    git_output(repo_hint, &["branch", "--show-current"])
        .ok()
        .map(|branch| branch.trim().to_string())
        .filter(|branch| !branch.is_empty())
}

fn inspect_local_project(project_hint: &Path) -> Result<LocalProjectMetadata> {
    let canonical_path = fs::canonicalize(project_hint)
        .with_context(|| format!("failed to resolve project path {}", project_hint.display()))?;

    if let Ok(repo_root) = discover_repo_root(&canonical_path) {
        return Ok(LocalProjectMetadata {
            project_root: repo_root,
            branch: current_branch(&canonical_path).unwrap_or_else(|| "HEAD".to_string()),
            git_status: WorkspaceGitStatus::Available,
            git_summary: git_change_summary(&canonical_path).ok(),
        });
    }

    Ok(LocalProjectMetadata {
        project_root: canonical_path,
        branch: NO_GIT_BRANCH_LABEL.to_string(),
        git_status: WorkspaceGitStatus::Unavailable,
        git_summary: None,
    })
}

fn git_common_dir(repo_hint: &Path) -> Result<PathBuf> {
    let output = git_output(
        repo_hint,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    Ok(PathBuf::from(output.trim()))
}

fn parse_worktree_paths(raw_worktrees: &str) -> Vec<PathBuf> {
    raw_worktrees
        .lines()
        .filter_map(|line| line.strip_prefix("worktree ").map(PathBuf::from))
        .collect()
}

fn remove_workspace_path(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect workspace path {}", path.display()))?;

    if metadata.file_type().is_dir() {
        fs::remove_dir_all(path)
            .with_context(|| format!("failed to remove workspace directory {}", path.display()))?;
    } else {
        fs::remove_file(path)
            .with_context(|| format!("failed to remove workspace path {}", path.display()))?;
    }

    Ok(())
}

fn cleanup_created_worktree(repo_root: &Path, worktree_path: &Path, branch_name: &str) {
    let worktree_path = worktree_path.to_string_lossy().to_string();
    if let Err(error) = run_git(
        repo_root,
        &["worktree", "remove", "--force", &worktree_path],
    ) {
        log::warn!("failed to remove newly created worktree after create error: {error:#}");
        if let Err(remove_error) = remove_workspace_path(Path::new(&worktree_path)) {
            log::warn!(
                "failed to remove newly created worktree path after create error: {remove_error:#}"
            );
        }
    }
    if let Err(error) = run_git(repo_root, &["branch", "-D", branch_name]) {
        log::warn!("failed to delete newly created branch after create error: {error:#}");
    }
}

fn should_remove_workspace_path_after_git_failure(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        let message = cause.to_string();
        message.contains("not a git repository")
            || message.contains("is not a git repository")
            || message.contains("not a repository")
    })
}

fn ensure_clean_worktree(repo_hint: &Path) -> Result<()> {
    let status = git_output(repo_hint, &["status", "--porcelain"])?;
    if status.is_empty() {
        return Ok(());
    }

    bail!("commit or stash local changes before creating a managed workspace");
}

fn normalize_branch_name(branch_name: Option<&str>) -> Option<String> {
    branch_name
        .map(str::trim)
        .filter(|branch_name| !branch_name.is_empty())
        .map(ToOwned::to_owned)
}

fn normalize_command(command: Option<&str>) -> Option<String> {
    command
        .map(str::trim)
        .filter(|command| !command.is_empty())
        .map(ToOwned::to_owned)
}

fn split_commands(command_text: &str) -> Vec<String> {
    command_text
        .lines()
        .map(str::trim)
        .filter(|command| !command.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn branch_reference_exists(repo_hint: &Path, branch_name: &str) -> bool {
    git_output(repo_hint, &["rev-parse", "--verify", branch_name]).is_ok()
}

fn repository_default_branch(repo_hint: &Path) -> Option<String> {
    for (symbolic_ref, strip_prefix) in [
        ("refs/remotes/upstream/HEAD", "refs/remotes/upstream/"),
        ("refs/remotes/origin/HEAD", "refs/remotes/origin/"),
    ] {
        if let Ok(output) = git_output(repo_hint, &["symbolic-ref", symbolic_ref]) {
            if let Some(branch_name) = output.strip_prefix(strip_prefix) {
                return Some(branch_name.to_string());
            }
        }
    }

    if let Ok(configured_default_branch) = git_output(repo_hint, &["config", "init.defaultBranch"])
        && branch_reference_exists(repo_hint, &configured_default_branch)
    {
        return Some(configured_default_branch);
    }

    for branch_name in ["main", "master"] {
        if branch_reference_exists(repo_hint, branch_name) {
            return Some(branch_name.to_string());
        }
    }

    None
}

fn resolve_workspace_base_branch_for_repo(
    repo_root: &Path,
    base_workspace_path: Option<&Path>,
    config: &SuperzentConfig,
    base_branch_override: Option<&str>,
) -> Result<WorkspaceBaseBranchResolution> {
    if let Some(base_branch_override) = normalize_branch_name(base_branch_override) {
        if !branch_reference_exists(repo_root, &base_branch_override) {
            bail!("base branch `{base_branch_override}` was not found");
        }

        return Ok(WorkspaceBaseBranchResolution {
            effective_base_branch: base_branch_override,
            notice: None,
        });
    }

    let current_base_workspace_branch = current_branch(base_workspace_path.unwrap_or(repo_root));
    let repository_default_branch = repository_default_branch(repo_root);

    let configured_base_branch = normalize_branch_name(config.base_branch.as_deref());
    if let Some(configured_base_branch) = configured_base_branch {
        if branch_reference_exists(repo_root, &configured_base_branch) {
            return Ok(WorkspaceBaseBranchResolution {
                effective_base_branch: configured_base_branch,
                notice: None,
            });
        }

        if let Some(current_base_workspace_branch) = current_base_workspace_branch.clone() {
            return Ok(WorkspaceBaseBranchResolution {
                effective_base_branch: current_base_workspace_branch.clone(),
                notice: Some(format!(
                    "Configured base branch `{configured_base_branch}` was not found. Using the base workspace current branch `{current_base_workspace_branch}`."
                )),
            });
        }

        let repository_default_branch = repository_default_branch.ok_or_else(|| {
            anyhow::anyhow!(
                "could not determine the repository default branch; configure `.superzent/config.json` `base_branch` or choose an override"
            )
        })?;
        return Ok(WorkspaceBaseBranchResolution {
            effective_base_branch: repository_default_branch.clone(),
            notice: Some(format!(
                "Configured base branch `{configured_base_branch}` was not found. The base workspace has no current branch, so using repository default `{repository_default_branch}`."
            )),
        });
    }

    if let Some(current_base_workspace_branch) = current_base_workspace_branch {
        return Ok(WorkspaceBaseBranchResolution {
            effective_base_branch: current_base_workspace_branch,
            notice: None,
        });
    }

    let repository_default_branch = repository_default_branch.ok_or_else(|| {
        anyhow::anyhow!(
            "could not determine the repository default branch; configure `.superzent/config.json` `base_branch` or choose an override"
        )
    })?;

    Ok(WorkspaceBaseBranchResolution {
        effective_base_branch: repository_default_branch,
        notice: None,
    })
}

fn git_change_summary(repo_hint: &Path) -> Result<GitChangeSummary> {
    let status = git_output(repo_hint, &["status", "--porcelain"])?;
    let mut summary = GitChangeSummary::default();

    for line in status.lines() {
        if line.is_empty() {
            continue;
        }
        summary.changed_files += 1;
        if line.starts_with("??") {
            summary.untracked_files += 1;
        }
        let stage = line.chars().next().unwrap_or(' ');
        if stage != ' ' && stage != '?' {
            summary.staged_files += 1;
        }
    }

    let (added_lines, deleted_lines) = git_diff_line_summary(repo_hint)?;
    summary.added_lines = added_lines;
    summary.deleted_lines = deleted_lines;

    let (ahead_commits, behind_commits) = git_tracking_summary(repo_hint);
    summary.ahead_commits = ahead_commits;
    summary.behind_commits = behind_commits;

    Ok(summary)
}

fn git_has_head(repo_hint: &Path) -> bool {
    git_output(repo_hint, &["rev-parse", "--verify", "HEAD"]).is_ok()
}

fn git_diff_line_summary(repo_hint: &Path) -> Result<(usize, usize)> {
    if !git_has_head(repo_hint) {
        return Ok((0, 0));
    }

    let output = git_output(repo_hint, &["diff", "--numstat", "--no-renames", "HEAD"])?;
    let mut added_lines = 0usize;
    let mut deleted_lines = 0usize;

    for line in output.lines() {
        let mut parts = line.splitn(3, '\t');
        let (Some(added), Some(deleted), Some(_path)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let (Ok(added), Ok(deleted)) = (added.parse::<usize>(), deleted.parse::<usize>()) else {
            continue;
        };
        added_lines += added;
        deleted_lines += deleted;
    }

    Ok((added_lines, deleted_lines))
}

fn git_tracking_summary(repo_hint: &Path) -> (usize, usize) {
    let Ok(output) = git_output(
        repo_hint,
        &["rev-list", "--left-right", "--count", "@{upstream}...HEAD"],
    ) else {
        return (0, 0);
    };

    let mut counts = output.split_whitespace();
    let Some(behind_commits) = counts.next().and_then(|count| count.parse::<usize>().ok()) else {
        return (0, 0);
    };
    let Some(ahead_commits) = counts.next().and_then(|count| count.parse::<usize>().ok()) else {
        return (0, 0);
    };

    (ahead_commits, behind_commits)
}

fn run_git(repo_root: &Path, args: &[&str]) -> Result<()> {
    git_output(repo_root, args).map(|_| ())
}

fn run_command_output(
    mut command: smol::process::Command,
    failure_context: impl FnOnce() -> String,
) -> Result<std::process::Output> {
    smol::block_on(command.output()).with_context(failure_context)
}

fn git_output(repo_root: &Path, args: &[&str]) -> Result<String> {
    let mut command = smol::process::Command::new("git");
    command.arg("-C").arg(repo_root).args(args);
    let output = run_command_output(command, || format!("failed to run git {}", args.join(" ")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git {} failed: {}", args.join(" "), stderr.trim());
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn lifecycle_commands(
    config: &SuperzentConfig,
    command_override: Option<&str>,
    phase: WorkspaceLifecyclePhase,
) -> Vec<String> {
    if let Some(command_override) = normalize_command(command_override) {
        return split_commands(&command_override);
    }

    match phase {
        WorkspaceLifecyclePhase::Setup => config.setup.clone(),
        WorkspaceLifecyclePhase::Teardown => config.teardown.clone(),
    }
}

fn run_lifecycle_commands(
    commands: &[String],
    repo_root: &Path,
    worktree_path: &Path,
    base_workspace_path: Option<&Path>,
    workspace_name: &str,
    phase: WorkspaceLifecyclePhase,
) -> std::result::Result<(), WorkspaceLifecycleFailure> {
    for command in commands {
        run_shell_command(
            command,
            worktree_path,
            base_workspace_path,
            workspace_name,
            repo_root,
            phase,
        )?;
    }

    Ok(())
}

fn prepare_superzent_config_for_create(
    repo_root: &Path,
    setup_script: Option<String>,
    teardown_script: Option<String>,
    persist_lifecycle_scripts: bool,
) -> Result<SuperzentConfig> {
    let mut config = load_superzent_config(repo_root)?;
    if persist_lifecycle_scripts {
        if let Some(setup_script) = setup_script {
            config.setup = split_commands(&setup_script);
        }
        if let Some(teardown_script) = teardown_script {
            config.teardown = split_commands(&teardown_script);
        }
    }
    Ok(config)
}

fn load_superzent_config(repo_root: &Path) -> Result<SuperzentConfig> {
    let config_path = repo_root.join(".superzent").join("config.json");
    if !config_path.exists() {
        return Ok(SuperzentConfig::default());
    }

    let contents = fs::read_to_string(&config_path)
        .with_context(|| format!("failed to read {}", config_path.display()))?;
    let config: SuperzentConfig = serde_json::from_str(&contents)
        .with_context(|| format!("failed to parse {}", config_path.display()))?;
    if !config.copy.is_empty() {
        bail!(
            "`.superzent/config.json` `copy` is no longer supported; move this logic into `setup` commands"
        );
    }

    Ok(config)
}

fn write_superzent_config(repo_root: &Path, config: &SuperzentConfig) -> Result<()> {
    let superzent_directory = repo_root.join(".superzent");
    fs::create_dir_all(&superzent_directory)
        .with_context(|| format!("failed to create {}", superzent_directory.display()))?;
    let config_path = superzent_directory.join("config.json");
    let contents = serde_json::to_string_pretty(config)
        .context("failed to serialize .superzent/config.json")?;
    fs::write(&config_path, contents)
        .with_context(|| format!("failed to write {}", config_path.display()))
}

fn run_shell_command(
    command: &str,
    cwd: &Path,
    base_workspace_path: Option<&Path>,
    workspace_name: &str,
    repo_root: &Path,
    phase: WorkspaceLifecyclePhase,
) -> std::result::Result<(), WorkspaceLifecycleFailure> {
    let mut shell_command = smol::process::Command::new("/bin/zsh");
    shell_command
        .arg("-lc")
        .arg(command)
        .current_dir(cwd)
        .env("SUPERZENT_ROOT_PATH", repo_root)
        .env("SUPERZENT_WORKTREE_PATH", cwd)
        .envs(
            base_workspace_path
                .map(|base_workspace_path| {
                    [
                        ("SUPERZENT_BASE_PATH", base_workspace_path.as_os_str()),
                        (
                            "SUPERZENT_SOURCE_WORKSPACE_PATH",
                            base_workspace_path.as_os_str(),
                        ),
                    ]
                })
                .into_iter()
                .flatten(),
        )
        .env("SUPERZENT_WORKSPACE_NAME", workspace_name)
        .env("SUPERSET_WORKSPACE_NAME", workspace_name)
        .env("SUPERSET_ROOT_PATH", repo_root);
    let output = run_command_output(shell_command, || {
        format!("failed to run hook command: {command}")
    })
    .map_err(|error| WorkspaceLifecycleFailure {
        phase,
        command: command.to_string(),
        exit_code: None,
        stdout: String::new(),
        stderr: error.to_string(),
    })?;

    if !output.status.success() {
        return Err(WorkspaceLifecycleFailure {
            phase,
            command: command.to_string(),
            exit_code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).trim().to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }

    Ok(())
}

fn workspace_lifecycle_failure_from_error(
    phase: WorkspaceLifecyclePhase,
    command: &str,
    error: anyhow::Error,
) -> WorkspaceLifecycleFailure {
    WorkspaceLifecycleFailure {
        phase,
        command: command.to_string(),
        exit_code: None,
        stdout: String::new(),
        stderr: error.to_string(),
    }
}

fn unique_worktree_directory_name(worktree_root: &Path, branch_name: &str) -> String {
    let base = slugify(branch_name);
    if !worktree_root.join(&base).exists() {
        return base;
    }

    for attempt in 1..100 {
        let candidate = format!("{base}-{attempt}");
        if !worktree_root.join(&candidate).exists() {
            return candidate;
        }
    }

    format!("{}-{}", base, &Uuid::new_v4().to_string()[..8])
}

fn slugify(input: &str) -> String {
    let slug = input
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect::<String>();
    let slug = slug.trim_matches('-').to_lowercase();
    if slug.is_empty() {
        "workspace".into()
    } else {
        slug
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn create_workspace_options(branch_name: &str) -> CreateWorkspaceOptions {
        CreateWorkspaceOptions {
            branch_name: branch_name.to_string(),
            base_branch_override: None,
            base_workspace_path: None,
            setup_script: None,
            teardown_script: None,
            persist_lifecycle_scripts: false,
            allow_dirty: false,
        }
    }

    fn assert_deleted(outcome: WorkspaceDeleteOutcome, context: &str) {
        assert!(
            matches!(outcome, WorkspaceDeleteOutcome::Deleted),
            "{context}"
        );
    }

    #[test]
    fn discover_repo_root_uses_common_git_dir_for_linked_worktrees() {
        let repo = init_repo();
        let worktree_path = repo.repo_path.parent().unwrap().join("feature-worktree");

        git(
            &repo.repo_path,
            &[
                "worktree",
                "add",
                "-b",
                "feature/superzent-test",
                worktree_path.to_str().unwrap(),
                "HEAD",
            ],
        );

        let discovered = discover_repo_root(&worktree_path).unwrap();

        assert_eq!(discovered, repo.repo_path);
    }

    #[test]
    fn create_workspace_blocks_dirty_worktrees() {
        let repo = init_repo();
        fs::write(repo.repo_path.join("dirty.txt"), "dirty\n").unwrap();
        let registration = register_project(&repo.repo_path, "codex").unwrap();

        let error = create_workspace(
            &registration.project,
            "codex",
            create_workspace_options("feature/dirty-worktree"),
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("commit or stash local changes before creating a managed workspace")
        );
    }

    #[test]
    fn create_workspace_can_override_dirty_worktree_guard() {
        let repo = init_repo();
        fs::write(repo.repo_path.join("dirty.txt"), "dirty\n").unwrap();
        let registration = register_project(&repo.repo_path, "codex").unwrap();

        let outcome = create_workspace(
            &registration.project,
            "codex",
            CreateWorkspaceOptions {
                branch_name: "feature/dirty-override".to_string(),
                base_branch_override: None,
                base_workspace_path: None,
                setup_script: None,
                teardown_script: None,
                persist_lifecycle_scripts: false,
                allow_dirty: true,
            },
        )
        .unwrap();

        assert!(
            outcome
                .workspace
                .local_worktree_path()
                .expect("workspace should be local")
                .exists()
        );

        assert_deleted(
            delete_workspace(&outcome.workspace, repo.repo_path.as_path(), true).unwrap(),
            "dirty override workspace should be deleted",
        );
    }

    #[test]
    fn create_workspace_blocks_dirty_active_base_workspace() {
        let repo = init_repo();
        let secondary_worktree_path = repo.repo_path.parent().unwrap().join("dirty-secondary");
        git(
            &repo.repo_path,
            &[
                "worktree",
                "add",
                "-b",
                "feature/dirty-secondary-source",
                secondary_worktree_path.to_str().unwrap(),
                "main",
            ],
        );
        fs::write(secondary_worktree_path.join("dirty.txt"), "dirty\n").unwrap();

        let registration = register_project(&repo.repo_path, "codex").unwrap();
        let error = create_workspace(
            &registration.project,
            "codex",
            CreateWorkspaceOptions {
                branch_name: "feature/dirty-secondary-target".to_string(),
                base_branch_override: None,
                base_workspace_path: Some(secondary_worktree_path.clone()),
                setup_script: None,
                teardown_script: None,
                persist_lifecycle_scripts: false,
                allow_dirty: false,
            },
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("commit or stash local changes before creating a managed workspace")
        );

        git(
            &repo.repo_path,
            &[
                "worktree",
                "remove",
                "--force",
                secondary_worktree_path.to_str().unwrap(),
            ],
        );
    }

    #[test]
    fn prepare_superzent_config_for_create_splits_multiline_scripts() {
        let repo = init_repo();

        let config = prepare_superzent_config_for_create(
            &repo.repo_path,
            Some("echo one\necho two".to_string()),
            Some("cargo clean\ncargo test".to_string()),
            true,
        )
        .unwrap();

        assert_eq!(
            config.setup,
            vec!["echo one".to_string(), "echo two".to_string()]
        );
        assert_eq!(
            config.teardown,
            vec!["cargo clean".to_string(), "cargo test".to_string()]
        );
    }

    #[test]
    fn create_workspace_persists_lifecycle_scripts_when_requested() {
        let repo = init_repo();
        let registration = register_project(&repo.repo_path, "codex").unwrap();
        let outcome = create_workspace(
            &registration.project,
            "codex",
            CreateWorkspaceOptions {
                branch_name: "feature/persisted-scripts".to_string(),
                base_branch_override: None,
                base_workspace_path: Some(repo.repo_path.clone()),
                setup_script: Some("echo setup one\necho setup two".to_string()),
                teardown_script: Some("echo teardown".to_string()),
                persist_lifecycle_scripts: true,
                allow_dirty: false,
            },
        )
        .unwrap();

        let config = load_superzent_config(&repo.repo_path).unwrap();
        assert_eq!(
            config.setup,
            vec!["echo setup one".to_string(), "echo setup two".to_string()]
        );
        assert_eq!(config.teardown, vec!["echo teardown".to_string()]);

        assert_deleted(
            delete_workspace(&outcome.workspace, repo.repo_path.as_path(), true).unwrap(),
            "persisted-scripts workspace should be deleted",
        );
    }

    #[test]
    fn create_workspace_does_not_persist_config_when_validation_fails() {
        let repo = init_repo();
        let registration = register_project(&repo.repo_path, "codex").unwrap();
        let error = create_workspace(
            &registration.project,
            "codex",
            CreateWorkspaceOptions {
                branch_name: "feature/persist-validation-fail".to_string(),
                base_branch_override: Some("missing".to_string()),
                base_workspace_path: Some(repo.repo_path.clone()),
                setup_script: Some("echo setup".to_string()),
                teardown_script: Some("echo teardown".to_string()),
                persist_lifecycle_scripts: true,
                allow_dirty: false,
            },
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("base branch `missing` was not found")
        );
        assert!(
            !repo
                .repo_path
                .join(".superzent")
                .join("config.json")
                .exists()
        );
    }

    #[test]
    fn create_workspace_cleans_up_worktree_when_persisted_config_write_fails() {
        let repo = init_repo();
        fs::write(repo.repo_path.join(".superzent"), "not a directory").unwrap();
        let registration = register_project(&repo.repo_path, "codex").unwrap();
        let error = create_workspace(
            &registration.project,
            "codex",
            CreateWorkspaceOptions {
                branch_name: "feature/persist-write-fail".to_string(),
                base_branch_override: None,
                base_workspace_path: Some(repo.repo_path.clone()),
                setup_script: Some("echo setup".to_string()),
                teardown_script: None,
                persist_lifecycle_scripts: true,
                allow_dirty: false,
            },
        )
        .unwrap_err();

        assert!(!error.to_string().is_empty());
        let expected_worktree_path = repo
            .repo_path
            .parent()
            .unwrap()
            .join(".superzent-worktrees")
            .join("repo")
            .join("feature-persist-write-fail");
        assert!(!expected_worktree_path.exists());
        assert!(!branch_reference_exists(
            &repo.repo_path,
            "feature/persist-write-fail"
        ));
    }

    #[test]
    fn move_changes_to_workspace_moves_tracked_and_untracked_changes() {
        let repo = init_repo();
        fs::write(repo.repo_path.join("tracked.txt"), "tracked\n").unwrap();
        fs::write(repo.repo_path.join("untracked.txt"), "untracked\n").unwrap();
        git(&repo.repo_path, &["add", "tracked.txt"]);

        let registration = register_project(&repo.repo_path, "codex").unwrap();
        let outcome = create_workspace(
            &registration.project,
            "codex",
            CreateWorkspaceOptions {
                branch_name: "feature/move-changes".to_string(),
                base_branch_override: None,
                base_workspace_path: Some(repo.repo_path.clone()),
                setup_script: None,
                teardown_script: None,
                persist_lifecycle_scripts: false,
                allow_dirty: true,
            },
        )
        .unwrap();

        let worktree_path = outcome
            .workspace
            .local_worktree_path()
            .expect("workspace should be local")
            .to_path_buf();

        move_changes_to_workspace(&repo.repo_path, &worktree_path).unwrap();

        assert!(!repo.repo_path.join("tracked.txt").exists());
        assert!(!repo.repo_path.join("untracked.txt").exists());
        assert!(worktree_path.join("tracked.txt").exists());
        assert!(worktree_path.join("untracked.txt").exists());

        let source_status = git_output(&repo.repo_path, &["status", "--porcelain"]).unwrap();
        assert!(source_status.is_empty());

        assert_deleted(
            delete_workspace(&outcome.workspace, repo.repo_path.as_path(), true).unwrap(),
            "move-changes workspace should be deleted",
        );
    }

    #[test]
    fn create_workspace_from_linked_worktree_uses_primary_repo_root() {
        let repo = init_repo();
        let worktree_path = repo.repo_path.parent().unwrap().join("feature-worktree");

        git(
            &repo.repo_path,
            &[
                "worktree",
                "add",
                "-b",
                "feature/superzent-test",
                worktree_path.to_str().unwrap(),
                "HEAD",
            ],
        );

        let registration = register_project(&worktree_path, "codex").unwrap();
        let outcome = create_workspace(
            &registration.project,
            "codex",
            create_workspace_options("feature/linked-root"),
        )
        .unwrap();
        let expected_root = repo
            .repo_path
            .parent()
            .unwrap()
            .join(".superzent-worktrees")
            .join("repo");

        assert_eq!(
            registration.project.local_repo_root(),
            Some(repo.repo_path.as_path())
        );
        assert!(
            outcome
                .workspace
                .local_worktree_path()
                .is_some_and(|worktree_path| worktree_path.starts_with(&expected_root))
        );

        assert_deleted(
            delete_workspace(&outcome.workspace, repo.repo_path.as_path(), true).unwrap(),
            "linked-root workspace should be deleted",
        );
    }

    #[test]
    fn create_workspace_uses_active_base_workspace_branch_when_provided() {
        let repo = init_repo();
        git(&repo.repo_path, &["checkout", "-b", "develop"]);
        fs::write(repo.repo_path.join("develop.txt"), "from develop\n").unwrap();
        commit_all_changes(&repo.repo_path, "add develop marker");
        git(&repo.repo_path, &["checkout", "main"]);

        let secondary_worktree_path = repo.repo_path.parent().unwrap().join("develop-worktree");
        git(
            &repo.repo_path,
            &[
                "worktree",
                "add",
                secondary_worktree_path.to_str().unwrap(),
                "develop",
            ],
        );

        let registration = register_project(&repo.repo_path, "codex").unwrap();
        let outcome = create_workspace(
            &registration.project,
            "codex",
            CreateWorkspaceOptions {
                branch_name: "feature/from-secondary-base".to_string(),
                base_branch_override: None,
                base_workspace_path: Some(secondary_worktree_path.clone()),
                setup_script: None,
                teardown_script: None,
                persist_lifecycle_scripts: false,
                allow_dirty: false,
            },
        )
        .unwrap();

        assert!(
            outcome
                .workspace
                .local_worktree_path()
                .expect("workspace should be local")
                .join("develop.txt")
                .exists()
        );

        assert_deleted(
            delete_workspace(&outcome.workspace, repo.repo_path.as_path(), true).unwrap(),
            "secondary-base workspace should be deleted",
        );
        git(
            &repo.repo_path,
            &[
                "worktree",
                "remove",
                "--force",
                secondary_worktree_path.to_str().unwrap(),
            ],
        );
    }

    #[test]
    fn create_workspace_uses_requested_branch_name_and_sanitized_worktree_path() {
        let repo = init_repo();
        let registration = register_project(&repo.repo_path, "codex").unwrap();
        let outcome = create_workspace(
            &registration.project,
            "codex",
            create_workspace_options("feature/superzent-test"),
        )
        .unwrap();

        assert_eq!(outcome.workspace.name, "feature/superzent-test");
        assert_eq!(outcome.workspace.branch, "feature/superzent-test");
        assert_eq!(
            outcome
                .workspace
                .local_worktree_path()
                .expect("workspace should be local")
                .file_name()
                .and_then(|name| name.to_str()),
            Some("feature-superzent-test")
        );

        assert_deleted(
            delete_workspace(&outcome.workspace, repo.repo_path.as_path(), true).unwrap(),
            "superzent-test workspace should be deleted",
        );
    }

    #[test]
    fn create_workspace_uses_configured_base_branch_instead_of_current_branch() {
        let repo = init_repo();
        fs::create_dir_all(repo.repo_path.join(".superzent")).unwrap();
        fs::write(
            repo.repo_path.join(".superzent").join("config.json"),
            r#"{"base_branch":"develop"}"#,
        )
        .unwrap();
        commit_all_changes(&repo.repo_path, "add lifecycle config");

        git(&repo.repo_path, &["checkout", "-b", "develop"]);
        fs::write(repo.repo_path.join("develop.txt"), "from develop\n").unwrap();
        commit_all_changes(&repo.repo_path, "add develop marker");
        git(
            &repo.repo_path,
            &["checkout", "-b", "feature/current", "main"],
        );

        let registration = register_project(&repo.repo_path, "codex").unwrap();
        let outcome = create_workspace(
            &registration.project,
            "codex",
            create_workspace_options("feature/configured-base"),
        )
        .unwrap();

        assert!(
            outcome
                .workspace
                .local_worktree_path()
                .expect("workspace should be local")
                .join("develop.txt")
                .exists()
        );

        assert_deleted(
            delete_workspace(&outcome.workspace, repo.repo_path.as_path(), true).unwrap(),
            "configured-base workspace should be deleted",
        );
    }

    #[test]
    fn register_project_supports_plain_local_folders() {
        let temp_dir = TempDir::new().unwrap();
        let project_path = temp_dir.path().join("plain-project");
        fs::create_dir_all(&project_path).unwrap();
        fs::write(project_path.join("README.txt"), "hello\n").unwrap();
        let project_path = project_path.canonicalize().unwrap();

        let registration = register_project(&project_path, "codex").unwrap();

        assert_eq!(
            registration.project.local_repo_root(),
            Some(project_path.as_path())
        );
        assert_eq!(
            registration.primary_workspace.local_worktree_path(),
            Some(project_path.as_path())
        );
        assert_eq!(registration.primary_workspace.branch, NO_GIT_BRANCH_LABEL);
        assert_eq!(
            registration.primary_workspace.git_status,
            WorkspaceGitStatus::Unavailable
        );
        assert!(registration.primary_workspace.git_summary.is_none());
    }

    #[test]
    fn initialize_git_repository_upgrades_plain_local_folders() {
        let temp_dir = TempDir::new().unwrap();
        let project_path = temp_dir.path().join("plain-project");
        fs::create_dir_all(&project_path).unwrap();
        let project_path = project_path.canonicalize().unwrap();

        let refresh = initialize_git_repository(&project_path).unwrap();

        assert_ne!(refresh.branch, NO_GIT_BRANCH_LABEL);
        assert_eq!(refresh.git_status, WorkspaceGitStatus::Available);
        assert!(refresh.git_summary.is_some());
        assert!(project_path.join(".git").exists());
    }

    #[test]
    fn git_change_summary_collects_line_counts_from_head_diff() {
        let repo = init_repo();
        fs::write(repo.repo_path.join("README.md"), "hi\nthere\n").unwrap();

        let summary = git_change_summary(&repo.repo_path).unwrap();

        assert_eq!(summary.added_lines, 2);
        assert_eq!(summary.deleted_lines, 1);
    }

    #[test]
    fn git_change_summary_defaults_tracking_counts_without_upstream() {
        let repo = init_repo();
        fs::write(repo.repo_path.join("README.md"), "hi\nthere\n").unwrap();

        let summary = git_change_summary(&repo.repo_path).unwrap();

        assert_eq!(summary.ahead_commits, 0);
        assert_eq!(summary.behind_commits, 0);
    }

    #[test]
    fn git_change_summary_collects_upstream_tracking_counts() {
        let repo = init_repo();
        let remote_path = repo.repo_path.parent().unwrap().join("remote.git");
        let other_clone_path = repo.repo_path.parent().unwrap().join("other-clone");

        git(
            remote_path.parent().unwrap(),
            &[
                "init",
                "--bare",
                "--initial-branch=main",
                remote_path.to_str().unwrap(),
            ],
        );
        git(
            &repo.repo_path,
            &["remote", "add", "origin", remote_path.to_str().unwrap()],
        );
        git(&repo.repo_path, &["push", "-u", "origin", "main"]);

        fs::write(repo.repo_path.join("README.md"), "hello\nlocal\n").unwrap();
        git(&repo.repo_path, &["commit", "-am", "local change"]);

        git(
            repo.repo_path.parent().unwrap(),
            &[
                "clone",
                remote_path.to_str().unwrap(),
                other_clone_path.to_str().unwrap(),
            ],
        );
        git(
            &other_clone_path,
            &["config", "user.name", "Superzent Tests"],
        );
        git(
            &other_clone_path,
            &["config", "user.email", "tests@superzent.dev"],
        );
        fs::write(other_clone_path.join("README.md"), "hello\nremote\n").unwrap();
        git(&other_clone_path, &["commit", "-am", "remote change"]);
        git(&other_clone_path, &["push", "origin", "main"]);
        git(&repo.repo_path, &["fetch", "origin"]);

        let summary = git_change_summary(&repo.repo_path).unwrap();

        assert_eq!(summary.ahead_commits, 1);
        assert_eq!(summary.behind_commits, 1);
    }

    #[test]
    fn create_workspace_override_base_branch_wins_over_configured_base_branch() {
        let repo = init_repo();
        fs::create_dir_all(repo.repo_path.join(".superzent")).unwrap();
        fs::write(
            repo.repo_path.join(".superzent").join("config.json"),
            r#"{"base_branch":"develop"}"#,
        )
        .unwrap();
        commit_all_changes(&repo.repo_path, "add lifecycle config");

        git(&repo.repo_path, &["checkout", "-b", "develop"]);
        fs::write(repo.repo_path.join("develop.txt"), "from develop\n").unwrap();
        commit_all_changes(&repo.repo_path, "add develop marker");
        git(
            &repo.repo_path,
            &["checkout", "-b", "feature/current", "main"],
        );

        let registration = register_project(&repo.repo_path, "codex").unwrap();
        let outcome = create_workspace(
            &registration.project,
            "codex",
            CreateWorkspaceOptions {
                branch_name: "feature/override-base".to_string(),
                base_branch_override: Some("main".to_string()),
                base_workspace_path: None,
                setup_script: None,
                teardown_script: None,
                persist_lifecycle_scripts: false,
                allow_dirty: false,
            },
        )
        .unwrap();

        assert!(
            !outcome
                .workspace
                .local_worktree_path()
                .expect("workspace should be local")
                .join("develop.txt")
                .exists()
        );

        assert_deleted(
            delete_workspace(&outcome.workspace, repo.repo_path.as_path(), true).unwrap(),
            "override-base workspace should be deleted",
        );
    }

    #[test]
    fn create_workspace_rejects_invalid_base_branch_override() {
        let repo = init_repo();
        let registration = register_project(&repo.repo_path, "codex").unwrap();
        let error = create_workspace(
            &registration.project,
            "codex",
            CreateWorkspaceOptions {
                branch_name: "feature/invalid-override".to_string(),
                base_branch_override: Some("missing".to_string()),
                base_workspace_path: None,
                setup_script: None,
                teardown_script: None,
                persist_lifecycle_scripts: false,
                allow_dirty: false,
            },
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("base branch `missing` was not found")
        );
    }

    #[test]
    fn resolve_workspace_base_branch_prefers_current_base_workspace_branch_when_config_is_absent() {
        let repo = init_repo();
        git(&repo.repo_path, &["checkout", "-b", "develop"]);

        let registration = register_project(&repo.repo_path, "codex").unwrap();
        let resolution = resolve_workspace_base_branch(&registration.project, None).unwrap();

        assert_eq!(resolution.effective_base_branch, "develop");
        assert!(resolution.notice.is_none());
    }

    #[test]
    fn resolve_workspace_base_branch_falls_back_to_current_base_workspace_branch_when_configured_branch_is_missing()
     {
        let repo = init_repo();
        fs::create_dir_all(repo.repo_path.join(".superzent")).unwrap();
        fs::write(
            repo.repo_path.join(".superzent").join("config.json"),
            r#"{"base_branch":"missing"}"#,
        )
        .unwrap();
        commit_all_changes(&repo.repo_path, "add lifecycle config");
        git(&repo.repo_path, &["checkout", "-b", "develop"]);

        let registration = register_project(&repo.repo_path, "codex").unwrap();
        let resolution = resolve_workspace_base_branch(&registration.project, None).unwrap();

        assert_eq!(resolution.effective_base_branch, "develop");
        assert!(
            resolution
                .notice
                .as_deref()
                .is_some_and(|notice| notice.contains("base workspace current branch `develop`"))
        );
    }

    #[test]
    fn create_workspace_rejects_deprecated_copy_config() {
        let repo = init_repo();
        fs::create_dir_all(repo.repo_path.join(".superzent")).unwrap();
        fs::write(
            repo.repo_path.join(".superzent").join("config.json"),
            r#"{"copy":["templates"]}"#,
        )
        .unwrap();
        commit_all_changes(&repo.repo_path, "add deprecated copy config");

        let registration = register_project(&repo.repo_path, "codex").unwrap();
        let error = create_workspace(
            &registration.project,
            "codex",
            create_workspace_options("feature/deprecated-copy"),
        )
        .unwrap_err();

        assert!(error.to_string().contains("`copy` is no longer supported"));
    }

    #[test]
    fn create_workspace_runs_setup_commands_with_superzent_and_legacy_env_vars() {
        let repo = init_repo();
        fs::create_dir_all(repo.repo_path.join(".superzent")).unwrap();
        fs::write(
            repo.repo_path.join(".superzent").join("config.json"),
            r#"{"setup":["printf '%s\n%s\n%s\n%s\n%s\n%s' \"$SUPERZENT_ROOT_PATH\" \"$SUPERZENT_WORKTREE_PATH\" \"$SUPERZENT_BASE_PATH\" \"$SUPERZENT_WORKSPACE_NAME\" \"$SUPERSET_ROOT_PATH\" \"$SUPERSET_WORKSPACE_NAME\" > lifecycle-env.txt"]}"#,
        )
        .unwrap();
        commit_all_changes(&repo.repo_path, "add setup env config");

        let registration = register_project(&repo.repo_path, "codex").unwrap();
        let outcome = create_workspace(
            &registration.project,
            "codex",
            CreateWorkspaceOptions {
                branch_name: "feature/setup-env".to_string(),
                base_branch_override: None,
                base_workspace_path: Some(repo.repo_path.clone()),
                setup_script: None,
                teardown_script: None,
                persist_lifecycle_scripts: false,
                allow_dirty: false,
            },
        )
        .unwrap();

        let worktree_path = outcome
            .workspace
            .local_worktree_path()
            .expect("workspace should be local");
        let env_file = fs::read_to_string(worktree_path.join("lifecycle-env.txt")).unwrap();
        let env_lines = env_file.lines().collect::<Vec<_>>();
        assert_eq!(env_lines[0], repo.repo_path.to_string_lossy());
        assert_eq!(env_lines[1], worktree_path.to_string_lossy());
        assert_eq!(env_lines[2], repo.repo_path.to_string_lossy());
        assert_eq!(env_lines[3], "feature/setup-env");
        assert_eq!(env_lines[4], repo.repo_path.to_string_lossy());
        assert_eq!(env_lines[5], "feature/setup-env");

        assert_deleted(
            delete_workspace(&outcome.workspace, repo.repo_path.as_path(), true).unwrap(),
            "setup-env workspace should be deleted",
        );
    }

    #[test]
    fn create_workspace_keeps_worktree_when_setup_fails() {
        let repo = init_repo();
        fs::create_dir_all(repo.repo_path.join(".superzent")).unwrap();
        fs::write(
            repo.repo_path.join(".superzent").join("config.json"),
            r#"{"setup":["echo partial > setup-output.txt","false"]}"#,
        )
        .unwrap();
        commit_all_changes(&repo.repo_path, "add failing setup config");

        let registration = register_project(&repo.repo_path, "codex").unwrap();
        let outcome = create_workspace(
            &registration.project,
            "codex",
            create_workspace_options("feature/setup-failure"),
        )
        .unwrap();

        let worktree_path = outcome
            .workspace
            .local_worktree_path()
            .expect("workspace should be local");
        assert!(worktree_path.exists());
        assert!(worktree_path.join("setup-output.txt").exists());
        let setup_failure = outcome
            .setup_failure
            .expect("setup failure should be captured");
        assert_eq!(setup_failure.phase, WorkspaceLifecyclePhase::Setup);
        assert!(setup_failure.summary().contains("Setup failed"));

        assert_deleted(
            delete_workspace(&outcome.workspace, repo.repo_path.as_path(), true).unwrap(),
            "setup-failure workspace should be deleted",
        );
    }

    #[test]
    fn run_workspace_setup_can_copy_from_superzent_base_path() {
        let repo = init_repo();
        fs::write(repo.repo_path.join(".env"), "API_KEY=test\n").unwrap();
        let registration = register_project(&repo.repo_path, "codex").unwrap();
        let outcome = create_workspace_without_setup(
            &registration.project,
            "codex",
            CreateWorkspaceOptions {
                branch_name: "feature/base-path-copy".to_string(),
                base_branch_override: None,
                base_workspace_path: Some(repo.repo_path.clone()),
                setup_script: Some("cp \"$SUPERZENT_BASE_PATH\"/.env .env".to_string()),
                teardown_script: None,
                persist_lifecycle_scripts: false,
                allow_dirty: true,
            },
        )
        .unwrap();

        run_workspace_setup(
            &registration.project,
            &outcome.workspace,
            Some(repo.repo_path.as_path()),
            Some("cp \"$SUPERZENT_BASE_PATH\"/.env .env"),
        )
        .unwrap();

        let worktree_path = outcome
            .workspace
            .local_worktree_path()
            .expect("workspace should be local");
        let copied_env = fs::read_to_string(worktree_path.join(".env")).unwrap();
        assert_eq!(copied_env, "API_KEY=test\n");

        assert_deleted(
            delete_workspace(&outcome.workspace, repo.repo_path.as_path(), true).unwrap(),
            "base-path-copy workspace should be deleted",
        );
    }

    #[test]
    fn delete_workspace_blocks_on_teardown_failure_until_force_delete() {
        let repo = init_repo();
        fs::create_dir_all(repo.repo_path.join(".superzent")).unwrap();
        fs::write(
            repo.repo_path.join(".superzent").join("config.json"),
            r#"{"teardown":["echo teardown > teardown-log.txt","false"]}"#,
        )
        .unwrap();
        commit_all_changes(&repo.repo_path, "add failing teardown config");

        let registration = register_project(&repo.repo_path, "codex").unwrap();
        let outcome = create_workspace(
            &registration.project,
            "codex",
            create_workspace_options("feature/teardown-failure"),
        )
        .unwrap();

        let worktree_path = outcome
            .workspace
            .local_worktree_path()
            .expect("workspace should be local")
            .to_path_buf();

        let delete_outcome =
            delete_workspace(&outcome.workspace, repo.repo_path.as_path(), false).unwrap();
        let teardown_failure = match delete_outcome {
            WorkspaceDeleteOutcome::Deleted => panic!("delete should have been blocked"),
            WorkspaceDeleteOutcome::BlockedByTeardown(failure) => failure,
        };
        assert_eq!(teardown_failure.phase, WorkspaceLifecyclePhase::Teardown);
        assert!(worktree_path.exists());
        assert!(worktree_path.join("teardown-log.txt").exists());

        assert_deleted(
            delete_workspace(&outcome.workspace, repo.repo_path.as_path(), true).unwrap(),
            "force delete should skip teardown and remove the workspace",
        );
        assert!(!worktree_path.exists());
    }

    #[test]
    fn delete_workspace_blocks_on_invalid_lifecycle_config_until_force_delete() {
        let repo = init_repo();
        let registration = register_project(&repo.repo_path, "codex").unwrap();
        let outcome = create_workspace(
            &registration.project,
            "codex",
            create_workspace_options("feature/invalid-config-delete"),
        )
        .unwrap();
        fs::create_dir_all(repo.repo_path.join(".superzent")).unwrap();
        fs::write(
            repo.repo_path.join(".superzent").join("config.json"),
            r#"{"copy":["templates"]}"#,
        )
        .unwrap();

        let delete_outcome =
            delete_workspace(&outcome.workspace, repo.repo_path.as_path(), false).unwrap();
        let failure = match delete_outcome {
            WorkspaceDeleteOutcome::Deleted => panic!("delete should have been blocked"),
            WorkspaceDeleteOutcome::BlockedByTeardown(failure) => failure,
        };
        assert_eq!(failure.phase, WorkspaceLifecyclePhase::Teardown);
        assert_eq!(failure.command, "load .superzent/config.json");
        assert!(failure.stderr.contains("`copy` is no longer supported"));

        assert_deleted(
            delete_workspace(&outcome.workspace, repo.repo_path.as_path(), true).unwrap(),
            "invalid-config-delete workspace should be deleted",
        );
    }

    #[test]
    fn delete_workspace_runs_teardown_before_removing_worktree() {
        let repo = init_repo();
        fs::create_dir_all(repo.repo_path.join(".superzent")).unwrap();
        fs::write(
            repo.repo_path.join(".superzent").join("config.json"),
            r#"{"teardown":["printf teardown > \"$SUPERZENT_ROOT_PATH\"/teardown-log.txt"]}"#,
        )
        .unwrap();
        commit_all_changes(&repo.repo_path, "add teardown config");

        let registration = register_project(&repo.repo_path, "codex").unwrap();
        let outcome = create_workspace(
            &registration.project,
            "codex",
            create_workspace_options("feature/teardown-success"),
        )
        .unwrap();

        let worktree_path = outcome
            .workspace
            .local_worktree_path()
            .expect("workspace should be local")
            .to_path_buf();

        assert_deleted(
            delete_workspace(&outcome.workspace, repo.repo_path.as_path(), false).unwrap(),
            "teardown-success workspace should be deleted",
        );
        assert_eq!(
            fs::read_to_string(repo.repo_path.join("teardown-log.txt")).unwrap(),
            "teardown"
        );
        assert!(!worktree_path.exists());
    }

    #[test]
    fn delete_workspace_removes_stale_path_when_git_is_unavailable() {
        let repo = init_repo();
        let registration = register_project(&repo.repo_path, "codex").unwrap();
        let outcome = create_workspace(
            &registration.project,
            "codex",
            create_workspace_options("feature/stale-delete"),
        )
        .unwrap();

        let worktree_path = outcome
            .workspace
            .local_worktree_path()
            .expect("workspace should be local")
            .to_path_buf();
        let mut unavailable_workspace = outcome.workspace;
        unavailable_workspace.git_status = WorkspaceGitStatus::Unavailable;

        fs::remove_dir_all(repo.repo_path.join(".git")).unwrap();

        assert_deleted(
            delete_workspace(&unavailable_workspace, repo.repo_path.as_path(), false).unwrap(),
            "stale workspace should be deleted when git is unavailable",
        );

        assert!(!worktree_path.exists());
    }

    #[test]
    fn delete_workspace_removes_stale_path_when_git_metadata_is_stale() {
        let repo = init_repo();
        let registration = register_project(&repo.repo_path, "codex").unwrap();
        let outcome = create_workspace(
            &registration.project,
            "codex",
            create_workspace_options("feature/stale-delete-metadata"),
        )
        .unwrap();

        let worktree_path = outcome
            .workspace
            .local_worktree_path()
            .expect("workspace should be local")
            .to_path_buf();

        fs::remove_dir_all(repo.repo_path.join(".git")).unwrap();

        assert_deleted(
            delete_workspace(&outcome.workspace, repo.repo_path.as_path(), false).unwrap(),
            "stale metadata workspace should be deleted",
        );

        assert!(!worktree_path.exists());
    }

    #[test]
    fn discover_worktrees_includes_primary_and_linked_worktrees() {
        let repo = init_repo();
        let registration = register_project(&repo.repo_path, "codex").unwrap();
        let outcome = create_workspace(
            &registration.project,
            "codex",
            create_workspace_options("feature/discover-worktrees"),
        )
        .unwrap();

        let worktrees = discover_worktrees(&repo.repo_path).unwrap();
        let paths = worktrees
            .into_iter()
            .map(|worktree| worktree.path)
            .collect::<Vec<_>>();

        assert!(paths.iter().any(|path| path == &repo.repo_path));
        assert!(paths.iter().any(|path| {
            outcome
                .workspace
                .local_worktree_path()
                .is_some_and(|worktree_path| path == worktree_path)
        }));
    }

    struct RepoFixture {
        _temp_dir: TempDir,
        repo_path: PathBuf,
    }

    fn init_repo() -> RepoFixture {
        let temp_dir = TempDir::new().unwrap();
        let repo_path = temp_dir.path().join("repo");
        fs::create_dir_all(&repo_path).unwrap();

        git(&repo_path, &["init", "-b", "main"]);
        git(&repo_path, &["config", "user.name", "Superzent Tests"]);
        git(&repo_path, &["config", "user.email", "tests@superzent.dev"]);
        fs::write(repo_path.join("README.md"), "hello\n").unwrap();
        git(&repo_path, &["add", "README.md"]);
        git(&repo_path, &["commit", "-m", "init"]);

        RepoFixture {
            _temp_dir: temp_dir,
            repo_path: repo_path.canonicalize().unwrap(),
        }
    }

    fn git(repo_path: &Path, args: &[&str]) {
        let mut command = smol::process::Command::new("git");
        command.arg("-C").arg(repo_path).args(args);
        let output = smol::block_on(command.output()).unwrap();

        if !output.status.success() {
            panic!(
                "git {} failed\nstdout: {}\nstderr: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    fn commit_all_changes(repo_path: &Path, message: &str) {
        git(repo_path, &["add", "."]);
        git(repo_path, &["commit", "-m", message]);
    }
}
