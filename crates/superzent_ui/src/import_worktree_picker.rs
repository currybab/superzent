use std::path::PathBuf;
use std::sync::Arc;

use fuzzy::StringMatchCandidate;
use gpui::{
    App, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, SharedString,
    Subscription, Task, Window,
};
use picker::{Picker, PickerDelegate};
use superzent_model::{
    ProjectEntry, ProjectLocation, SuperzentStore,
};
use ui::{HighlightedLabel, ListItem, ListItemSpacing, prelude::*};
use workspace::{
    ModalView, Toast, Workspace,
    notifications::NotificationId,
};

use crate::{
    SuperzentSidebar, build_synced_local_workspace_entry,
    open_local_workspace_path_and_resolve,
};

pub fn run_import_worktree(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut gpui::Context<Workspace>,
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
                "Add a project before importing a worktree.",
            ),
            cx,
        );
        return;
    };

    if !matches!(&project.location, ProjectLocation::Local { .. }) {
        workspace.show_toast(
            Toast::new(
                NotificationId::unique::<SuperzentSidebar>(),
                "Import Worktree is only available for local projects.",
            ),
            cx,
        );
        return;
    }

    run_import_worktree_for_project(cx.entity(), project, window, cx);
}

pub fn run_import_worktree_for_project(
    workspace_handle: Entity<Workspace>,
    project: ProjectEntry,
    window: &mut Window,
    cx: &mut App,
) {
    workspace_handle.update(cx, |workspace, cx| {
        let workspace_weak = workspace.weak_handle();
        workspace.toggle_modal(window, cx, move |window, cx| {
            ImportWorktreeModal::new(workspace_weak, project, window, cx)
        });
    });
}

pub struct ImportWorktreeModal {
    picker: Entity<Picker<ImportWorktreeDelegate>>,
    _subscription: Subscription,
}

impl ImportWorktreeModal {
    fn new(
        workspace: gpui::WeakEntity<Workspace>,
        project: ProjectEntry,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> Self {
        let delegate = ImportWorktreeDelegate::new(workspace, project.clone(), window, cx);
        let picker = cx.new(|cx| Picker::uniform_list(delegate, window, cx));

        let ProjectLocation::Local { repo_root } = &project.location else {
            return Self {
                _subscription: cx.subscribe(&picker, |_, _, _, cx| cx.emit(DismissEvent)),
                picker,
            };
        };

        let repo_root = repo_root.clone();
        let store = SuperzentStore::global(cx);
        let project_id = project.id.clone();

        cx.spawn_in(window, async move |this, cx| {
            let discovered = cx
                .background_spawn({
                    let repo_root = repo_root.clone();
                    async move { superzent_git::discover_worktrees(&repo_root) }
                })
                .await;

            let discovered = match discovered {
                Ok(worktrees) => worktrees,
                Err(error) => {
                    this.update_in(cx, |this, _, cx| {
                        this.picker.update(cx, |picker, _| {
                            picker.delegate.discovery_error =
                                Some(format!("Failed to discover worktrees: {error}"));
                        });
                    })
                    .ok();
                    return anyhow::Ok(());
                }
            };

            let unimported = cx.update(|_, cx| {
                let store = store.read(cx);
                let existing_workspace_paths: std::collections::HashSet<PathBuf> = store
                    .workspaces_for_project(&project_id)
                    .into_iter()
                    .filter_map(|workspace| workspace.local_worktree_path().map(PathBuf::from))
                    .collect();

                discovered
                    .into_iter()
                    .filter(|worktree| !existing_workspace_paths.contains(&worktree.path))
                    .collect::<Vec<_>>()
            })?;

            this.update_in(cx, |this, window, cx| {
                this.picker.update(cx, |picker, cx| {
                    picker.delegate.all_worktrees = Some(unimported);
                    picker.refresh(window, cx);
                });
            })?;

            anyhow::Ok(())
        })
        .detach_and_log_err(cx);

        let subscription = cx.subscribe(&picker, |_, _, _, cx| cx.emit(DismissEvent));
        Self {
            picker,
            _subscription: subscription,
        }
    }
}

impl ModalView for ImportWorktreeModal {}
impl EventEmitter<DismissEvent> for ImportWorktreeModal {}

impl Focusable for ImportWorktreeModal {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.picker.focus_handle(cx)
    }
}

impl Render for ImportWorktreeModal {
    fn render(&mut self, _window: &mut Window, _cx: &mut gpui::Context<Self>) -> impl IntoElement {
        v_flex().w(rems(34.)).child(self.picker.clone())
    }
}

#[derive(Clone)]
struct WorktreeMatch {
    worktree: superzent_git::DiscoveredWorktree,
    positions: Vec<usize>,
}

pub struct ImportWorktreeDelegate {
    workspace: gpui::WeakEntity<Workspace>,
    project: ProjectEntry,
    all_worktrees: Option<Vec<superzent_git::DiscoveredWorktree>>,
    matches: Vec<WorktreeMatch>,
    selected_index: usize,
    discovery_error: Option<String>,
}

impl ImportWorktreeDelegate {
    fn new(
        workspace: gpui::WeakEntity<Workspace>,
        project: ProjectEntry,
        _window: &mut Window,
        _cx: &mut gpui::Context<ImportWorktreeModal>,
    ) -> Self {
        Self {
            workspace,
            project,
            all_worktrees: None,
            matches: Vec::new(),
            selected_index: 0,
            discovery_error: None,
        }
    }
}

impl PickerDelegate for ImportWorktreeDelegate {
    type ListItem = ListItem;

    fn placeholder_text(&self, _window: &mut Window, _cx: &mut App) -> Arc<str> {
        "Select worktree branch to import\u{2026}".into()
    }

    fn match_count(&self) -> usize {
        self.matches.len()
    }

    fn selected_index(&self) -> usize {
        self.selected_index
    }

    fn set_selected_index(
        &mut self,
        index: usize,
        _window: &mut Window,
        _cx: &mut gpui::Context<Picker<Self>>,
    ) {
        self.selected_index = index;
    }

    fn update_matches(
        &mut self,
        query: String,
        window: &mut Window,
        cx: &mut gpui::Context<Picker<Self>>,
    ) -> Task<()> {
        let Some(all_worktrees) = self.all_worktrees.clone() else {
            return Task::ready(());
        };

        cx.spawn_in(window, async move |picker, cx| {
            let matches: Vec<WorktreeMatch> = if query.is_empty() {
                all_worktrees
                    .into_iter()
                    .map(|worktree| WorktreeMatch {
                        worktree,
                        positions: Vec::new(),
                    })
                    .collect()
            } else {
                let candidates = all_worktrees
                    .iter()
                    .enumerate()
                    .map(|(index, worktree)| StringMatchCandidate::new(index, &worktree.branch))
                    .collect::<Vec<_>>();

                fuzzy::match_strings(
                    &candidates,
                    &query,
                    true,
                    true,
                    10000,
                    &Default::default(),
                    cx.background_executor().clone(),
                )
                .await
                .into_iter()
                .map(|candidate| WorktreeMatch {
                    worktree: all_worktrees[candidate.candidate_id].clone(),
                    positions: candidate.positions,
                })
                .collect()
            };

            picker
                .update(cx, |picker, _| {
                    picker.delegate.matches = matches;
                    if picker.delegate.matches.is_empty() {
                        picker.delegate.selected_index = 0;
                    } else {
                        picker.delegate.selected_index = picker
                            .delegate
                            .selected_index
                            .min(picker.delegate.matches.len() - 1);
                    }
                })
                .ok();
        })
    }

    fn confirm(
        &mut self,
        _secondary: bool,
        window: &mut Window,
        cx: &mut gpui::Context<Picker<Self>>,
    ) {
        let Some(selected) = self.matches.get(self.selected_index).cloned() else {
            return;
        };

        let project = self.project.clone();
        let store = SuperzentStore::global(cx);
        let workspace_handle = self.workspace.clone();

        let workspace_entry = {
            let store_ref = store.read(cx);
            build_synced_local_workspace_entry(&project, &selected.worktree, &store_ref)
        };

        let Some(workspace_entry) = workspace_entry else {
            workspace_handle
                .update(cx, |workspace, cx| {
                    workspace.show_toast(
                        Toast::new(
                            NotificationId::unique::<SuperzentSidebar>(),
                            "Failed to build workspace entry for the selected worktree.",
                        ),
                        cx,
                    );
                })
                .ok();
            cx.emit(DismissEvent);
            return;
        };

        let worktree_path = selected.worktree.path;
        let app_state = workspace_handle
            .update(cx, |workspace, _| workspace.app_state().clone())
            .ok();

        let Some(app_state) = app_state else {
            cx.emit(DismissEvent);
            return;
        };

        store.update(cx, |store, cx| {
            store.upsert_workspace(workspace_entry, cx);
        });

        let open_task = open_local_workspace_path_and_resolve(
            worktree_path.clone(),
            app_state,
            window,
            cx,
        );

        cx.spawn_in(window, async move |_, cx| {
            match open_task.await {
                Ok(_workspace) => {}
                Err(error) => {
                    workspace_handle
                        .update(cx, |workspace, cx| {
                            workspace.show_toast(
                                Toast::new(
                                    NotificationId::unique::<SuperzentSidebar>(),
                                    format!(
                                        "Failed to open worktree at {}: {error}",
                                        worktree_path.display()
                                    ),
                                ),
                                cx,
                            );
                        })
                        .ok();
                }
            }
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);

        cx.emit(DismissEvent);
    }

    fn dismissed(&mut self, _window: &mut Window, cx: &mut gpui::Context<Picker<Self>>) {
        cx.emit(DismissEvent);
    }

    fn render_match(
        &self,
        index: usize,
        selected: bool,
        _window: &mut Window,
        _cx: &mut gpui::Context<Picker<Self>>,
    ) -> Option<Self::ListItem> {
        let entry = self.matches.get(index)?;
        let path_display = entry.worktree.path.to_string_lossy().to_string();

        let branch_label = if entry.positions.is_empty() {
            Label::new(entry.worktree.branch.clone())
                .truncate()
                .into_any_element()
        } else {
            let branch = &entry.worktree.branch;
            let positions: Vec<_> = entry
                .positions
                .iter()
                .copied()
                .filter(|&pos| pos < branch.len())
                .collect();
            HighlightedLabel::new(branch.clone(), positions)
                .truncate()
                .into_any_element()
        };

        Some(
            ListItem::new(format!("import-worktree-{index}"))
                .inset(true)
                .spacing(ListItemSpacing::Sparse)
                .toggle_state(selected)
                .child(
                    v_flex()
                        .w_full()
                        .child(branch_label)
                        .child(
                            Label::new(path_display)
                                .size(ui::LabelSize::Small)
                                .color(Color::Muted)
                                .truncate()
                                .into_any_element(),
                        ),
                ),
        )
    }

    fn no_matches_text(&self, _window: &mut Window, _cx: &mut App) -> Option<SharedString> {
        if let Some(error) = &self.discovery_error {
            return Some(error.clone().into());
        }
        if self.all_worktrees.is_none() {
            return Some("Discovering worktrees\u{2026}".into());
        }
        Some("No unimported worktrees found.".into())
    }
}
