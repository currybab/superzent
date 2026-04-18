use std::path::PathBuf;
use std::sync::Arc;

use fuzzy::StringMatchCandidate;
use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    ParentElement, Render, SharedString, Styled, Task, Window,
};
use picker::{Picker, PickerDelegate};
use superzent_model::SuperzentStore;
use ui::{HighlightedLabel, ListItem, ListItemSpacing, prelude::*};
use util::ResultExt;
use workspace::item::ItemHandle;
use workspace::{ModalView, MultiWorkspace, Pane, Workspace};

use crate::TerminalView;

struct WorkspaceCandidate {
    display_name: String,
    workspace: Entity<Workspace>,
}

pub struct WorkspaceMovePicker {
    picker: Entity<Picker<WorkspaceMovePickerDelegate>>,
}

impl WorkspaceMovePicker {
    fn new(
        source_pane: Entity<Pane>,
        item_to_move: Box<dyn ItemHandle>,
        candidates: Vec<WorkspaceCandidate>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let delegate = WorkspaceMovePickerDelegate {
            source_pane,
            item_to_move,
            candidates,
            matches: Vec::new(),
            selected_index: 0,
        };
        let picker = cx.new(|cx| Picker::uniform_list(delegate, window, cx).modal(true));
        cx.subscribe(&picker, |_, _, _, cx| cx.emit(DismissEvent))
            .detach();
        Self { picker }
    }
}

impl ModalView for WorkspaceMovePicker {}
impl EventEmitter<DismissEvent> for WorkspaceMovePicker {}

impl Focusable for WorkspaceMovePicker {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.picker.focus_handle(cx)
    }
}

impl Render for WorkspaceMovePicker {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        v_flex().w(rems(34.)).child(self.picker.clone())
    }
}

struct WorkspaceMovePickerDelegate {
    source_pane: Entity<Pane>,
    item_to_move: Box<dyn ItemHandle>,
    candidates: Vec<WorkspaceCandidate>,
    matches: Vec<fuzzy::StringMatch>,
    selected_index: usize,
}

impl PickerDelegate for WorkspaceMovePickerDelegate {
    type ListItem = ListItem;

    fn match_count(&self) -> usize {
        self.matches.len()
    }

    fn selected_index(&self) -> usize {
        self.selected_index
    }

    fn set_selected_index(
        &mut self,
        ix: usize,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) {
        self.selected_index = ix;
    }

    fn placeholder_text(&self, _window: &mut Window, _cx: &mut App) -> Arc<str> {
        "Select target workspace...".into()
    }

    fn update_matches(
        &mut self,
        query: String,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        let candidates: Vec<StringMatchCandidate> = self
            .candidates
            .iter()
            .enumerate()
            .map(|(ix, candidate)| StringMatchCandidate::new(ix, &candidate.display_name))
            .collect();

        if query.is_empty() {
            self.matches = candidates
                .into_iter()
                .enumerate()
                .map(|(ix, candidate)| fuzzy::StringMatch {
                    candidate_id: ix,
                    score: 0.0,
                    positions: Vec::new(),
                    string: candidate.string,
                })
                .collect();
            self.selected_index = 0;
            cx.notify();
            return Task::ready(());
        }

        let executor = cx.background_executor().clone();
        cx.spawn_in(window, async move |picker, cx| {
            let matches = fuzzy::match_strings(
                &candidates,
                &query,
                true,
                false,
                100,
                &std::sync::atomic::AtomicBool::default(),
                executor,
            )
            .await;

            picker
                .update_in(cx, |picker, _window, cx| {
                    let delegate = &mut picker.delegate;
                    delegate.matches = matches;
                    if delegate.selected_index >= delegate.matches.len() {
                        delegate.selected_index = delegate.matches.len().saturating_sub(1);
                    }
                    cx.notify();
                })
                .log_err();
        })
    }

    fn confirm(&mut self, _secondary: bool, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        let Some(string_match) = self.matches.get(self.selected_index) else {
            return;
        };
        let Some(candidate) = self.candidates.get(string_match.candidate_id) else {
            return;
        };
        let target_workspace = candidate.workspace.clone();

        let item = self.item_to_move.boxed_clone();
        let item_id = item.item_id();

        self.source_pane.update(cx, |pane, cx| {
            pane.remove_item(item_id, false, false, window, cx);
        });

        target_workspace.update(cx, |workspace, cx| {
            let target_pane = workspace.active_pane().clone();
            target_pane.update(cx, |pane, cx| {
                pane.add_item(item, true, true, None, window, cx);
            });
        });

        if let Some(Some(multi_workspace)) = window.root::<MultiWorkspace>() {
            multi_workspace.update(cx, |multi_workspace, cx| {
                multi_workspace.activate(target_workspace.clone(), cx);
            });
        }

        cx.emit(DismissEvent);
    }

    fn dismissed(&mut self, _window: &mut Window, cx: &mut Context<Picker<Self>>) {
        cx.emit(DismissEvent);
    }

    fn render_match(
        &self,
        ix: usize,
        selected: bool,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) -> Option<Self::ListItem> {
        let string_match = self.matches.get(ix)?;

        Some(
            ListItem::new(SharedString::from(format!("workspace-move-{ix}")))
                .inset(true)
                .spacing(ListItemSpacing::Sparse)
                .toggle_state(selected)
                .child(
                    HighlightedLabel::new(
                        string_match.string.clone(),
                        string_match.positions.clone(),
                    )
                    .single_line()
                    .truncate(),
                ),
        )
    }
}

pub fn move_terminal_to_workspace(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let active_pane = workspace.active_pane().clone();
    let Some(active_item) = active_pane.read(cx).active_item() else {
        return;
    };

    if active_item.downcast::<TerminalView>().is_none() {
        return;
    }

    let Some(Some(multi_workspace)) = window.root::<MultiWorkspace>() else {
        return;
    };

    let raw_entries = multi_workspace
        .read(cx)
        .workspace_entries_excluding_active(cx);

    if raw_entries.is_empty() {
        workspace.show_toast(
            workspace::Toast::new(
                workspace::notifications::NotificationId::unique::<WorkspaceMovePicker>(),
                "Open another workspace first to move this terminal.",
            ),
            cx,
        );
        return;
    }

    let candidates = build_workspace_candidates(&raw_entries, cx);
    let item_to_move = active_item.boxed_clone();

    workspace.toggle_modal(window, cx, |window, cx| {
        WorkspaceMovePicker::new(active_pane, item_to_move, candidates, window, cx)
    });
}

fn build_workspace_candidates(
    entries: &[(usize, PathBuf, Entity<Workspace>)],
    cx: &App,
) -> Vec<WorkspaceCandidate> {
    let store = SuperzentStore::global(cx);
    let store = store.read(cx);

    entries
        .iter()
        .map(|(index, worktree_path, workspace_entity)| {
            let display_name =
                if let Some(workspace_entry) = store.workspace_for_path(worktree_path) {
                    let project_name = store
                        .project(&workspace_entry.project_id)
                        .map(|project| project.name.as_str())
                        .unwrap_or("Unknown");

                    let workspace_label =
                        if workspace_entry.kind == superzent_model::WorkspaceKind::Primary {
                            "local".to_string()
                        } else if let Some(display) = workspace_entry
                            .display_name
                            .as_deref()
                            .filter(|name| !name.trim().is_empty())
                        {
                            display.to_string()
                        } else {
                            workspace_entry.branch.clone()
                        };

                    format!("[{project_name}] {workspace_label}")
                } else {
                    worktree_path
                        .file_name()
                        .map(|name| name.to_string_lossy().to_string())
                        .unwrap_or_else(|| format!("Workspace {}", index + 1))
                };

            WorkspaceCandidate {
                display_name,
                workspace: workspace_entity.clone(),
            }
        })
        .collect()
}
