use crate::{
    changes_diff_view::{ChangesDiffKind, ChangesDiffView, NotATextFile, OpenChangesDiff},
    file_history_view::FileHistoryView,
    git_panel::{
        CollapseSelectedEntry, ExpandSelectedEntry, FirstEntry, LastEntry, NextEntry, PreviousEntry,
    },
    git_panel_settings::GitPanelSettings,
};
use anyhow::{Context as _, Result};
use collections::HashSet;
use file_icons::FileIcons;
use git::{
    AddToGitignore, FileHistory, RestoreFile, StageFile, StageRange, ToggleStaged, UnstageFile,
    repository::RepoPath,
    status::{DiffStat, FileStatus, StatusCode},
};
use gpui::{
    Action, AsyncApp, ClickEvent, Corner, DismissEvent, Div, Entity, FocusHandle, Focusable,
    FontWeight, KeyContext, MouseButton, MouseDownEvent, Point, PromptLevel, ScrollStrategy,
    Stateful, Subscription, Task, UniformListScrollHandle, WeakEntity, actions, anchored, deferred,
    uniform_list,
};
use language::LineEnding;
use project::{
    Project, ProjectPath,
    git_store::{
        GitStoreEvent, LocalRepositoryState, Repository, RepositoryEvent, RepositoryState,
        StatusEntry,
    },
};
use settings::{GitPanelChangesLayout, Settings as _, SettingsStore};
use std::ops::Range;
use ui::{ContextMenu, Scrollbars, Tooltip, WithScrollbar, prelude::*};
use workspace::{Pane, Workspace, notifications::DetachAndPromptErr as _};

const ROW_GROUP: &str = "changes-view-row";

actions!(
    git_panel,
    [
        /// Switches the changes list between the VS Code and Zed layouts.
        ToggleChangesLayout,
    ]
);

pub(crate) fn register(workspace: &mut Workspace) {
    workspace.register_action(|workspace, _: &ToggleChangesLayout, _, cx| {
        let next_layout = if changes_layout_enabled(cx) {
            GitPanelChangesLayout::Zed
        } else {
            GitPanelChangesLayout::Vscode
        };
        let fs = workspace.app_state().fs.clone();
        cx.update_global::<SettingsStore, _>(|store, _| {
            store.update_settings_file(fs, move |settings, _| {
                settings.git_panel.get_or_insert_default().changes_layout = Some(next_layout);
            });
        });
    });
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum ChangesSection {
    Merge,
    Staged,
    Unstaged,
}

impl ChangesSection {
    const ALL: [ChangesSection; 3] = [
        ChangesSection::Merge,
        ChangesSection::Staged,
        ChangesSection::Unstaged,
    ];

    fn title(self) -> &'static str {
        match self {
            ChangesSection::Merge => "Merge Changes",
            ChangesSection::Staged => "Staged Changes",
            ChangesSection::Unstaged => "Changes",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FileChange {
    Modified,
    TypeChanged,
    Added,
    Deleted,
    Renamed,
    Copied,
    Untracked,
    Conflicted,
}

impl FileChange {
    fn from_status_code(code: StatusCode) -> Option<Self> {
        match code {
            StatusCode::Modified => Some(FileChange::Modified),
            StatusCode::TypeChanged => Some(FileChange::TypeChanged),
            StatusCode::Added => Some(FileChange::Added),
            StatusCode::Deleted => Some(FileChange::Deleted),
            StatusCode::Renamed => Some(FileChange::Renamed),
            StatusCode::Copied => Some(FileChange::Copied),
            StatusCode::Unmodified => None,
        }
    }

    fn letter(self) -> &'static str {
        match self {
            FileChange::Modified => "M",
            FileChange::TypeChanged => "T",
            FileChange::Added => "A",
            FileChange::Deleted => "D",
            FileChange::Renamed => "R",
            FileChange::Copied => "C",
            FileChange::Untracked => "U",
            FileChange::Conflicted => "!",
        }
    }

    fn color(self) -> Color {
        match self {
            FileChange::Modified | FileChange::TypeChanged | FileChange::Renamed => {
                Color::VersionControlModified
            }
            FileChange::Added | FileChange::Copied | FileChange::Untracked => {
                Color::VersionControlAdded
            }
            FileChange::Deleted => Color::VersionControlDeleted,
            FileChange::Conflicted => Color::VersionControlConflict,
        }
    }

    fn description(self) -> &'static str {
        match self {
            FileChange::Modified => "Modified",
            FileChange::TypeChanged => "Type Changed",
            FileChange::Added => "Added",
            FileChange::Deleted => "Deleted",
            FileChange::Renamed => "Renamed",
            FileChange::Copied => "Copied",
            FileChange::Untracked => "Untracked",
            FileChange::Conflicted => "Conflict",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ChangedFile {
    section: ChangesSection,
    repo_path: RepoPath,
    change: FileChange,
    status: FileStatus,
    diff_stat: Option<DiffStat>,
}

impl ChangedFile {
    fn has_staged_changes(&self) -> bool {
        matches!(
            self.status,
            FileStatus::Tracked(tracked) if tracked.index_status != StatusCode::Unmodified
        )
    }

    fn has_unstaged_changes(&self) -> bool {
        match self.status {
            FileStatus::Tracked(tracked) => tracked.worktree_status != StatusCode::Unmodified,
            FileStatus::Untracked | FileStatus::Unmerged(_) => true,
            FileStatus::Ignored => false,
        }
    }

    /// Discarding a file the index doesn't have (untracked, intent-to-add, or a staged
    /// deletion that was recreated) means deleting it from the working tree.
    fn is_missing_from_index(&self) -> bool {
        match self.status {
            FileStatus::Untracked => true,
            FileStatus::Tracked(tracked) => {
                tracked.index_status == StatusCode::Deleted || self.is_intent_to_add()
            }
            FileStatus::Unmerged(_) | FileStatus::Ignored => false,
        }
    }

    /// `git add -N` leaves an empty index entry that HEAD doesn't have.
    fn is_intent_to_add(&self) -> bool {
        matches!(
            self.status,
            FileStatus::Tracked(tracked)
                if tracked.index_status == StatusCode::Unmodified
                    && tracked.worktree_status == StatusCode::Added
        )
    }

    /// Opening a file that is gone from disk would create an empty buffer, which
    /// recreates the file when saved. A staged deletion whose file was recreated
    /// still exists, so the index status alone isn't enough.
    fn is_missing_from_worktree(&self) -> bool {
        match self.status {
            FileStatus::Tracked(tracked) => match tracked.worktree_status {
                StatusCode::Deleted => true,
                StatusCode::Unmodified => tracked.index_status == StatusCode::Deleted,
                _ => false,
            },
            FileStatus::Untracked | FileStatus::Unmerged(_) | FileStatus::Ignored => false,
        }
    }

    /// A row's own change can be `Modified` for a file that was never committed, such
    /// as the unstaged half of a staged addition, so the whole status is checked.
    fn has_history(&self) -> bool {
        !self.status.is_created()
    }

    /// Files that only exist in the working tree or in a merge have no diff to show.
    fn opens_as_file(&self) -> bool {
        matches!(self.change, FileChange::Untracked | FileChange::Conflicted)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ChangesRow {
    Header {
        section: ChangesSection,
        file_count: usize,
    },
    File(ChangedFile),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum RowKey {
    Header(ChangesSection),
    File(ChangesSection, RepoPath),
}

/// Opens can finish after the active repository or pane changed, so they keep what
/// the row was opened against.
struct OpenRequest {
    id: usize,
    repository: Entity<Repository>,
    pane: Option<WeakEntity<Pane>>,
}

impl ChangesRow {
    fn key(&self) -> RowKey {
        match self {
            ChangesRow::Header { section, .. } => RowKey::Header(*section),
            ChangesRow::File(file) => RowKey::File(file.section, file.repo_path.clone()),
        }
    }

    fn section(&self) -> ChangesSection {
        match self {
            ChangesRow::Header { section, .. } => *section,
            ChangesRow::File(file) => file.section,
        }
    }
}

fn changed_files(statuses: impl IntoIterator<Item = StatusEntry>) -> Vec<ChangedFile> {
    let mut merge = Vec::new();
    let mut staged = Vec::new();
    let mut unstaged = Vec::new();
    for entry in statuses {
        // The stat compares HEAD with the working tree, which matches neither row of a
        // partially staged file.
        let is_partially_staged = matches!(
            entry.status,
            FileStatus::Tracked(tracked)
                if tracked.index_status != StatusCode::Unmodified
                    && tracked.worktree_status != StatusCode::Unmodified
        );
        let file = |section, change| ChangedFile {
            section,
            repo_path: entry.repo_path.clone(),
            change,
            status: entry.status,
            diff_stat: entry.diff_stat.filter(|_| !is_partially_staged),
        };
        match entry.status {
            FileStatus::Ignored => {}
            FileStatus::Unmerged(_) => {
                merge.push(file(ChangesSection::Merge, FileChange::Conflicted));
            }
            FileStatus::Untracked => {
                unstaged.push(file(ChangesSection::Unstaged, FileChange::Untracked));
            }
            FileStatus::Tracked(tracked) => {
                if let Some(change) = FileChange::from_status_code(tracked.index_status) {
                    staged.push(file(ChangesSection::Staged, change));
                }
                if let Some(change) = FileChange::from_status_code(tracked.worktree_status) {
                    unstaged.push(file(ChangesSection::Unstaged, change));
                }
            }
        }
    }
    merge.into_iter().chain(staged).chain(unstaged).collect()
}

fn build_rows(files: &[ChangedFile], collapsed: &HashSet<ChangesSection>) -> Vec<ChangesRow> {
    ChangesSection::ALL
        .into_iter()
        .flat_map(|section| {
            let section_files = files
                .iter()
                .filter(|file| file.section == section)
                .cloned()
                .collect::<Vec<_>>();
            if section_files.is_empty() {
                return Vec::new();
            }
            let header = ChangesRow::Header {
                section,
                file_count: section_files.len(),
            };
            let file_rows = if collapsed.contains(&section) {
                Vec::new()
            } else {
                section_files.into_iter().map(ChangesRow::File).collect()
            };
            std::iter::once(header).chain(file_rows).collect()
        })
        .collect()
}

/// Keeps the selection on the same row when it still exists. When a file left its
/// section (e.g. it was just staged), the next file of that section is selected, so
/// staging files one after another walks down the list instead of landing on the
/// file that was just moved.
fn reselect(
    rows: &[ChangesRow],
    previous_key: Option<&RowKey>,
    previous_index: Option<usize>,
) -> Option<usize> {
    if rows.is_empty() {
        return None;
    }
    if let Some(key) = previous_key {
        if let Some(index) = rows.iter().position(|row| &row.key() == key) {
            return Some(index);
        }
        if let RowKey::File(section, repo_path) = key {
            let mut section_files = rows
                .iter()
                .enumerate()
                .filter_map(|(index, row)| match row {
                    ChangesRow::File(file) if file.section == *section => Some((index, file)),
                    _ => None,
                });
            let next_in_section = section_files
                .clone()
                .find(|(_, file)| &file.repo_path > repo_path)
                .or_else(move || section_files.next_back());
            if let Some((index, _)) = next_in_section {
                return Some(index);
            }
        }
    }
    previous_index.map(|index| index.min(rows.len() - 1))
}

pub(crate) fn changes_layout_enabled(cx: &App) -> bool {
    GitPanelSettings::get_global(cx).changes_layout == GitPanelChangesLayout::Vscode
}

/// Returns whether the view was created or dropped, in which case the panel has to
/// re-render: the layout setting may be the only setting that changed.
pub(crate) fn sync_changes_view(
    changes_view: &mut Option<Entity<ChangesView>>,
    project: &Entity<Project>,
    workspace: &WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    match (changes_layout_enabled(cx), changes_view.is_some()) {
        (true, false) => {
            *changes_view =
                Some(cx.new(|cx| ChangesView::new(project.clone(), workspace.clone(), window, cx)));
            true
        }
        (false, true) => {
            *changes_view = None;
            true
        }
        (true, true) | (false, false) => false,
    }
}

pub(crate) struct ChangesView {
    project: Entity<Project>,
    workspace: WeakEntity<Workspace>,
    active_repository: Option<Entity<Repository>>,
    focus_handle: FocusHandle,
    scroll_handle: UniformListScrollHandle,
    files: Vec<ChangedFile>,
    rows: Vec<ChangesRow>,
    collapsed_sections: HashSet<ChangesSection>,
    selected_row: Option<usize>,
    stage_anchor: Option<RepoPath>,
    preview_open: Task<Option<()>>,
    latest_open: usize,
    context_menu: Option<(Entity<ContextMenu>, Point<Pixels>, Subscription)>,
    _subscriptions: Vec<Subscription>,
}

impl ChangesView {
    fn new(
        project: Entity<Project>,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let git_store = project.read(cx).git_store().clone();
        let subscriptions = vec![
            cx.subscribe(
                &git_store,
                |this, _, event: &GitStoreEvent, cx| match event {
                    GitStoreEvent::RepositoryUpdated(_, RepositoryEvent::StatusesChanged, true)
                    | GitStoreEvent::RepositoryAdded
                    | GitStoreEvent::RepositoryRemoved(_)
                    | GitStoreEvent::ActiveRepositoryChanged(_) => this.refresh(cx),
                    _ => {}
                },
            ),
            cx.on_focus(&focus_handle, window, |this, _, cx| {
                if this.selected_row.is_none() && !this.rows.is_empty() {
                    this.select_row(0, cx);
                }
            }),
        ];

        let mut this = Self {
            project,
            workspace,
            active_repository: None,
            focus_handle,
            scroll_handle: UniformListScrollHandle::new(),
            files: Vec::new(),
            rows: Vec::new(),
            collapsed_sections: HashSet::default(),
            selected_row: None,
            stage_anchor: None,
            preview_open: Task::ready(None),
            latest_open: 0,
            context_menu: None,
            _subscriptions: subscriptions,
        };
        this.refresh(cx);
        this
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let active_repository = self.project.read(cx).active_repository(cx);
        if active_repository != self.active_repository {
            self.stage_anchor = None;
        }
        self.active_repository = active_repository;
        self.files = match &self.active_repository {
            Some(repository) => changed_files(repository.read(cx).cached_status()),
            None => Vec::new(),
        };
        self.rebuild_rows(cx);
    }

    fn rebuild_rows(&mut self, cx: &mut Context<Self>) {
        let previous_key = self.selected_changes_row().map(ChangesRow::key);
        self.rows = build_rows(&self.files, &self.collapsed_sections);
        self.selected_row = reselect(&self.rows, previous_key.as_ref(), self.selected_row);
        cx.notify();
    }

    fn selected_changes_row(&self) -> Option<&ChangesRow> {
        self.selected_row.and_then(|index| self.rows.get(index))
    }

    fn select_row(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.rows.len() {
            return;
        }
        self.selected_row = Some(index);
        self.scroll_handle
            .scroll_to_item(index, ScrollStrategy::Center);
        cx.notify();
    }

    fn select_next(&mut self, _: &NextEntry, _: &mut Window, cx: &mut Context<Self>) {
        let next = self.selected_row.map_or(0, |index| index + 1);
        self.select_row(next, cx);
    }

    fn select_previous(&mut self, _: &PreviousEntry, _: &mut Window, cx: &mut Context<Self>) {
        let previous = self.selected_row.map_or(0, |index| index.saturating_sub(1));
        self.select_row(previous, cx);
    }

    fn select_first(&mut self, _: &FirstEntry, _: &mut Window, cx: &mut Context<Self>) {
        self.select_row(0, cx);
    }

    fn select_last(&mut self, _: &LastEntry, _: &mut Window, cx: &mut Context<Self>) {
        self.select_row(self.rows.len().saturating_sub(1), cx);
    }

    fn menu_select_next(
        &mut self,
        _: &menu::SelectNext,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_next(&NextEntry, window, cx);
    }

    fn menu_select_previous(
        &mut self,
        _: &menu::SelectPrevious,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_previous(&PreviousEntry, window, cx);
    }

    fn menu_select_first(
        &mut self,
        _: &menu::SelectFirst,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_first(&FirstEntry, window, cx);
    }

    fn menu_select_last(
        &mut self,
        _: &menu::SelectLast,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_last(&LastEntry, window, cx);
    }

    fn set_section_collapsed(
        &mut self,
        section: ChangesSection,
        collapsed: bool,
        cx: &mut Context<Self>,
    ) {
        let changed = if collapsed {
            self.collapsed_sections.insert(section)
        } else {
            self.collapsed_sections.remove(&section)
        };
        if changed {
            self.rebuild_rows(cx);
        }
    }

    fn toggle_section(&mut self, section: ChangesSection, cx: &mut Context<Self>) {
        let collapsed = !self.collapsed_sections.contains(&section);
        self.set_section_collapsed(section, collapsed, cx);
    }

    fn collapse_selected(
        &mut self,
        _: &CollapseSelectedEntry,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.selected_changes_row() {
            Some(ChangesRow::Header { section, .. }) => {
                let section = *section;
                self.set_section_collapsed(section, true, cx);
            }
            Some(ChangesRow::File(file)) => {
                let header_key = RowKey::Header(file.section);
                if let Some(index) = self.rows.iter().position(|row| row.key() == header_key) {
                    self.select_row(index, cx);
                }
            }
            None => {}
        }
    }

    fn expand_selected(&mut self, _: &ExpandSelectedEntry, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(ChangesRow::Header { section, .. }) = self.selected_changes_row() {
            let section = *section;
            self.set_section_collapsed(section, false, cx);
        }
    }

    fn section_files(&self, section: ChangesSection) -> Vec<ChangedFile> {
        self.files
            .iter()
            .filter(|file| file.section == section)
            .cloned()
            .collect()
    }

    fn row_files(&self, row: &ChangesRow) -> Vec<ChangedFile> {
        match row {
            ChangesRow::Header { section, .. } => self.section_files(*section),
            ChangesRow::File(file) => vec![file.clone()],
        }
    }

    fn set_staged(
        &mut self,
        stage: bool,
        files: Vec<ChangedFile>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(repository) = self.active_repository.clone() else {
            return;
        };
        let repo_paths = files
            .into_iter()
            .map(|file| file.repo_path)
            .collect::<Vec<_>>();
        if repo_paths.is_empty() {
            return;
        }
        let task = repository.update(cx, |repository, cx| {
            if stage {
                repository.stage_entries(repo_paths, cx)
            } else {
                repository.unstage_entries(repo_paths, cx)
            }
        });
        let message = if stage {
            "Failed to stage changes"
        } else {
            "Failed to unstage changes"
        };
        task.detach_and_prompt_err(message, window, cx, |error, _, _| Some(error.to_string()));
    }

    fn stage_row(&mut self, row: &ChangesRow, window: &mut Window, cx: &mut Context<Self>) {
        if row.section() != ChangesSection::Staged {
            if let ChangesRow::File(file) = row {
                self.stage_anchor = Some(file.repo_path.clone());
            }
            let files = self.row_files(row);
            self.set_staged(true, files, window, cx);
        }
    }

    fn unstage_row(&mut self, row: &ChangesRow, window: &mut Window, cx: &mut Context<Self>) {
        if row.section() == ChangesSection::Staged {
            if let ChangesRow::File(file) = row
                && self.stage_anchor.as_ref() == Some(&file.repo_path)
            {
                self.stage_anchor = None;
            }
            let files = self.row_files(row);
            self.set_staged(false, files, window, cx);
        }
    }

    fn toggle_staged(&mut self, _: &ToggleStaged, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.selected_changes_row().cloned() else {
            return;
        };
        if row.section() == ChangesSection::Staged {
            self.unstage_row(&row, window, cx);
        } else {
            self.stage_row(&row, window, cx);
        }
    }

    /// Stages every file between the last staged file and the selected one. The
    /// anchor has usually moved to "Staged Changes" by then, so the range is taken by
    /// path, which is the order a section lists its files in.
    fn stage_range(&mut self, _: &StageRange, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ChangesRow::File(file)) = self.selected_changes_row().cloned() else {
            return;
        };
        if file.section == ChangesSection::Staged {
            return;
        }
        let Some(anchor) = self.stage_anchor.replace(file.repo_path.clone()) else {
            self.set_staged(true, vec![file], window, cx);
            return;
        };
        let range = if anchor <= file.repo_path {
            anchor..=file.repo_path.clone()
        } else {
            file.repo_path.clone()..=anchor
        };
        let files = self
            .section_files(file.section)
            .into_iter()
            .filter(|section_file| range.contains(&section_file.repo_path))
            .collect();
        self.set_staged(true, files, window, cx);
    }

    fn stage_selected(&mut self, _: &StageFile, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(row) = self.selected_changes_row().cloned() {
            self.stage_row(&row, window, cx);
        }
    }

    fn unstage_selected(&mut self, _: &UnstageFile, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(row) = self.selected_changes_row().cloned() {
            self.unstage_row(&row, window, cx);
        }
    }

    fn discard_selected(
        &mut self,
        action: &RestoreFile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(row) = self.selected_changes_row().cloned() {
            self.discard_row(&row, action.skip_prompt, window, cx);
        }
    }

    fn discard_row(
        &mut self,
        row: &ChangesRow,
        skip_prompt: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if row.section() != ChangesSection::Unstaged {
            return;
        }
        let files = self.row_files(row);
        let message = match files.as_slice() {
            [] => return,
            [file] if file.is_missing_from_index() => format!(
                "Are you sure you want to delete {}?",
                display_file_name(&file.repo_path)
            ),
            [file] => format!(
                "Are you sure you want to discard changes in {}?",
                display_file_name(&file.repo_path)
            ),
            files => format!(
                "Are you sure you want to discard all {} changes?",
                files.len()
            ),
        };
        if skip_prompt {
            self.discard_files(files, window, cx);
            return;
        }
        let prompt = window.prompt(
            PromptLevel::Warning,
            &message,
            Some("This action cannot be undone."),
            &["Discard", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if prompt.await? != 0 {
                return anyhow::Ok(());
            }
            this.update_in(cx, |this, window, cx| this.discard_files(files, window, cx))
        })
        .detach_and_log_err(cx);
    }

    fn discard_files(
        &mut self,
        files: Vec<ChangedFile>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(repository) = self.active_repository.clone() else {
            return;
        };
        let project = self.project.clone();
        let (missing_from_index, in_index): (Vec<_>, Vec<_>) = files
            .into_iter()
            .partition(ChangedFile::is_missing_from_index);
        // `git checkout HEAD` would also throw away the staged half of a partially
        // staged file, so those are reverted to their index contents instead.
        let (partially_staged, worktree_only): (Vec<_>, Vec<_>) = in_index
            .into_iter()
            .partition(ChangedFile::has_staged_changes);

        // An intent-to-add entry outlives its file as a deletion that `git checkout HEAD`
        // can't restore, so it is dropped from the index first.
        let intent_to_add = missing_from_index
            .iter()
            .filter(|file| file.is_intent_to_add())
            .map(|file| file.repo_path.clone())
            .collect::<Vec<_>>();

        let task = cx.spawn(async move |_, cx| {
            repository
                .update(cx, |repository, cx| {
                    repository.unstage_entries(intent_to_add, cx)
                })
                .await?;
            for file in missing_from_index {
                let Some(project_path) = project_path_for(&repository, &file.repo_path, cx) else {
                    continue;
                };
                let delete = project.update(cx, |project, cx| {
                    project.delete_file(project_path, true, cx)
                });
                if let Some(delete) = delete {
                    delete.await?;
                }
            }

            if !worktree_only.is_empty() {
                let repo_paths = worktree_only
                    .iter()
                    .map(|file| file.repo_path.clone())
                    .collect::<Vec<_>>();
                repository
                    .update(cx, |repository, cx| {
                        repository.checkout_files("HEAD", repo_paths, cx)
                    })
                    .await?;
                reload_dirty_buffers(&project, &repository, &worktree_only, cx).await?;
            }

            for file in partially_staged {
                restore_from_index(&project, &repository, &file.repo_path, cx).await?;
            }
            anyhow::Ok(())
        });
        task.detach_and_prompt_err("Failed to discard changes", window, cx, |error, _, _| {
            Some(error.to_string())
        });
    }

    fn open_selected(&mut self, _: &menu::Confirm, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.selected_changes_row().cloned() else {
            return;
        };
        self.open_row(
            &row,
            OpenChangesDiff {
                preview: true,
                focus: true,
            },
            window,
            cx,
        );
    }

    fn open_selected_file(
        &mut self,
        _: &menu::SecondaryConfirm,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(ChangesRow::File(file)) = self.selected_changes_row().cloned() {
            self.open_file(
                &file,
                OpenChangesDiff {
                    preview: false,
                    focus: true,
                },
                window,
                cx,
            );
        }
    }

    fn open_row(
        &mut self,
        row: &ChangesRow,
        options: OpenChangesDiff,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let file = match row {
            ChangesRow::Header { section, .. } => {
                self.toggle_section(*section, cx);
                return;
            }
            ChangesRow::File(file) => file,
        };
        if file.opens_as_file() {
            self.open_file(file, options, window, cx);
            return;
        }
        self.latest_open += 1;
        let Some(repository) = self.active_repository.clone() else {
            return;
        };
        let kind = match file.section {
            ChangesSection::Staged => ChangesDiffKind::Index,
            ChangesSection::Merge | ChangesSection::Unstaged => ChangesDiffKind::WorkingTree,
        };
        // Pinned opens all finish, but one that loads after a newer open adds its tab
        // without activating it.
        let is_superseded = {
            let this = cx.weak_entity();
            let open_id = self.latest_open;
            move |cx: &App| {
                this.read_with(cx, |this, _| this.latest_open != open_id)
                    .unwrap_or(true)
            }
        };
        let open_diff = ChangesDiffView::open(
            self.workspace.clone(),
            repository.clone(),
            file.repo_path.clone(),
            kind,
            options,
            is_superseded,
            window,
            cx,
        );
        let file = file.clone();
        let request = OpenRequest {
            id: self.latest_open,
            repository,
            pane: self
                .workspace
                .read_with(cx, |workspace, _| workspace.active_pane().downgrade())
                .ok(),
        };
        let open = cx
            .spawn_in(window, async move |this, cx| match open_diff.await {
                Err(error) if error.downcast_ref::<NotATextFile>().is_some() => {
                    this.update_in(cx, |this, window, cx| {
                        this.open_without_diff(&file, kind, options, &request, window, cx)
                    })?
                }
                result => result,
            })
            .prompt_err("Failed to open diff", window, cx, |error, _, _| {
                Some(error.to_string())
            });
        // Replacing the task cancels the previous preview, so a slow file can't finish
        // loading after a newer selection and take the active tab.
        if options.preview {
            self.preview_open = open;
        } else {
            self.preview_open = Task::ready(None);
            open.detach();
        }
    }

    /// Binary files and symlinks have no text to diff. Opening a binary file itself
    /// shows images in the image viewer, and other files with the workspace's own
    /// message, but that only shows the staged version while the file on disk still
    /// is it.
    fn open_without_diff(
        &mut self,
        file: &ChangedFile,
        kind: ChangesDiffKind,
        options: OpenChangesDiff,
        request: &OpenRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let path = file.repo_path.as_unix_str();
        // The file may have changed while its diff was loading, and opening it is only
        // right if it still matches the row.
        let file = request
            .repository
            .read(cx)
            .status_for_path(&file.repo_path)
            .and_then(|entry| {
                changed_files([entry])
                    .into_iter()
                    .find(|current| current.section == file.section)
            })
            .with_context(|| format!("{path} changed while it was being opened."))?;
        let file = &file;
        // Opening a symlink shows its target, not the link that git tracks.
        if self.is_symlink(file, &request.repository, cx) {
            anyhow::bail!("{path} is a symlink, so its changes can't be shown here.");
        }
        if file.change == FileChange::Deleted {
            anyhow::bail!("{path} isn't a text file, so its deletion can't be shown here.");
        }
        if kind == ChangesDiffKind::Index && file.has_unstaged_changes() {
            anyhow::bail!(
                "{path} isn't a text file and has changed since it was staged, so its \
                 staged version can't be shown here."
            );
        }
        self.open_worktree_file(file, options, request, window, cx);
        Ok(())
    }

    fn is_symlink(&self, file: &ChangedFile, repository: &Entity<Repository>, cx: &App) -> bool {
        repository
            .read(cx)
            .repo_path_to_project_path(&file.repo_path, cx)
            .and_then(|project_path| self.project.read(cx).entry_for_path(&project_path, cx))
            .is_some_and(|entry| entry.canonical_path.is_some())
    }

    fn open_file(
        &mut self,
        file: &ChangedFile,
        options: OpenChangesDiff,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.latest_open += 1;
        self.preview_open = Task::ready(None);
        let Some(repository) = self.active_repository.clone() else {
            return;
        };
        let request = OpenRequest {
            id: self.latest_open,
            repository,
            pane: None,
        };
        self.open_worktree_file(file, options, &request, window, cx);
    }

    fn open_worktree_file(
        &mut self,
        file: &ChangedFile,
        options: OpenChangesDiff,
        request: &OpenRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if file.is_missing_from_worktree() {
            return;
        }
        let Some(project_path) = request
            .repository
            .read(cx)
            .repo_path_to_project_path(&file.repo_path, cx)
        else {
            return;
        };
        // The file is only activated once it loads, so a slow one can't take over from
        // a newer open.
        let Ok(open) = self.workspace.update(cx, |workspace, cx| {
            workspace.open_path_preview(
                project_path,
                request.pane.clone(),
                false,
                options.preview,
                false,
                window,
                cx,
            )
        }) else {
            return;
        };
        let open_id = request.id;
        let open = cx
            .spawn_in(window, async move |this, cx| {
                let item = open.await?;
                this.update_in(cx, |this, window, cx| {
                    if this.latest_open != open_id {
                        return Ok(());
                    }
                    this.workspace.update(cx, |workspace, cx| {
                        workspace.activate_item(item.as_ref(), true, options.focus, window, cx);
                    })
                })?
            })
            .prompt_err("Failed to open file", window, cx, |error, _, _| {
                Some(error.to_string())
            });
        // Opening a preview replaces the pane's current one as soon as it loads, even
        // without activating it, so a stale one is cancelled instead.
        if options.preview {
            self.preview_open = open;
        } else {
            open.detach();
        }
    }

    fn open_file_history(&mut self, _: &FileHistory, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ChangesRow::File(file)) = self.selected_changes_row() else {
            return;
        };
        let Some(repository) = self.active_repository.as_ref() else {
            return;
        };
        let git_store = self.project.read(cx).git_store().downgrade();
        FileHistoryView::open(
            file.repo_path.clone(),
            git_store,
            repository.downgrade(),
            self.workspace.clone(),
            window,
            cx,
        );
    }

    fn add_to_gitignore(
        &mut self,
        _: &AddToGitignore,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ChangesRow::File(file)) = self.selected_changes_row() else {
            return;
        };
        if file.change != FileChange::Untracked {
            return;
        }
        let Some(repository) = self.active_repository.clone() else {
            return;
        };
        let project = self.project.clone();
        let ignored_path = file.repo_path.as_unix_str().to_string();
        let task = cx.spawn(async move |_, cx| {
            let gitignore_path = repository.read_with(cx, |repository, _| {
                repository
                    .snapshot()
                    .work_directory_abs_path
                    .join(".gitignore")
            });
            let buffer = project
                .update(cx, |project, cx| {
                    project.open_local_buffer(gitignore_path, cx)
                })
                .await?;
            let edited = buffer.update(cx, |buffer, cx| {
                let text = buffer.text();
                if text.lines().any(|line| line.trim() == ignored_path) {
                    return false;
                }
                let separator = if text.is_empty() || text.ends_with('\n') {
                    ""
                } else {
                    "\n"
                };
                let end = text.len();
                buffer.edit(
                    [(end..end, format!("{separator}{ignored_path}\n"))],
                    None,
                    cx,
                );
                true
            });
            if edited {
                project
                    .update(cx, |project, cx| project.save_buffer(buffer, cx))
                    .await?;
            }
            anyhow::Ok(())
        });
        task.detach_and_prompt_err("Failed to add to .gitignore", window, cx, |error, _, _| {
            Some(error.to_string())
        });
    }

    /// Mirrors `GitPanel`, which drops its mutating handlers for read-only projects.
    fn has_write_access(&self, cx: &App) -> bool {
        !self.project.read(cx).is_read_only(cx)
    }

    fn handle_row_click(
        &mut self,
        index: usize,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self.rows.get(index).cloned() else {
            return;
        };
        self.select_row(index, cx);
        window.focus(&self.focus_handle, cx);

        if event.modifiers().secondary() {
            if let ChangesRow::File(file) = &row {
                self.open_file(
                    file,
                    OpenChangesDiff {
                        preview: false,
                        focus: true,
                    },
                    window,
                    cx,
                );
            }
            return;
        }

        let options = if event.click_count() > 1 {
            OpenChangesDiff {
                preview: false,
                focus: true,
            }
        } else {
            OpenChangesDiff {
                preview: true,
                focus: false,
            }
        };
        if event.click_count() > 1 && matches!(row, ChangesRow::Header { .. }) {
            // The first click already toggled the section.
            return;
        }
        self.open_row(&row, options, window, cx);
    }

    fn deploy_context_menu(
        &mut self,
        position: Point<Pixels>,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self.rows.get(index).cloned() else {
            return;
        };
        self.select_row(index, cx);
        let focus_handle = self.focus_handle.clone();
        let read_only = !self.has_write_access(cx);
        let context_menu = ContextMenu::build(window, cx, move |menu, _, _| {
            let menu = menu.context(focus_handle);
            match &row {
                ChangesRow::Header { section, .. } => match section {
                    ChangesSection::Staged => menu.action_disabled_when(
                        read_only,
                        "Unstage All Changes",
                        UnstageFile.boxed_clone(),
                    ),
                    ChangesSection::Merge => menu.action_disabled_when(
                        read_only,
                        "Stage All Merge Changes",
                        StageFile.boxed_clone(),
                    ),
                    ChangesSection::Unstaged => menu
                        .action_disabled_when(
                            read_only,
                            "Stage All Changes",
                            StageFile.boxed_clone(),
                        )
                        .action_disabled_when(
                            read_only,
                            "Discard All Changes",
                            RestoreFile::default().boxed_clone(),
                        ),
                },
                ChangesRow::File(file) => {
                    let menu = if file.opens_as_file() {
                        menu
                    } else {
                        menu.action("Open Changes", menu::Confirm.boxed_clone())
                    };
                    let menu = menu
                        .action_disabled_when(
                            file.is_missing_from_worktree(),
                            "Open File",
                            menu::SecondaryConfirm.boxed_clone(),
                        )
                        .separator();
                    let menu = match file.section {
                        ChangesSection::Staged => menu.action_disabled_when(
                            read_only,
                            "Unstage Changes",
                            UnstageFile.boxed_clone(),
                        ),
                        ChangesSection::Merge => menu.action_disabled_when(
                            read_only,
                            "Stage Changes",
                            StageFile.boxed_clone(),
                        ),
                        ChangesSection::Unstaged => menu
                            .action_disabled_when(
                                read_only,
                                "Stage Changes",
                                StageFile.boxed_clone(),
                            )
                            .action_disabled_when(
                                read_only,
                                "Discard Changes",
                                RestoreFile::default().boxed_clone(),
                            )
                            .when(file.change == FileChange::Untracked, |menu| {
                                menu.action_disabled_when(
                                    read_only,
                                    "Add to .gitignore",
                                    AddToGitignore.boxed_clone(),
                                )
                            }),
                    };
                    menu.separator().action_disabled_when(
                        !file.has_history(),
                        "View File History",
                        FileHistory.boxed_clone(),
                    )
                }
            }
        });

        let subscription =
            cx.subscribe_in(
                &context_menu,
                window,
                |this, _, _: &DismissEvent, window, cx| {
                    if this.context_menu.as_ref().is_some_and(|(menu, _, _)| {
                        menu.focus_handle(cx).contains_focused(window, cx)
                    }) {
                        window.focus(&this.focus_handle, cx);
                    }
                    this.context_menu.take();
                    cx.notify();
                },
            );
        self.context_menu = Some((context_menu, position, subscription));
        cx.notify();
    }

    fn dispatch_context(&self) -> KeyContext {
        // Reuses the git panel's list bindings, which are scoped to `GitPanel && ChangesList`.
        let mut context = KeyContext::new_with_defaults();
        context.add("GitPanel");
        context.add("ChangesList");
        context.add("menu");
        context
    }

    fn render_header(
        &self,
        index: usize,
        section: ChangesSection,
        file_count: usize,
        selected: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let collapsed = self.collapsed_sections.contains(&section);
        let header_row = ChangesRow::Header {
            section,
            file_count,
        };
        let actions = match section {
            ChangesSection::Staged => vec![self.row_action_button(
                ("unstage-all", index),
                IconName::Dash,
                "Unstage All Changes",
                header_row,
                RowAction::Unstage,
                cx,
            )],
            ChangesSection::Merge => vec![self.row_action_button(
                ("stage-all-merge", index),
                IconName::Plus,
                "Stage All Merge Changes",
                header_row,
                RowAction::Stage,
                cx,
            )],
            ChangesSection::Unstaged => vec![
                self.row_action_button(
                    ("discard-all", index),
                    IconName::Undo,
                    "Discard All Changes",
                    header_row.clone(),
                    RowAction::Discard,
                    cx,
                ),
                self.row_action_button(
                    ("stage-all", index),
                    IconName::Plus,
                    "Stage All Changes",
                    header_row,
                    RowAction::Stage,
                    cx,
                ),
            ],
        };

        self.row_container(("changes-header", index), index, selected, window, cx)
            .child(
                Icon::new(if collapsed {
                    IconName::ChevronRight
                } else {
                    IconName::ChevronDown
                })
                .size(IconSize::Small)
                .color(Color::Muted),
            )
            .child(
                Label::new(section.title())
                    .size(LabelSize::Small)
                    .weight(FontWeight::SEMIBOLD)
                    .single_line(),
            )
            .child(div().flex_1())
            .child(self.row_actions(actions, selected))
            .child(
                Label::new(file_count.to_string())
                    .size(LabelSize::XSmall)
                    .color(Color::Muted),
            )
            .into_any_element()
    }

    fn render_file(
        &self,
        index: usize,
        file: &ChangedFile,
        selected: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let row = ChangesRow::File(file.clone());
        let file_name = display_file_name(&file.repo_path);
        let parent = file
            .repo_path
            .parent()
            .filter(|parent| !parent.is_empty())
            .map(|parent| {
                parent
                    .display(self.project.read(cx).path_style(cx))
                    .to_string()
            });
        let icon = GitPanelSettings::get_global(cx).file_icons.then(|| {
            FileIcons::get_icon(file.repo_path.as_std_path(), cx)
                .map(Icon::from_path)
                .unwrap_or_else(|| Icon::new(IconName::File))
        });
        let is_deleted = file.change == FileChange::Deleted;

        let open_file = (!file.is_missing_from_worktree()).then(|| {
            self.row_action_button(
                ("open-file", index),
                IconName::FileTextOutlined,
                "Open File",
                row.clone(),
                RowAction::OpenFile,
                cx,
            )
        });
        let actions = open_file
            .into_iter()
            .chain(match file.section {
                ChangesSection::Staged => vec![self.row_action_button(
                    ("unstage", index),
                    IconName::Dash,
                    "Unstage Changes",
                    row,
                    RowAction::Unstage,
                    cx,
                )],
                ChangesSection::Merge => vec![self.row_action_button(
                    ("stage-merge", index),
                    IconName::Plus,
                    "Stage Changes",
                    row,
                    RowAction::Stage,
                    cx,
                )],
                ChangesSection::Unstaged => vec![
                    self.row_action_button(
                        ("discard", index),
                        IconName::Undo,
                        "Discard Changes",
                        row.clone(),
                        RowAction::Discard,
                        cx,
                    ),
                    self.row_action_button(
                        ("stage", index),
                        IconName::Plus,
                        "Stage Changes",
                        row,
                        RowAction::Stage,
                        cx,
                    ),
                ],
            })
            .collect::<Vec<_>>();

        let tooltip_text: SharedString = format!(
            "{} • {}",
            file.repo_path.display(self.project.read(cx).path_style(cx)),
            file.change.description()
        )
        .into();

        self.row_container(("changes-file", index), index, selected, window, cx)
            .pl_5()
            .tooltip(Tooltip::text(tooltip_text))
            .when_some(icon, |this, icon| {
                this.child(icon.size(IconSize::Small).color(Color::Muted))
            })
            .child(
                h_flex()
                    .min_w_0()
                    .flex_1()
                    .gap_1p5()
                    .child(
                        Label::new(file_name)
                            .single_line()
                            .when(is_deleted, Label::strikethrough)
                            .flex_none(),
                    )
                    .when_some(parent, |this, parent| {
                        this.child(
                            Label::new(parent)
                                .size(LabelSize::Small)
                                .color(Color::Muted)
                                .single_line()
                                .truncate(),
                        )
                    }),
            )
            .when(GitPanelSettings::get_global(cx).diff_stats, |this| {
                this.when_some(file.diff_stat, |this, stat| {
                    this.child(ui::DiffStat::new(
                        ("diff-stat", index),
                        stat.added as usize,
                        stat.deleted as usize,
                    ))
                })
            })
            .child(self.row_actions(actions, selected))
            .child(
                div().w_3().flex_none().child(
                    Label::new(file.change.letter())
                        .size(LabelSize::Small)
                        .color(file.change.color()),
                ),
            )
            .into_any_element()
    }

    fn row_container(
        &self,
        id: impl Into<ElementId>,
        index: usize,
        selected: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let colors = cx.theme().colors();
        let focused = self.focus_handle.is_focused(window);
        h_flex()
            .id(id)
            .group(ROW_GROUP)
            .h(rems(1.75))
            .w_full()
            .pl_2()
            .pr_1()
            .gap_1p5()
            .border_1()
            .border_color(if selected && focused {
                colors.panel_focused_border
            } else {
                gpui::transparent_black()
            })
            .when(selected, |this| this.bg(colors.ghost_element_selected))
            .hover(|style| style.bg(colors.ghost_element_hover))
            .cursor_pointer()
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.handle_row_click(index, event, window, cx);
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.deploy_context_menu(event.position, index, window, cx);
                    cx.stop_propagation();
                }),
            )
    }

    fn row_actions(&self, actions: Vec<AnyElement>, selected: bool) -> impl IntoElement {
        h_flex()
            .flex_none()
            .gap_0p5()
            .when(!selected, |this| this.visible_on_hover(ROW_GROUP))
            .children(actions)
    }

    fn row_action_button(
        &self,
        id: impl Into<ElementId>,
        icon: IconName,
        tooltip: &'static str,
        row: ChangesRow,
        action: RowAction,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        IconButton::new(id, icon)
            .icon_size(IconSize::Small)
            .icon_color(Color::Muted)
            .disabled(action.mutates_repository() && !self.has_write_access(cx))
            .tooltip(Tooltip::text(tooltip))
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                cx.stop_propagation();
                this.run_row_action(&row, action, window, cx);
            }))
            .into_any_element()
    }

    fn run_row_action(
        &mut self,
        row: &ChangesRow,
        action: RowAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            RowAction::Stage => self.stage_row(row, window, cx),
            RowAction::Unstage => self.unstage_row(row, window, cx),
            RowAction::Discard => self.discard_row(row, false, window, cx),
            RowAction::OpenFile => {
                if let ChangesRow::File(file) = row {
                    self.open_file(
                        file,
                        OpenChangesDiff {
                            preview: false,
                            focus: true,
                        },
                        window,
                        cx,
                    );
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum RowAction {
    Stage,
    Unstage,
    Discard,
    OpenFile,
}

impl RowAction {
    fn mutates_repository(self) -> bool {
        match self {
            RowAction::Stage | RowAction::Unstage | RowAction::Discard => true,
            RowAction::OpenFile => false,
        }
    }
}

fn display_file_name(repo_path: &RepoPath) -> String {
    repo_path
        .file_name()
        .unwrap_or_else(|| repo_path.as_unix_str())
        .to_string()
}

fn project_path_for(
    repository: &Entity<Repository>,
    repo_path: &RepoPath,
    cx: &AsyncApp,
) -> Option<ProjectPath> {
    repository.read_with(cx, |repository, cx| {
        repository.repo_path_to_project_path(repo_path, cx)
    })
}

/// `git checkout` rewrites the files on disk; buffers with unsaved edits would
/// otherwise keep showing the discarded changes.
async fn reload_dirty_buffers(
    project: &Entity<Project>,
    repository: &Entity<Repository>,
    files: &[ChangedFile],
    cx: &mut AsyncApp,
) -> Result<()> {
    let reloads = cx.update(|cx| {
        files
            .iter()
            .filter_map(|file| {
                let project_path = repository
                    .read(cx)
                    .repo_path_to_project_path(&file.repo_path, cx)?;
                let buffer = project.read(cx).get_open_buffer(&project_path, cx)?;
                buffer.update(cx, |buffer, cx| {
                    buffer.is_dirty().then(|| buffer.reload(cx))
                })
            })
            .collect::<Vec<_>>()
    });
    for reload in reloads {
        reload.await?;
    }
    Ok(())
}

async fn restore_from_index(
    project: &Entity<Project>,
    repository: &Entity<Repository>,
    repo_path: &RepoPath,
    cx: &mut AsyncApp,
) -> Result<()> {
    let project_path = project_path_for(repository, repo_path, cx)
        .with_context(|| format!("{} is outside of the project", repo_path.as_unix_str()))?;
    let buffer = project
        .update(cx, |project, cx| project.open_buffer(project_path, cx))
        .await?;
    let unstaged_diff = project
        .update(cx, |project, cx| {
            project.open_unstaged_diff(buffer.clone(), cx)
        })
        .await?;
    // Staged symlinks and non-UTF-8 blobs have no base text, and `Repository` has no
    // way to check out a raw index entry, so those are left untouched.
    let index_text = unstaged_diff
        .read_with(cx, |diff, cx| diff.base_text_string(cx))
        .with_context(|| {
            let path = repo_path.as_unix_str();
            format!(
                "The staged version of {path} isn't plain text, so its unstaged changes \
                 can't be discarded here. Run `git restore {path}` instead."
            )
        })?;
    // The diff's base text has normalized line endings, so the index's are read from
    // its raw entry. Otherwise the buffer would be saved with the working tree's.
    let index_line_ending = load_index_line_ending(repository, repo_path, cx).await?;
    // Buffers keep a BOM out of their text and write it back on save, so one left in
    // the index text would be saved twice.
    let (index_text, index_has_bom) = match index_text.strip_prefix('\u{FEFF}') {
        Some(text) => (text.to_string(), true),
        None => (index_text, false),
    };
    buffer.update(cx, |buffer, cx| {
        buffer.set_has_bom(index_has_bom);
        buffer.set_text(index_text, cx);
        if let Some(line_ending) = index_line_ending {
            buffer.set_line_ending(line_ending, cx);
        }
    });
    project
        .update(cx, |project, cx| project.save_buffer(buffer, cx))
        .await
}

/// Remote repositories don't expose raw index entries, so those buffers keep their
/// own line endings.
async fn load_index_line_ending(
    repository: &Entity<Repository>,
    repo_path: &RepoPath,
    cx: &mut AsyncApp,
) -> Result<Option<LineEnding>> {
    let repo_path = repo_path.clone();
    let index_text = repository
        .update(cx, |repository, _| {
            repository.send_job(None, move |state, _| async move {
                match state {
                    RepositoryState::Local(LocalRepositoryState { backend, .. }) => {
                        backend.load_index_text(repo_path).await
                    }
                    RepositoryState::Remote(_) => None,
                }
            })
        })
        .await?;
    Ok(index_text.map(|text| LineEnding::detect(&text)))
}

impl Focusable for ChangesView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ChangesView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("changes-view")
            .key_context(self.dispatch_context())
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::select_first))
            .on_action(cx.listener(Self::select_last))
            .on_action(cx.listener(Self::menu_select_next))
            .on_action(cx.listener(Self::menu_select_previous))
            .on_action(cx.listener(Self::menu_select_first))
            .on_action(cx.listener(Self::menu_select_last))
            .on_action(cx.listener(Self::collapse_selected))
            .on_action(cx.listener(Self::expand_selected))
            .on_action(cx.listener(Self::open_selected))
            .on_action(cx.listener(Self::open_selected_file))
            .on_action(cx.listener(Self::open_file_history))
            .when(self.has_write_access(cx), |this| {
                this.on_action(cx.listener(Self::toggle_staged))
                    .on_action(cx.listener(Self::stage_range))
                    .on_action(cx.listener(Self::stage_selected))
                    .on_action(cx.listener(Self::unstage_selected))
                    .on_action(cx.listener(Self::discard_selected))
                    .on_action(cx.listener(Self::add_to_gitignore))
            })
            .flex_1()
            .size_full()
            .overflow_hidden()
            .child(
                uniform_list(
                    "changes-view-rows",
                    self.rows.len(),
                    cx.processor(move |this, range: Range<usize>, window, cx| {
                        range
                            .filter_map(|index| {
                                let selected = this.selected_row == Some(index);
                                match this.rows.get(index)?.clone() {
                                    ChangesRow::Header {
                                        section,
                                        file_count,
                                    } => Some(this.render_header(
                                        index, section, file_count, selected, window, cx,
                                    )),
                                    ChangesRow::File(file) => {
                                        Some(this.render_file(index, &file, selected, window, cx))
                                    }
                                }
                            })
                            .collect()
                    }),
                )
                .size_full()
                .flex_grow()
                .track_scroll(&self.scroll_handle),
            )
            .custom_scrollbars(
                Scrollbars::for_settings::<GitPanelSettings>()
                    .tracked_scroll_handle(&self.scroll_handle),
                window,
                cx,
            )
            .children(self.context_menu.as_ref().map(|(menu, position, _)| {
                deferred(
                    anchored()
                        .position(*position)
                        .anchor(Corner::TopLeft)
                        .child(menu.clone()),
                )
                .with_priority(1)
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use editor::Editor;
    use git::status::{TrackedStatus, UnmergedStatus, UnmergedStatusCode};
    use gpui::{TestAppContext, VisualTestContext};
    use project::{FakeFs, Fs as _};
    use serde_json::json;
    use settings::SettingsStore;
    use std::{path::Path, sync::Arc};
    use theme::LoadThemes;
    use util::path;
    use workspace::{MultiWorkspace, SplitDirection};

    fn status_entry(path: &str, status: FileStatus) -> StatusEntry {
        StatusEntry {
            repo_path: RepoPath::new(path).expect("valid repo path"),
            status,
            diff_stat: None,
        }
    }

    fn tracked(index_status: StatusCode, worktree_status: StatusCode) -> FileStatus {
        FileStatus::Tracked(TrackedStatus {
            index_status,
            worktree_status,
        })
    }

    fn summary(rows: &[ChangesRow]) -> Vec<String> {
        rows.iter()
            .map(|row| match row {
                ChangesRow::Header {
                    section,
                    file_count,
                } => format!("{} ({file_count})", section.title()),
                ChangesRow::File(file) => {
                    format!(
                        "  {} {}",
                        file.change.letter(),
                        file.repo_path.as_unix_str()
                    )
                }
            })
            .collect()
    }

    #[test]
    fn partially_staged_files_appear_in_both_sections() {
        let files = changed_files([
            status_entry("a.txt", tracked(StatusCode::Modified, StatusCode::Modified)),
            status_entry("b.txt", tracked(StatusCode::Added, StatusCode::Unmodified)),
            status_entry(
                "c.txt",
                tracked(StatusCode::Unmodified, StatusCode::Deleted),
            ),
            status_entry("d.txt", FileStatus::Untracked),
            status_entry("e.txt", FileStatus::Ignored),
            status_entry(
                "f.txt",
                FileStatus::Unmerged(UnmergedStatus {
                    first_head: UnmergedStatusCode::Updated,
                    second_head: UnmergedStatusCode::Updated,
                }),
            ),
        ]);
        let rows = build_rows(&files, &HashSet::default());

        assert_eq!(
            summary(&rows),
            [
                "Merge Changes (1)",
                "  ! f.txt",
                "Staged Changes (2)",
                "  M a.txt",
                "  A b.txt",
                "Changes (3)",
                "  M a.txt",
                "  D c.txt",
                "  U d.txt",
            ]
        );
        assert!(files[1].has_staged_changes());
        assert!(!files[5].has_staged_changes());
    }

    #[test]
    fn only_files_gone_from_disk_are_missing_from_the_worktree() {
        let file = |status| {
            changed_files([status_entry("a.txt", status)])
                .into_iter()
                .next()
                .expect("a changed file")
        };
        let staged_deletion = file(tracked(StatusCode::Deleted, StatusCode::Unmodified));
        let unstaged_deletion = file(tracked(StatusCode::Unmodified, StatusCode::Deleted));
        let recreated = file(tracked(StatusCode::Deleted, StatusCode::Added));
        let modified = file(tracked(StatusCode::Modified, StatusCode::Unmodified));

        assert!(staged_deletion.is_missing_from_worktree());
        assert!(unstaged_deletion.is_missing_from_worktree());
        assert!(!recreated.is_missing_from_worktree());
        assert!(!modified.is_missing_from_worktree());
    }

    #[test]
    fn discarding_intent_to_add_files_deletes_them() {
        let file = |status| {
            changed_files([status_entry("a.txt", status)])
                .into_iter()
                .next()
                .expect("a changed file")
        };
        let intent_to_add = file(tracked(StatusCode::Unmodified, StatusCode::Added));
        let staged_addition = file(tracked(StatusCode::Added, StatusCode::Modified));
        let unstaged_change = file(tracked(StatusCode::Unmodified, StatusCode::Modified));

        assert!(intent_to_add.is_intent_to_add());
        assert!(intent_to_add.is_missing_from_index());
        assert!(!staged_addition.is_intent_to_add());
        assert!(!staged_addition.is_missing_from_index());
        assert!(!unstaged_change.is_missing_from_index());
    }

    #[test]
    fn diff_stats_are_only_kept_for_files_in_one_section() {
        let stat = Some(DiffStat {
            added: 2,
            deleted: 1,
        });
        let with_stat = |path, status| StatusEntry {
            diff_stat: stat,
            ..status_entry(path, status)
        };
        let files = changed_files([
            with_stat(
                "a.txt",
                tracked(StatusCode::Modified, StatusCode::Unmodified),
            ),
            with_stat(
                "b.txt",
                tracked(StatusCode::Unmodified, StatusCode::Modified),
            ),
            with_stat("c.txt", tracked(StatusCode::Modified, StatusCode::Modified)),
        ]);
        let diff_stats = files
            .iter()
            .map(|file| (file.repo_path.as_unix_str(), file.section, file.diff_stat))
            .collect::<Vec<_>>();

        assert_eq!(
            diff_stats,
            [
                ("a.txt", ChangesSection::Staged, stat),
                ("c.txt", ChangesSection::Staged, None),
                ("b.txt", ChangesSection::Unstaged, stat),
                ("c.txt", ChangesSection::Unstaged, None),
            ]
        );
    }

    #[test]
    fn files_that_were_never_committed_have_no_history() {
        let files = changed_files([
            status_entry("a.txt", tracked(StatusCode::Added, StatusCode::Modified)),
            status_entry("b.txt", tracked(StatusCode::Unmodified, StatusCode::Added)),
            status_entry("c.txt", FileStatus::Untracked),
            status_entry("d.txt", tracked(StatusCode::Modified, StatusCode::Modified)),
        ]);
        let with_history = files
            .iter()
            .map(|file| {
                (
                    file.repo_path.as_unix_str(),
                    file.section,
                    file.has_history(),
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(
            with_history,
            [
                ("a.txt", ChangesSection::Staged, false),
                ("d.txt", ChangesSection::Staged, true),
                ("a.txt", ChangesSection::Unstaged, false),
                ("b.txt", ChangesSection::Unstaged, false),
                ("c.txt", ChangesSection::Unstaged, false),
                ("d.txt", ChangesSection::Unstaged, true),
            ]
        );
    }

    #[test]
    fn collapsed_sections_keep_their_header() {
        let files = changed_files([
            status_entry("a.txt", tracked(StatusCode::Modified, StatusCode::Modified)),
            status_entry("b.txt", FileStatus::Untracked),
        ]);
        let collapsed = HashSet::from_iter([ChangesSection::Unstaged]);
        let rows = build_rows(&files, &collapsed);

        assert_eq!(
            summary(&rows),
            ["Staged Changes (1)", "  M a.txt", "Changes (2)"]
        );
    }

    #[test]
    fn reselect_follows_the_row_or_keeps_the_position() {
        let before = build_rows(
            &changed_files([
                status_entry(
                    "a.txt",
                    tracked(StatusCode::Unmodified, StatusCode::Modified),
                ),
                status_entry(
                    "b.txt",
                    tracked(StatusCode::Unmodified, StatusCode::Modified),
                ),
            ]),
            &HashSet::default(),
        );
        // Staging "a.txt" moves it into "Staged Changes".
        let after = build_rows(
            &changed_files([
                status_entry(
                    "a.txt",
                    tracked(StatusCode::Modified, StatusCode::Unmodified),
                ),
                status_entry(
                    "b.txt",
                    tracked(StatusCode::Unmodified, StatusCode::Modified),
                ),
            ]),
            &HashSet::default(),
        );

        assert_eq!(
            summary(&after),
            [
                "Staged Changes (1)",
                "  M a.txt",
                "Changes (1)",
                "  M b.txt"
            ]
        );

        let a_key = before[1].key();
        assert_eq!(reselect(&after, Some(&a_key), Some(1)), Some(3));

        let b_key = before[2].key();
        assert_eq!(reselect(&after, Some(&b_key), Some(2)), Some(3));

        let past_the_end = RowKey::File(
            ChangesSection::Unstaged,
            RepoPath::new("z.txt").expect("valid"),
        );
        assert_eq!(reselect(&after, Some(&past_the_end), Some(10)), Some(3));

        let header = RowKey::Header(ChangesSection::Merge);
        assert_eq!(reselect(&after, Some(&header), Some(10)), Some(3));
        assert_eq!(reselect(&[], Some(&header), Some(0)), None);
    }

    fn init_test(cx: &mut TestAppContext) {
        zlog::init_test();
        cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
            theme::init(LoadThemes::JustBase, cx);
            editor::init(cx);
            crate::init(cx);
        });
    }

    async fn setup(
        cx: &mut TestAppContext,
    ) -> (
        Arc<FakeFs>,
        Entity<Workspace>,
        Entity<ChangesView>,
        VisualTestContext,
    ) {
        setup_repo(
            cx,
            json!({
                "a.txt": "three\n",
                "b.txt": "b changed\n",
                "c.txt": "new\n",
            }),
            &[("a.txt", "one\n"), ("b.txt", "b\n")],
            &[("a.txt", "two\n"), ("b.txt", "b\n")],
        )
        .await
    }

    async fn setup_repo(
        cx: &mut TestAppContext,
        worktree: serde_json::Value,
        head: &[(&str, &str)],
        index: &[(&str, &str)],
    ) -> (
        Arc<FakeFs>,
        Entity<Workspace>,
        Entity<ChangesView>,
        VisualTestContext,
    ) {
        init_test(cx);
        let fs = FakeFs::new(cx.background_executor.clone());
        fs.insert_tree(path!("/project"), worktree).await;
        fs.create_dir(Path::new(path!("/project/.git")))
            .await
            .expect("can create .git");
        let dot_git = Path::new(path!("/project/.git"));
        fn contents<'a>(files: &[(&'a str, &str)]) -> Vec<(&'a str, String)> {
            files
                .iter()
                .map(|(path, text)| (*path, text.to_string()))
                .collect()
        }
        fs.set_head_for_repo(dot_git, &contents(head), "deadbeef");
        fs.set_index_for_repo(dot_git, &contents(index));

        let project = Project::test(fs.clone(), [Path::new(path!("/project"))], cx).await;
        let window_handle =
            cx.add_window(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
        let workspace = window_handle
            .read_with(cx, |multi_workspace, _| multi_workspace.workspace().clone())
            .expect("window exists");
        let mut cx = VisualTestContext::from_window(window_handle.into(), cx);
        cx.run_until_parked();

        let view = workspace.update_in(&mut cx, |workspace, window, cx| {
            let project = workspace.project().clone();
            let workspace = workspace.weak_handle();
            cx.new(|cx| ChangesView::new(project, workspace, window, cx))
        });
        cx.run_until_parked();
        (fs, workspace, view, cx)
    }

    fn view_summary(view: &Entity<ChangesView>, cx: &mut VisualTestContext) -> Vec<String> {
        view.read_with(cx, |view, _| {
            let mut lines = summary(&view.rows);
            if let Some(selected) = view.selected_row
                && let Some(line) = lines.get_mut(selected)
            {
                line.push_str("  <== selected");
            }
            lines
        })
    }

    fn select_file(
        view: &Entity<ChangesView>,
        section: ChangesSection,
        path: &str,
        cx: &mut VisualTestContext,
    ) {
        let key = RowKey::File(section, RepoPath::new(path).expect("valid repo path"));
        view.update(cx, |view, cx| {
            let index = view
                .rows
                .iter()
                .position(|row| row.key() == key)
                .expect("row exists");
            view.select_row(index, cx);
        });
    }

    #[gpui::test]
    async fn test_staging_moves_files_between_sections(cx: &mut TestAppContext) {
        let (_fs, _workspace, view, mut cx) = setup(cx).await;
        let cx = &mut cx;

        assert_eq!(
            view_summary(&view, cx),
            [
                "Staged Changes (1)",
                "  M a.txt",
                "Changes (3)",
                "  M a.txt",
                "  M b.txt",
                "  U c.txt",
            ]
        );

        select_file(&view, ChangesSection::Unstaged, "b.txt", cx);
        view.update_in(cx, |view, window, cx| {
            view.toggle_staged(&ToggleStaged, window, cx)
        });
        cx.run_until_parked();
        assert_eq!(
            view_summary(&view, cx),
            [
                "Staged Changes (2)",
                "  M a.txt",
                "  M b.txt",
                "Changes (2)",
                "  M a.txt",
                "  U c.txt  <== selected",
            ]
        );

        select_file(&view, ChangesSection::Staged, "b.txt", cx);
        view.update_in(cx, |view, window, cx| {
            view.toggle_staged(&ToggleStaged, window, cx)
        });
        cx.run_until_parked();
        assert_eq!(
            view_summary(&view, cx),
            [
                "Staged Changes (1)",
                "  M a.txt  <== selected",
                "Changes (3)",
                "  M a.txt",
                "  M b.txt",
                "  U c.txt",
            ]
        );

        // Staging the "Changes" header stages every file in it.
        view.update_in(cx, |view, window, cx| {
            view.select_row(2, cx);
            view.stage_selected(&StageFile, window, cx);
        });
        cx.run_until_parked();
        assert_eq!(
            view_summary(&view, cx),
            [
                "Staged Changes (3)",
                "  M a.txt",
                "  M b.txt  <== selected",
                "  A c.txt",
            ]
        );
    }

    #[gpui::test]
    async fn test_opening_rows_shows_single_file_diffs(cx: &mut TestAppContext) {
        let (_fs, workspace, view, mut cx) = setup(cx).await;
        let cx = &mut cx;

        let open_selected = |view: &Entity<ChangesView>, cx: &mut VisualTestContext| {
            view.update_in(cx, |view, window, cx| {
                view.open_selected(&menu::Confirm, window, cx)
            });
            cx.run_until_parked();
        };
        let active_diff = |workspace: &Entity<Workspace>, cx: &mut VisualTestContext| {
            workspace.read_with(cx, |workspace, cx| {
                let item = workspace.active_item(cx).expect("an item is open");
                let title = item.tab_content_text(0, cx).to_string();
                let editor = item
                    .act_as::<Editor>(cx)
                    .expect("diff views expose an editor");
                let text = editor.read(cx).text(cx);
                (title, text)
            })
        };
        let item_count = |workspace: &Entity<Workspace>, cx: &mut VisualTestContext| {
            workspace.read_with(cx, |workspace, cx| {
                workspace.active_pane().read(cx).items_len()
            })
        };

        select_file(&view, ChangesSection::Staged, "a.txt", cx);
        open_selected(&view, cx);
        assert_eq!(
            active_diff(&workspace, cx),
            ("a.txt (Index)".to_string(), "two\n".to_string())
        );

        select_file(&view, ChangesSection::Unstaged, "a.txt", cx);
        open_selected(&view, cx);
        assert_eq!(
            active_diff(&workspace, cx),
            ("a.txt (Working Tree)".to_string(), "three\n".to_string())
        );
        // Both were opened as preview tabs, so the second one replaced the first.
        assert_eq!(item_count(&workspace, cx), 1);

        // Reopening an already open diff activates it instead of adding a tab.
        open_selected(&view, cx);
        assert_eq!(item_count(&workspace, cx), 1);

        // Untracked files have nothing to diff against, so the file itself is opened.
        select_file(&view, ChangesSection::Unstaged, "c.txt", cx);
        open_selected(&view, cx);
        let active_title = workspace.read_with(cx, |workspace, cx| {
            workspace
                .active_item(cx)
                .expect("an item is open")
                .tab_content_text(0, cx)
                .to_string()
        });
        assert_eq!(active_title, "c.txt");
    }

    #[gpui::test]
    async fn test_newer_previews_win_over_slower_older_ones(cx: &mut TestAppContext) {
        let (_fs, workspace, view, mut cx) = setup(cx).await;
        let cx = &mut cx;

        // A staged diff takes more steps to load than a working tree one, so the
        // older preview would finish last.
        view.update_in(cx, |view, window, cx| {
            for (section, path) in [
                (ChangesSection::Staged, "a.txt"),
                (ChangesSection::Unstaged, "b.txt"),
            ] {
                let key = RowKey::File(section, RepoPath::new(path).expect("valid repo path"));
                let row = view
                    .rows
                    .iter()
                    .find(|row| row.key() == key)
                    .cloned()
                    .expect("row exists");
                let options = OpenChangesDiff {
                    preview: true,
                    focus: false,
                };
                view.open_row(&row, options, window, cx);
            }
        });
        cx.run_until_parked();

        workspace.read_with(cx, |workspace, cx| {
            let title = workspace
                .active_item(cx)
                .expect("a preview is open")
                .tab_content_text(0, cx);
            assert_eq!(title, "b.txt (Working Tree)");
        });
    }

    #[gpui::test]
    async fn test_pinned_opens_win_over_slower_older_previews(cx: &mut TestAppContext) {
        let (_fs, workspace, view, mut cx) = setup(cx).await;
        let cx = &mut cx;

        view.update_in(cx, |view, window, cx| {
            for (section, path, preview) in [
                (ChangesSection::Staged, "a.txt", true),
                (ChangesSection::Unstaged, "b.txt", false),
            ] {
                let key = RowKey::File(section, RepoPath::new(path).expect("valid repo path"));
                let row = view
                    .rows
                    .iter()
                    .find(|row| row.key() == key)
                    .cloned()
                    .expect("row exists");
                let options = OpenChangesDiff {
                    preview,
                    focus: false,
                };
                view.open_row(&row, options, window, cx);
            }
        });
        cx.run_until_parked();

        workspace.read_with(cx, |workspace, cx| {
            let titles = workspace
                .items(cx)
                .map(|item| item.tab_content_text(0, cx))
                .collect::<Vec<_>>();
            assert_eq!(titles, ["b.txt (Working Tree)"]);
        });
    }

    #[gpui::test]
    async fn test_slower_older_pinned_opens_stay_behind_newer_ones(cx: &mut TestAppContext) {
        let (_fs, workspace, view, mut cx) = setup(cx).await;
        let cx = &mut cx;

        view.update_in(cx, |view, window, cx| {
            for (section, path) in [
                (ChangesSection::Staged, "a.txt"),
                (ChangesSection::Unstaged, "b.txt"),
            ] {
                let key = RowKey::File(section, RepoPath::new(path).expect("valid repo path"));
                let row = view
                    .rows
                    .iter()
                    .find(|row| row.key() == key)
                    .cloned()
                    .expect("row exists");
                let options = OpenChangesDiff {
                    preview: false,
                    focus: false,
                };
                view.open_row(&row, options, window, cx);
            }
        });
        cx.run_until_parked();

        workspace.read_with(cx, |workspace, cx| {
            let mut titles = workspace
                .items(cx)
                .map(|item| item.tab_content_text(0, cx))
                .collect::<Vec<_>>();
            titles.sort();
            assert_eq!(titles, ["a.txt (Index)", "b.txt (Working Tree)"]);
            let active_title = workspace
                .active_item(cx)
                .expect("a diff is open")
                .tab_content_text(0, cx);
            assert_eq!(active_title, "b.txt (Working Tree)");
        });
    }

    #[gpui::test]
    async fn test_slower_older_file_opens_stay_behind_newer_ones(cx: &mut TestAppContext) {
        let (_fs, workspace, view, mut cx) = setup(cx).await;
        let cx = &mut cx;
        let open = |section, path: &str, cx: &mut VisualTestContext| {
            view.update_in(cx, |view, window, cx| {
                let key = RowKey::File(section, RepoPath::new(path).expect("valid repo path"));
                let row = view
                    .rows
                    .iter()
                    .find(|row| row.key() == key)
                    .cloned()
                    .expect("row exists");
                let options = OpenChangesDiff {
                    preview: false,
                    focus: false,
                };
                view.open_row(&row, options, window, cx);
            });
        };
        open(ChangesSection::Unstaged, "b.txt", cx);
        cx.run_until_parked();

        // Switching back to an open diff is immediate, so the untracked file, which
        // opens as a file, finishes loading last.
        open(ChangesSection::Unstaged, "c.txt", cx);
        open(ChangesSection::Unstaged, "b.txt", cx);
        cx.run_until_parked();

        workspace.read_with(cx, |workspace, cx| {
            let mut titles = workspace
                .items(cx)
                .map(|item| item.tab_content_text(0, cx))
                .collect::<Vec<_>>();
            titles.sort();
            assert_eq!(titles, ["b.txt (Working Tree)", "c.txt"]);
            let active_title = workspace
                .active_item(cx)
                .expect("a diff is open")
                .tab_content_text(0, cx);
            assert_eq!(active_title, "b.txt (Working Tree)");
        });
    }

    #[gpui::test]
    async fn test_slower_older_file_previews_do_not_replace_newer_ones(cx: &mut TestAppContext) {
        let (_fs, workspace, view, mut cx) = setup(cx).await;
        let cx = &mut cx;
        let preview = |section, path: &str, cx: &mut VisualTestContext| {
            view.update_in(cx, |view, window, cx| {
                let key = RowKey::File(section, RepoPath::new(path).expect("valid repo path"));
                let row = view
                    .rows
                    .iter()
                    .find(|row| row.key() == key)
                    .cloned()
                    .expect("row exists");
                let options = OpenChangesDiff {
                    preview: true,
                    focus: false,
                };
                view.open_row(&row, options, window, cx);
            });
        };
        preview(ChangesSection::Unstaged, "b.txt", cx);
        cx.run_until_parked();

        // Switching back to an open diff is immediate, so the untracked file, which
        // opens as a file, finishes loading last.
        preview(ChangesSection::Unstaged, "c.txt", cx);
        preview(ChangesSection::Unstaged, "b.txt", cx);
        cx.run_until_parked();

        workspace.read_with(cx, |workspace, cx| {
            let titles = workspace
                .items(cx)
                .map(|item| item.tab_content_text(0, cx))
                .collect::<Vec<_>>();
            assert_eq!(titles, ["b.txt (Working Tree)"]);
        });
    }

    #[gpui::test]
    async fn test_diffs_open_in_the_pane_that_was_active_when_clicked(cx: &mut TestAppContext) {
        let (_fs, workspace, view, mut cx) = setup(cx).await;
        let cx = &mut cx;
        let clicked_pane = workspace.read_with(cx, |workspace, _| workspace.active_pane().clone());

        select_file(&view, ChangesSection::Staged, "a.txt", cx);
        view.update_in(cx, |view, window, cx| {
            let row = view
                .selected_changes_row()
                .cloned()
                .expect("a row is selected");
            let options = OpenChangesDiff {
                preview: true,
                focus: false,
            };
            view.open_row(&row, options, window, cx);
        });
        let other_pane = workspace.update_in(cx, |workspace, window, cx| {
            let pane =
                workspace.split_pane(clicked_pane.clone(), SplitDirection::Right, window, cx);
            window.focus(&pane.focus_handle(cx), cx);
            pane
        });
        workspace.read_with(cx, |workspace, _| {
            assert_eq!(workspace.active_pane(), &other_pane);
        });
        cx.run_until_parked();

        let item_counts =
            [&clicked_pane, &other_pane].map(|pane| pane.read_with(cx, |pane, _| pane.items_len()));
        assert_eq!(item_counts, [1, 0]);
    }

    #[gpui::test]
    async fn test_opening_a_row_twice_while_loading_adds_one_tab(cx: &mut TestAppContext) {
        let (_fs, workspace, view, mut cx) = setup(cx).await;
        let cx = &mut cx;

        select_file(&view, ChangesSection::Unstaged, "b.txt", cx);
        // A double-click opens a preview on the first click and pins it on the second.
        view.update_in(cx, |view, window, cx| {
            let row = view
                .selected_changes_row()
                .cloned()
                .expect("a row is selected");
            for preview in [true, false] {
                let options = OpenChangesDiff {
                    preview,
                    focus: false,
                };
                view.open_row(&row, options, window, cx);
            }
        });
        cx.run_until_parked();

        workspace.read_with(cx, |workspace, cx| {
            assert_eq!(workspace.items_of_type::<ChangesDiffView>(cx).count(), 1);
            assert_eq!(workspace.active_pane().read(cx).preview_item_id(), None);
        });
    }

    #[gpui::test]
    async fn test_discard_keeps_staged_changes(cx: &mut TestAppContext) {
        let (fs, _workspace, view, mut cx) = setup(cx).await;
        let cx = &mut cx;

        select_file(&view, ChangesSection::Unstaged, "a.txt", cx);
        view.update_in(cx, |view, window, cx| {
            view.discard_selected(&RestoreFile { skip_prompt: true }, window, cx)
        });
        cx.run_until_parked();
        assert_eq!(
            fs.load(Path::new(path!("/project/a.txt")))
                .await
                .expect("a.txt exists"),
            "two\n"
        );

        select_file(&view, ChangesSection::Unstaged, "c.txt", cx);
        view.update_in(cx, |view, window, cx| {
            view.discard_selected(&RestoreFile { skip_prompt: true }, window, cx)
        });
        cx.run_until_parked();
        assert!(!fs.is_file(Path::new(path!("/project/c.txt"))).await);

        assert_eq!(
            view_summary(&view, cx),
            [
                "Staged Changes (1)",
                "  M a.txt",
                "Changes (1)",
                "  M b.txt  <== selected",
            ]
        );

        // Staged changes cannot be discarded from the "Staged Changes" section.
        select_file(&view, ChangesSection::Staged, "a.txt", cx);
        view.update_in(cx, |view, window, cx| {
            view.discard_selected(&RestoreFile { skip_prompt: true }, window, cx)
        });
        cx.run_until_parked();
        assert_eq!(
            fs.load(Path::new(path!("/project/a.txt")))
                .await
                .expect("a.txt exists"),
            "two\n"
        );
    }

    #[gpui::test]
    async fn test_stage_range_stages_files_from_the_last_staged_one(cx: &mut TestAppContext) {
        let files = ["a.txt", "b.txt", "c.txt", "d.txt", "e.txt"];
        let (_fs, _workspace, view, mut cx) = setup_repo(
            cx,
            json!({
                "a.txt": "changed\n",
                "b.txt": "changed\n",
                "c.txt": "changed\n",
                "d.txt": "changed\n",
                "e.txt": "changed\n",
            }),
            &files.map(|path| (path, "original\n")),
            &files.map(|path| (path, "original\n")),
        )
        .await;
        let cx = &mut cx;

        select_file(&view, ChangesSection::Unstaged, "b.txt", cx);
        view.update_in(cx, |view, window, cx| {
            view.toggle_staged(&ToggleStaged, window, cx)
        });
        cx.run_until_parked();
        select_file(&view, ChangesSection::Unstaged, "d.txt", cx);
        view.update_in(cx, |view, window, cx| {
            view.stage_range(&StageRange, window, cx)
        });
        cx.run_until_parked();
        assert_eq!(
            view_summary(&view, cx),
            [
                "Staged Changes (3)",
                "  M b.txt",
                "  M c.txt",
                "  M d.txt",
                "Changes (2)",
                "  M a.txt",
                "  M e.txt  <== selected",
            ]
        );
    }

    #[gpui::test]
    async fn test_discard_restores_the_index_line_endings(cx: &mut TestAppContext) {
        let (fs, _workspace, view, mut cx) = setup_repo(
            cx,
            json!({ "a.txt": "one\nthree\n" }),
            &[("a.txt", "one\r\n")],
            &[("a.txt", "one\r\ntwo\r\n")],
        )
        .await;
        let cx = &mut cx;

        select_file(&view, ChangesSection::Unstaged, "a.txt", cx);
        view.update_in(cx, |view, window, cx| {
            view.discard_selected(&RestoreFile { skip_prompt: true }, window, cx)
        });
        cx.run_until_parked();
        assert_eq!(
            fs.load(Path::new(path!("/project/a.txt")))
                .await
                .expect("a.txt exists"),
            "one\r\ntwo\r\n"
        );
    }

    #[gpui::test]
    async fn test_discard_restores_the_index_byte_order_mark(cx: &mut TestAppContext) {
        let (fs, _workspace, view, mut cx) = setup_repo(
            cx,
            json!({
                "a.txt": "\u{FEFF}one\nthree\n",
                "b.txt": "one\nthree\n",
                "c.txt": "\u{FEFF}one\nthree\n",
            }),
            &[("a.txt", "one\n"), ("b.txt", "one\n"), ("c.txt", "one\n")],
            &[
                ("a.txt", "\u{FEFF}one\ntwo\n"),
                ("b.txt", "\u{FEFF}one\ntwo\n"),
                ("c.txt", "one\ntwo\n"),
            ],
        )
        .await;
        let cx = &mut cx;

        for name in ["a.txt", "b.txt", "c.txt"] {
            select_file(&view, ChangesSection::Unstaged, name, cx);
            view.update_in(cx, |view, window, cx| {
                view.discard_selected(&RestoreFile { skip_prompt: true }, window, cx)
            });
            cx.run_until_parked();
        }
        for (name, expected) in [
            ("a.txt", "\u{FEFF}one\ntwo\n"),
            ("b.txt", "\u{FEFF}one\ntwo\n"),
            ("c.txt", "one\ntwo\n"),
        ] {
            assert_eq!(
                fs.load(&Path::new(path!("/project")).join(name))
                    .await
                    .expect("file exists"),
                expected,
                "{name}"
            );
        }
    }

    #[gpui::test]
    async fn test_discard_recreated_file_keeps_staged_deletion(cx: &mut TestAppContext) {
        let (fs, _workspace, view, mut cx) = setup_repo(
            cx,
            json!({ "a.txt": "recreated\n" }),
            &[("a.txt", "one\n")],
            &[],
        )
        .await;
        let cx = &mut cx;
        assert_eq!(
            view_summary(&view, cx),
            [
                "Staged Changes (1)",
                "  D a.txt",
                "Changes (1)",
                "  A a.txt",
            ]
        );

        select_file(&view, ChangesSection::Unstaged, "a.txt", cx);
        view.update_in(cx, |view, window, cx| {
            view.discard_selected(&RestoreFile { skip_prompt: true }, window, cx)
        });
        cx.run_until_parked();
        assert!(!fs.is_file(Path::new(path!("/project/a.txt"))).await);
        // The fake repository reports a file missing from both the index and the
        // working tree as deleted in both, so only the staged deletion is checked.
        assert_eq!(
            view_summary(&view, cx)[..2],
            ["Staged Changes (1)", "  D a.txt"]
        );
    }

    #[gpui::test]
    async fn test_sync_changes_view_reports_layout_switches(cx: &mut TestAppContext) {
        let (_fs, workspace, _view, mut cx) = setup(cx).await;
        let cx = &mut cx;
        let set_layout = |layout: GitPanelChangesLayout, cx: &mut VisualTestContext| {
            cx.update(|_, cx| {
                cx.update_global::<SettingsStore, _>(|store, cx| {
                    store.update_user_settings(cx, |settings| {
                        settings.git_panel.get_or_insert_default().changes_layout = Some(layout);
                    })
                });
            });
        };
        let sync = |changes_view: &mut Option<Entity<ChangesView>>, cx: &mut VisualTestContext| {
            workspace.update_in(cx, |workspace, window, cx| {
                let project = workspace.project().clone();
                let workspace = workspace.weak_handle();
                sync_changes_view(changes_view, &project, &workspace, window, cx)
            })
        };

        let mut changes_view = None;
        set_layout(GitPanelChangesLayout::Zed, cx);
        assert!(!sync(&mut changes_view, cx));
        assert!(changes_view.is_none());

        set_layout(GitPanelChangesLayout::Vscode, cx);
        assert!(sync(&mut changes_view, cx));
        assert!(changes_view.is_some());
        assert!(!sync(&mut changes_view, cx));

        set_layout(GitPanelChangesLayout::Zed, cx);
        assert!(sync(&mut changes_view, cx));
        assert!(changes_view.is_none());
    }

    #[gpui::test]
    async fn test_binary_files_open_instead_of_their_diff(cx: &mut TestAppContext) {
        let (fs, workspace, view, mut cx) = setup_repo(
            cx,
            json!({}),
            &[("image.bin", "\0old")],
            &[("image.bin", "\0old")],
        )
        .await;
        let cx = &mut cx;
        fs.insert_file(path!("/project/image.bin"), vec![0, 1, 2, 3])
            .await;
        cx.run_until_parked();

        select_file(&view, ChangesSection::Unstaged, "image.bin", cx);
        view.update_in(cx, |view, window, cx| {
            view.open_selected(&menu::Confirm, window, cx)
        });
        cx.run_until_parked();

        workspace.read_with(cx, |workspace, cx| {
            assert_eq!(workspace.items_of_type::<ChangesDiffView>(cx).count(), 0);
            let active_title = workspace
                .active_item(cx)
                .expect("the file itself is opened")
                .tab_content_text(0, cx);
            assert_eq!(active_title, "image.bin");
        });
    }

    #[gpui::test]
    async fn test_changed_staged_binary_files_are_not_opened_from_disk(cx: &mut TestAppContext) {
        let (fs, workspace, view, mut cx) = setup_repo(
            cx,
            json!({}),
            &[("image.bin", "\0old")],
            &[("image.bin", "\0staged")],
        )
        .await;
        let cx = &mut cx;
        fs.insert_file(path!("/project/image.bin"), vec![0, 1, 2, 3])
            .await;
        cx.run_until_parked();

        select_file(&view, ChangesSection::Staged, "image.bin", cx);
        view.update_in(cx, |view, window, cx| {
            view.open_selected(&menu::Confirm, window, cx)
        });
        cx.run_until_parked();

        assert!(cx.has_pending_prompt());
        workspace.read_with(cx, |workspace, cx| {
            assert!(workspace.active_item(cx).is_none());
        });
    }

    async fn setup_changed_binary(
        cx: &mut TestAppContext,
        index: &str,
    ) -> (Entity<Workspace>, Entity<ChangesView>, VisualTestContext) {
        let (fs, workspace, view, cx) = setup_repo(
            cx,
            json!({}),
            &[("image.bin", "\0old")],
            &[("image.bin", index)],
        )
        .await;
        fs.insert_file(path!("/project/image.bin"), vec![0, 1, 2, 3])
            .await;
        cx.run_until_parked();
        (workspace, view, cx)
    }

    fn open_request(view: &ChangesView) -> OpenRequest {
        OpenRequest {
            id: view.latest_open,
            repository: view
                .active_repository
                .clone()
                .expect("a repository is active"),
            pane: None,
        }
    }

    #[gpui::test]
    async fn test_binary_fallbacks_recheck_the_file_status(cx: &mut TestAppContext) {
        let (_workspace, view, mut cx) = setup_changed_binary(cx, "\0staged").await;
        let cx = &mut cx;
        // The row as it was before the file on disk changed after staging.
        let stale_file = ChangedFile {
            section: ChangesSection::Staged,
            repo_path: RepoPath::new("image.bin").expect("valid repo path"),
            change: FileChange::Modified,
            status: tracked(StatusCode::Modified, StatusCode::Unmodified),
            diff_stat: None,
        };

        let result = view.update_in(cx, |view, window, cx| {
            let request = open_request(view);
            let options = OpenChangesDiff {
                preview: false,
                focus: false,
            };
            view.open_without_diff(
                &stale_file,
                ChangesDiffKind::Index,
                options,
                &request,
                window,
                cx,
            )
        });

        let error = result.expect_err("the staged version isn't on disk");
        assert!(
            error
                .to_string()
                .contains("has changed since it was staged"),
            "{error}"
        );
    }

    #[gpui::test]
    async fn test_binary_fallbacks_use_the_repository_they_were_opened_in(cx: &mut TestAppContext) {
        let (workspace, view, mut cx) = setup_changed_binary(cx, "\0old").await;
        let cx = &mut cx;
        select_file(&view, ChangesSection::Unstaged, "image.bin", cx);

        view.update_in(cx, |view, window, cx| {
            let request = open_request(view);
            let Some(ChangesRow::File(file)) = view.selected_changes_row().cloned() else {
                panic!("a file is selected");
            };
            view.active_repository = None;
            let options = OpenChangesDiff {
                preview: false,
                focus: false,
            };
            view.open_without_diff(
                &file,
                ChangesDiffKind::WorkingTree,
                options,
                &request,
                window,
                cx,
            )
        })
        .expect("the file can be opened");
        cx.run_until_parked();

        workspace.read_with(cx, |workspace, cx| {
            let title = workspace
                .active_item(cx)
                .expect("the file is opened")
                .tab_content_text(0, cx);
            assert_eq!(title, "image.bin");
        });
    }

    #[gpui::test]
    async fn test_binary_fallbacks_open_in_the_pane_that_was_active_when_clicked(
        cx: &mut TestAppContext,
    ) {
        let (workspace, view, mut cx) = setup_changed_binary(cx, "\0old").await;
        let cx = &mut cx;
        let clicked_pane = workspace.read_with(cx, |workspace, _| workspace.active_pane().clone());

        select_file(&view, ChangesSection::Unstaged, "image.bin", cx);
        view.update_in(cx, |view, window, cx| {
            view.open_selected(&menu::Confirm, window, cx)
        });
        let other_pane = workspace.update_in(cx, |workspace, window, cx| {
            let pane =
                workspace.split_pane(clicked_pane.clone(), SplitDirection::Right, window, cx);
            window.focus(&pane.focus_handle(cx), cx);
            pane
        });
        cx.run_until_parked();

        let item_counts =
            [&clicked_pane, &other_pane].map(|pane| pane.read_with(cx, |pane, _| pane.items_len()));
        assert_eq!(item_counts, [1, 0]);
    }

    #[gpui::test]
    async fn test_staged_deletions_of_binary_files_are_not_shown_as_empty(cx: &mut TestAppContext) {
        let (fs, workspace, view, mut cx) =
            setup_repo(cx, json!({}), &[("image.bin", "binary")], &[]).await;
        let cx = &mut cx;
        // The fake repository only stores text, so a binary HEAD blob is simulated by
        // dropping it without a git event: the status still reports the deletion, but
        // the committed text can't be loaded.
        fs.with_git_state(Path::new(path!("/project/.git")), false, |state| {
            state.head_contents.clear();
        })
        .expect("repository exists");

        select_file(&view, ChangesSection::Staged, "image.bin", cx);
        view.update_in(cx, |view, window, cx| {
            view.open_selected(&menu::Confirm, window, cx)
        });
        cx.run_until_parked();

        assert!(cx.has_pending_prompt());
        workspace.read_with(cx, |workspace, cx| {
            assert_eq!(workspace.items_of_type::<ChangesDiffView>(cx).count(), 0);
        });
    }

    #[gpui::test]
    async fn test_binary_blobs_that_are_valid_utf8_are_not_diffed(cx: &mut TestAppContext) {
        let (_fs, workspace, view, mut cx) = setup_repo(
            cx,
            json!({ "replaced.bin": "new\n", "unstaged.bin": "new\n" }),
            &[
                ("deleted.bin", "\0\0old"),
                ("replaced.bin", "\0\0old"),
                ("unstaged.bin", "\0\0old"),
            ],
            &[("replaced.bin", "new\n"), ("unstaged.bin", "\0\0old")],
        )
        .await;
        let cx = &mut cx;
        let open = |section, name, cx: &mut VisualTestContext| {
            select_file(&view, section, name, cx);
            view.update_in(cx, |view, window, cx| {
                view.open_selected(&menu::Confirm, window, cx)
            });
            cx.run_until_parked();
        };
        let active_title = |cx: &mut VisualTestContext| {
            workspace.read_with(cx, |workspace, cx| {
                workspace
                    .active_item(cx)
                    .map(|item| item.tab_content_text(0, cx).to_string())
            })
        };

        open(ChangesSection::Staged, "deleted.bin", cx);
        let (_, detail) = cx.pending_prompt().expect("an error is shown");
        assert!(detail.contains("isn't a text file"), "{detail}");
        cx.simulate_prompt_answer("Ok");
        assert_eq!(active_title(cx), None);

        open(ChangesSection::Staged, "replaced.bin", cx);
        assert_eq!(active_title(cx).as_deref(), Some("replaced.bin"));

        open(ChangesSection::Unstaged, "unstaged.bin", cx);
        assert_eq!(active_title(cx).as_deref(), Some("unstaged.bin"));
        workspace.read_with(cx, |workspace, cx| {
            assert_eq!(workspace.items_of_type::<ChangesDiffView>(cx).count(), 0);
        });
    }

    #[gpui::test]
    async fn test_symlinks_do_not_open_their_target(cx: &mut TestAppContext) {
        let (fs, workspace, view, mut cx) = setup_repo(
            cx,
            json!({ "target.txt": "target\n" }),
            &[("target.txt", "target\n")],
            &[("target.txt", "target\n"), ("link", "target\n")],
        )
        .await;
        let cx = &mut cx;
        fs.create_symlink(
            Path::new(path!("/project/link")),
            std::path::PathBuf::from(path!("/project/target.txt")),
        )
        .await
        .expect("can create the symlink");
        cx.run_until_parked();

        // The fake repository doesn't follow symlinks, so it also reports the link as
        // deleted from the worktree; the message tells the symlink check apart.
        for section in [ChangesSection::Staged, ChangesSection::Unstaged] {
            select_file(&view, section, "link", cx);
            view.update_in(cx, |view, window, cx| {
                view.open_selected(&menu::Confirm, window, cx)
            });
            cx.run_until_parked();

            let (_, detail) = cx.pending_prompt().expect("an error is shown");
            assert!(detail.contains("is a symlink"), "{detail}");
            cx.simulate_prompt_answer("Ok");
            workspace.read_with(cx, |workspace, cx| {
                assert!(workspace.active_item(cx).is_none());
            });
        }
    }

    #[gpui::test]
    async fn test_add_to_gitignore_ignores_the_selected_untracked_file(cx: &mut TestAppContext) {
        let (fs, _workspace, view, mut cx) = setup(cx).await;
        let cx = &mut cx;

        select_file(&view, ChangesSection::Unstaged, "c.txt", cx);
        view.update_in(cx, |view, window, cx| {
            view.add_to_gitignore(&AddToGitignore, window, cx)
        });
        cx.run_until_parked();
        assert_eq!(
            fs.load(Path::new(path!("/project/.gitignore")))
                .await
                .expect(".gitignore exists"),
            "c.txt\n"
        );

        // Tracked files can't be ignored this way.
        select_file(&view, ChangesSection::Unstaged, "b.txt", cx);
        view.update_in(cx, |view, window, cx| {
            view.add_to_gitignore(&AddToGitignore, window, cx)
        });
        cx.run_until_parked();
        assert_eq!(
            fs.load(Path::new(path!("/project/.gitignore")))
                .await
                .expect(".gitignore exists"),
            "c.txt\n"
        );
    }
}
