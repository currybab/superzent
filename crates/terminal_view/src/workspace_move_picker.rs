use fuzzy::StringMatchCandidate;
use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    ParentElement, Render, SharedString, Styled, Task, Window,
};
use picker::{Picker, PickerDelegate};
use std::sync::Arc;
use ui::{HighlightedLabel, ListItem, ListItemSpacing, prelude::*};
use util::ResultExt;
use workspace::{ModalView, MultiWorkspace, Pane, Workspace};
use workspace::item::ItemHandle;

use crate::TerminalView;

pub struct WorkspaceMovePicker {
    picker: Entity<Picker<WorkspaceMovePickerDelegate>>,
}

impl WorkspaceMovePicker {
    fn new(
        source_pane: Entity<Pane>,
        item_to_move: Box<dyn ItemHandle>,
        workspace_entries: Vec<(usize, String, Entity<Workspace>)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let delegate = WorkspaceMovePickerDelegate {
            source_pane,
            item_to_move,
            workspace_entries,
            matches: Vec::new(),
            selected_index: 0,
        };
        let picker = cx.new(|cx| Picker::uniform_list(delegate, window, cx).modal(true));
        cx.subscribe(&picker, |_, _, _, cx| cx.emit(DismissEvent)).detach();
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
    workspace_entries: Vec<(usize, String, Entity<Workspace>)>,
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
            .workspace_entries
            .iter()
            .enumerate()
            .map(|(ix, (_, name, _))| StringMatchCandidate::new(ix, name))
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

            picker.update_in(cx, |picker, _window, cx| {
                let delegate = &mut picker.delegate;
                delegate.matches = matches;
                if delegate.selected_index >= delegate.matches.len() {
                    delegate.selected_index = delegate.matches.len().saturating_sub(1);
                }
                cx.notify();
            }).log_err();
        })
    }

    fn confirm(&mut self, _secondary: bool, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        let Some(string_match) = self.matches.get(self.selected_index) else {
            return;
        };
        let Some((target_index, _, target_workspace)) =
            self.workspace_entries.get(string_match.candidate_id).cloned()
        else {
            return;
        };

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
                multi_workspace.activate_index(target_index, window, cx);
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

    let workspace_entries = multi_workspace.read(cx).workspace_entries_excluding_active(cx);

    if workspace_entries.is_empty() {
        workspace.show_toast(
            workspace::Toast::new(
                workspace::notifications::NotificationId::unique::<WorkspaceMovePicker>(),
                "Open another workspace first to move this terminal.",
            ),
            cx,
        );
        return;
    }

    let item_to_move = active_item.boxed_clone();

    workspace.toggle_modal(window, cx, |window, cx| {
        WorkspaceMovePicker::new(active_pane, item_to_move, workspace_entries, window, cx)
    });
}
