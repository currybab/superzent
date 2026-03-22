use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde::Deserialize;
use std::{
    fs,
    path::{Component, Path, PathBuf},
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
    pub warning: Option<String>,
}

#[derive(Debug)]
pub struct WorkspaceRefresh {
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
}

#[derive(Default, Deserialize)]
struct SuperzentConfig {
    #[serde(default)]
    teardown: Vec<String>,
    #[serde(default)]
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
    let Some(project_repo_root) = project.local_repo_root() else {
        bail!("cannot create a local workspace for a remote project");
    };
    let repo_root = discover_repo_root(project_repo_root)
        .context("initialize Git before creating a managed workspace")?;
    ensure_clean_worktree(project_repo_root)?;
    let repo_name = repo_root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("project");
    let base_ref = current_branch(&repo_root).unwrap_or_else(|| "HEAD".to_string());
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
            &base_ref,
        ],
    )?;

    let warning = match prepare_workspace_contents(&repo_root, &worktree_path) {
        Ok(warnings) => (!warnings.is_empty()).then(|| warnings.join("\n")),
        Err(error) => {
            cleanup_worktree(&repo_root, &worktree_path);
            return Err(error);
        }
    };

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
            last_attention_reason: warning.clone(),
            created_at: Utc::now(),
            last_opened_at: Utc::now(),
        },
        warning,
    })
}

pub fn delete_workspace(workspace: &WorkspaceEntry, repo_root: &Path, force: bool) -> Result<()> {
    if !workspace.managed || workspace.kind == WorkspaceKind::Primary {
        return Ok(());
    }
    let Some(worktree_path) = workspace.local_worktree_path() else {
        bail!("cannot delete a local workspace for a remote project");
    };
    if !worktree_path.exists() {
        return Ok(());
    }

    run_repo_hooks(
        repo_root,
        worktree_path,
        &workspace.name,
        HookPhase::Teardown,
    )?;

    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    let worktree_path = worktree_path.to_string_lossy().to_string();
    args.push(worktree_path.as_str());

    run_git(repo_root, &args)
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

fn ensure_clean_worktree(repo_hint: &Path) -> Result<()> {
    let status = git_output(repo_hint, &["status", "--porcelain"])?;
    if status.is_empty() {
        return Ok(());
    }

    bail!("commit or stash local changes before creating a managed workspace");
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

#[derive(Copy, Clone)]
enum HookPhase {
    Teardown,
}

fn run_repo_hooks(
    repo_root: &Path,
    worktree_path: &Path,
    workspace_name: &str,
    phase: HookPhase,
) -> Result<()> {
    let config = load_superzent_config(repo_root)?;
    let commands = match phase {
        HookPhase::Teardown => config.teardown,
    };

    for command in commands {
        run_shell_command(&command, worktree_path, workspace_name, repo_root)?;
    }

    Ok(())
}

fn prepare_workspace_contents(repo_root: &Path, worktree_path: &Path) -> Result<Vec<String>> {
    let config = load_superzent_config(repo_root)?;
    let mut warnings = Vec::new();

    let superzent_source = repo_root.join(".superzent");
    if superzent_source.exists() {
        copy_repo_path(&superzent_source, &worktree_path.join(".superzent"))
            .with_context(|| format!("failed to copy {}", superzent_source.display()))?;
    }

    for copy_entry in &config.copy {
        let Some(source_path) = resolve_repo_relative_path(repo_root, copy_entry)? else {
            warnings.push(format!("Skipped missing copy source `{copy_entry}`."));
            continue;
        };

        let destination_path = worktree_path.join(copy_entry);
        copy_repo_path(&source_path, &destination_path)
            .with_context(|| format!("failed to copy {}", source_path.display()))?;
    }

    Ok(warnings)
}

fn load_superzent_config(repo_root: &Path) -> Result<SuperzentConfig> {
    let config_path = repo_root.join(".superzent").join("config.json");
    if !config_path.exists() {
        return Ok(SuperzentConfig::default());
    }

    let contents = fs::read_to_string(&config_path)
        .with_context(|| format!("failed to read {}", config_path.display()))?;
    serde_json::from_str(&contents)
        .with_context(|| format!("failed to parse {}", config_path.display()))
}

fn resolve_repo_relative_path(repo_root: &Path, relative_path: &str) -> Result<Option<PathBuf>> {
    let relative_path = Path::new(relative_path);
    if relative_path.is_absolute() {
        bail!(
            "copy path `{}` must be relative to the repository root",
            relative_path.display()
        );
    }

    for component in relative_path.components() {
        match component {
            Component::CurDir | Component::Normal(_) => {}
            Component::ParentDir => {
                bail!(
                    "copy path `{}` cannot leave the repository root",
                    relative_path.display()
                )
            }
            Component::Prefix(_) | Component::RootDir => {
                bail!(
                    "copy path `{}` must be relative to the repository root",
                    relative_path.display()
                )
            }
        }
    }

    let source_path = repo_root.join(relative_path);
    if !source_path.exists() {
        return Ok(None);
    }

    Ok(Some(source_path))
}

fn copy_repo_path(source_path: &Path, destination_path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source_path)
        .with_context(|| format!("failed to read {}", source_path.display()))?;

    if metadata.is_dir() {
        fs::create_dir_all(destination_path)
            .with_context(|| format!("failed to create {}", destination_path.display()))?;

        for entry in fs::read_dir(source_path)
            .with_context(|| format!("failed to read {}", source_path.display()))?
        {
            let entry = entry?;
            copy_repo_path(&entry.path(), &destination_path.join(entry.file_name()))?;
        }

        return Ok(());
    }

    if let Some(parent_directory) = destination_path.parent() {
        fs::create_dir_all(parent_directory)
            .with_context(|| format!("failed to create {}", parent_directory.display()))?;
    }

    fs::copy(source_path, destination_path).with_context(|| {
        format!(
            "failed to copy {} to {}",
            source_path.display(),
            destination_path.display()
        )
    })?;

    Ok(())
}

fn cleanup_worktree(repo_root: &Path, worktree_path: &Path) {
    let worktree_path = worktree_path.to_string_lossy().to_string();
    if let Err(error) = run_git(
        repo_root,
        &["worktree", "remove", "--force", &worktree_path],
    ) {
        log::error!("failed to clean up worktree {}: {error:#}", worktree_path);
    }
}

fn run_shell_command(
    command: &str,
    cwd: &Path,
    workspace_name: &str,
    repo_root: &Path,
) -> Result<()> {
    let mut shell_command = smol::process::Command::new("/bin/zsh");
    shell_command
        .arg("-lc")
        .arg(command)
        .current_dir(cwd)
        .env("SUPERSET_WORKSPACE_NAME", workspace_name)
        .env("SUPERSET_ROOT_PATH", repo_root);
    let output = run_command_output(shell_command, || {
        format!("failed to run hook command: {command}")
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        bail!(
            "hook command `{command}` failed{}\n{}",
            output
                .status
                .code()
                .map(|code| format!(" with exit code {code}"))
                .unwrap_or_default(),
            [stdout.trim(), stderr.trim()]
                .into_iter()
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    Ok(())
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
            CreateWorkspaceOptions {
                branch_name: "feature/dirty-worktree".to_string(),
            },
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("commit or stash local changes before creating a managed workspace")
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
            CreateWorkspaceOptions {
                branch_name: "feature/linked-root".to_string(),
            },
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

        delete_workspace(&outcome.workspace, repo.repo_path.as_path(), true).unwrap();
    }

    #[test]
    fn create_workspace_uses_requested_branch_name_and_sanitized_worktree_path() {
        let repo = init_repo();
        let registration = register_project(&repo.repo_path, "codex").unwrap();
        let outcome = create_workspace(
            &registration.project,
            "codex",
            CreateWorkspaceOptions {
                branch_name: "feature/superzent-test".to_string(),
            },
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

        delete_workspace(&outcome.workspace, repo.repo_path.as_path(), true).unwrap();
    }

    #[test]
    fn create_workspace_copies_superzent_directory_and_extra_paths() {
        let repo = init_repo();
        fs::create_dir_all(repo.repo_path.join(".superzent")).unwrap();
        fs::write(
            repo.repo_path.join(".superzent").join("config.json"),
            r#"{"copy":["templates"]}"#,
        )
        .unwrap();
        fs::write(
            repo.repo_path.join(".superzent").join("setup.sh"),
            "#!/bin/zsh\necho setup\n",
        )
        .unwrap();
        fs::create_dir_all(repo.repo_path.join("templates")).unwrap();
        fs::write(
            repo.repo_path.join("templates").join("agent.txt"),
            "preset\n",
        )
        .unwrap();
        commit_all_changes(&repo.repo_path, "add superzent files");

        let registration = register_project(&repo.repo_path, "codex").unwrap();
        let outcome = create_workspace(
            &registration.project,
            "codex",
            CreateWorkspaceOptions {
                branch_name: "feature/copy-paths".to_string(),
            },
        )
        .unwrap();

        assert!(
            outcome
                .workspace
                .local_worktree_path()
                .expect("workspace should be local")
                .join(".superzent")
                .join("setup.sh")
                .exists()
        );
        assert!(
            outcome
                .workspace
                .local_worktree_path()
                .expect("workspace should be local")
                .join("templates")
                .join("agent.txt")
                .exists()
        );

        delete_workspace(&outcome.workspace, repo.repo_path.as_path(), true).unwrap();
    }

    #[test]
    fn create_workspace_rejects_copy_paths_outside_repo() {
        let repo = init_repo();
        fs::create_dir_all(repo.repo_path.join(".superzent")).unwrap();
        fs::write(
            repo.repo_path.join(".superzent").join("config.json"),
            r#"{"copy":["../outside"]}"#,
        )
        .unwrap();
        commit_all_changes(&repo.repo_path, "add invalid copy config");

        let registration = register_project(&repo.repo_path, "codex").unwrap();
        let error = create_workspace(
            &registration.project,
            "codex",
            CreateWorkspaceOptions {
                branch_name: "feature/reject-copy".to_string(),
            },
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("cannot leave the repository root")
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
    fn create_workspace_reports_missing_copy_sources_as_warning() {
        let repo = init_repo();
        fs::create_dir_all(repo.repo_path.join(".superzent")).unwrap();
        fs::write(
            repo.repo_path.join(".superzent").join("config.json"),
            r#"{"copy":["missing-directory"]}"#,
        )
        .unwrap();
        commit_all_changes(&repo.repo_path, "add missing copy config");

        let registration = register_project(&repo.repo_path, "codex").unwrap();
        let outcome = create_workspace(
            &registration.project,
            "codex",
            CreateWorkspaceOptions {
                branch_name: "feature/missing-copy-warning".to_string(),
            },
        )
        .unwrap();

        assert!(
            outcome
                .warning
                .as_deref()
                .is_some_and(|warning| warning.contains("missing-directory"))
        );

        delete_workspace(&outcome.workspace, repo.repo_path.as_path(), true).unwrap();
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
