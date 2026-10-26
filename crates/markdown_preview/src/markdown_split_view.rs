use std::any::{Any, TypeId};
use std::sync::Arc;

use anyhow::{Context as _, Result};
use editor::scroll::Autoscroll;
use editor::{
    Bias, Editor, EditorEvent, MultiBuffer, MultiBufferOffset, SelectionEffects, ToOffset as _,
};
use fs::MTime;
use gpui::{
    AnyElement, App, Context, DragMoveEvent, Entity, EntityId, EventEmitter, FocusHandle,
    Focusable, Font, IntoElement, MouseButton, MouseUpEvent, Render, SharedString, Subscription,
    Task, WeakEntity, Window, relative,
};
use language::{Buffer, LanguageRegistry};
use project::project_settings::ProjectSettings;
use project::{Project, ProjectPath};
use settings::Settings as _;
use ui::prelude::*;
use workspace::item::{
    HighlightedText, Item, ItemBufferKind, ItemHandle, SaveOptions, SerializableItem,
    TabContentParams,
};
use workspace::searchable::SearchableItemHandle;
use workspace::{
    ItemId, ItemNavHistory, Pane, SaveIntent, ToolbarItemLocation, Workspace, WorkspaceId,
    delete_unloaded_items,
};

use crate::CloseAndReturnToEditor;
use crate::markdown_preview_view::{MarkdownPreviewView, SuppressedAutoPreviews};

const DEFAULT_EDITOR_RATIO: f32 = 0.5;
const MIN_EDITOR_RATIO: f32 = 0.15;
const MAX_EDITOR_RATIO: f32 = 0.85;

/// How a Markdown file is shown in its tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkdownLayout {
    /// A plain editor tab.
    Editor,
    /// The editor and the preview side by side in one tab.
    SideBySide,
    /// Only the preview, with the editor kept hidden in the same tab.
    Preview,
}

impl MarkdownLayout {
    /// The layout `item` currently shows, or `None` if it isn't a Markdown tab that can switch
    /// layouts.
    pub fn of_item(item: &dyn ItemHandle, cx: &App) -> Option<Self> {
        if let Some(split_view) = item.downcast::<MarkdownSplitView>() {
            return Some(split_view.read(cx).layout());
        }
        if let Some(editor) = item.downcast::<Editor>() {
            return MarkdownPreviewView::is_markdown_file(&editor, cx).then_some(Self::Editor);
        }
        let preview = item.downcast::<MarkdownPreviewView>()?;
        preview
            .read(cx)
            .switchable_source_editor()
            .filter(|editor| MarkdownPreviewView::is_markdown_file(editor, cx))
            .map(|_| Self::Preview)
    }
}

/// A Markdown file's editor and preview in one tab. The tab owns the editor: it is the only item
/// that shows it, so it reports and saves the editor's unsaved edits.
pub struct MarkdownSplitView {
    editor: Entity<Editor>,
    preview: Entity<MarkdownPreviewView>,
    preview_only: bool,
    editor_ratio: f32,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

#[derive(Clone, Debug)]
pub enum MarkdownSplitViewEvent {
    Editor(EditorEvent),
    LayoutChanged,
}

#[derive(Clone)]
struct DraggedDivider;

impl Render for DraggedDivider {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

impl MarkdownSplitView {
    pub fn new(
        editor: Entity<Editor>,
        preview_only: bool,
        workspace: WeakEntity<Workspace>,
        language_registry: Arc<LanguageRegistry>,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        cx.new(|cx| {
            let preview = MarkdownPreviewView::new_embedded(
                editor.clone(),
                workspace,
                language_registry,
                window,
                cx,
            );
            let focus_handle = cx.focus_handle();
            let editor_focus_handle = editor.focus_handle(cx);
            let subscriptions = vec![
                cx.subscribe(&editor, |_, _, event: &EditorEvent, cx| {
                    cx.emit(MarkdownSplitViewEvent::Editor(event.clone()));
                }),
                cx.on_focus(&focus_handle, window, |this: &mut Self, window, cx| {
                    this.focus_visible_side(window, cx);
                }),
                // Actions like go to line or outline focus the editor even while it is hidden.
                cx.on_focus(
                    &editor_focus_handle,
                    window,
                    |this: &mut Self, window, cx| {
                        if this.preview_only {
                            this.focus_visible_side(window, cx);
                        }
                    },
                ),
            ];
            Self {
                editor,
                preview,
                preview_only,
                editor_ratio: DEFAULT_EDITOR_RATIO,
                focus_handle,
                _subscriptions: subscriptions,
            }
        })
    }

    pub fn editor(&self) -> &Entity<Editor> {
        &self.editor
    }

    pub fn preview(&self) -> &Entity<MarkdownPreviewView> {
        &self.preview
    }

    pub fn layout(&self) -> MarkdownLayout {
        if self.preview_only {
            MarkdownLayout::Preview
        } else {
            MarkdownLayout::SideBySide
        }
    }

    fn set_preview_only(
        &mut self,
        preview_only: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.preview_only == preview_only {
            return;
        }
        let was_focused = self.focus_handle.contains_focused(window, cx);
        self.preview_only = preview_only;
        if was_focused {
            self.focus_visible_side(window, cx);
        }
        cx.emit(MarkdownSplitViewEvent::LayoutChanged);
        cx.notify();
    }

    fn focus_visible_side(&self, window: &mut Window, cx: &mut Context<Self>) {
        if self.preview_only {
            self.preview.focus_handle(cx).focus(window, cx);
        } else {
            self.editor.focus_handle(cx).focus(window, cx);
        }
    }

    fn resize(
        &mut self,
        event: &DragMoveEvent<DraggedDivider>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let width = event.bounds.size.width;
        if width <= px(0.) {
            return;
        }
        let ratio = (event.event.position.x - event.bounds.left()) / width;
        self.editor_ratio = ratio.clamp(MIN_EDITOR_RATIO, MAX_EDITOR_RATIO);
        cx.notify();
    }

    fn close_and_return_to_editor(
        &mut self,
        _: &CloseAndReturnToEditor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let split_view = cx.entity();
        window.defer(cx, move |window, cx| {
            let Some(workspace) = split_view.read(cx).editor.read(cx).workspace() else {
                return;
            };
            workspace.update(cx, |workspace, cx| {
                switch_layout(workspace, &split_view, MarkdownLayout::Editor, window, cx);
            });
        });
    }

    fn render_divider(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("markdown-split-view-divider")
            .relative()
            .flex_none()
            .h_full()
            .w_px()
            .bg(cx.theme().colors().border)
            .child(
                div()
                    .id("markdown-split-view-resize-handle")
                    .absolute()
                    .top_0()
                    .left(px(-3.))
                    .h_full()
                    .w(px(7.))
                    .cursor_col_resize()
                    .on_drag(DraggedDivider, |divider, _, _, cx| {
                        cx.stop_propagation();
                        cx.new(|_| divider.clone())
                    })
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseUpEvent, _, cx| {
                            if event.click_count == 2 {
                                this.editor_ratio = DEFAULT_EDITOR_RATIO;
                                cx.notify();
                            }
                        }),
                    ),
            )
    }
}

/// Shows `item` in `layout` by swapping it for a split view or a plain editor in its pane, or by
/// changing the layout of a split view in place.
pub fn switch_layout(
    workspace: &mut Workspace,
    item: &dyn ItemHandle,
    layout: MarkdownLayout,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if let Some(split_view) = item.downcast::<MarkdownSplitView>() {
        match layout {
            MarkdownLayout::Editor => unwrap_into_editor(workspace, &split_view, window, cx),
            MarkdownLayout::SideBySide | MarkdownLayout::Preview => {
                split_view.update(cx, |split_view, cx| {
                    split_view.set_preview_only(layout == MarkdownLayout::Preview, window, cx)
                });
                // The search bar asks the active item for its searchable side only when the
                // active item changes.
                if let Some(pane) = workspace.pane_for(item) {
                    pane.update(cx, |pane, cx| {
                        pane.toolbar().update(cx, |toolbar, cx| {
                            toolbar.set_active_item(Some(item), window, cx)
                        });
                    });
                }
            }
        }
    } else if let Some(editor) = item.downcast::<Editor>() {
        if layout != MarkdownLayout::Editor {
            replace_editor(
                workspace,
                &editor,
                layout == MarkdownLayout::Preview,
                window,
                cx,
            );
        }
    } else if let Some(preview) = item.downcast::<MarkdownPreviewView>() {
        switch_preview_layout(workspace, &preview, layout, window, cx);
    }
}

/// Replaces `editor`'s tab with a split view that takes over the editor, keeping the tab's
/// position, activation, focus, and temporary state.
pub fn replace_editor(
    workspace: &mut Workspace,
    editor: &Entity<Editor>,
    preview_only: bool,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Option<Entity<MarkdownSplitView>> {
    let pane = workspace.pane_for(editor)?;
    let split_view = MarkdownSplitView::new(
        editor.clone(),
        preview_only,
        workspace.weak_handle(),
        workspace.project().read(cx).languages().clone(),
        window,
        cx,
    );
    replace_item_in_pane(
        workspace,
        &pane,
        editor,
        Box::new(split_view.clone()),
        window,
        cx,
    );
    Some(split_view)
}

fn unwrap_into_editor(
    workspace: &mut Workspace,
    split_view: &Entity<MarkdownSplitView>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(pane) = workspace.pane_for(split_view) else {
        return;
    };
    let editor = split_view.read(cx).editor.clone();
    // Re-adding the editor emits `ItemAdded`; don't let the automatic preview wrap it again.
    cx.global_mut::<SuppressedAutoPreviews>()
        .0
        .insert(editor.entity_id());
    if replace_item_in_pane(
        workspace,
        &pane,
        split_view,
        Box::new(editor.clone()),
        window,
        cx,
    ) {
        // The split view can outlive its tab, and it must stop redirecting the editor's focus.
        split_view.update(cx, |split_view, _| split_view._subscriptions.clear());
    } else {
        cx.global_mut::<SuppressedAutoPreviews>()
            .0
            .remove(&editor.entity_id());
    }
}

fn switch_preview_layout(
    workspace: &mut Workspace,
    preview: &Entity<MarkdownPreviewView>,
    layout: MarkdownLayout,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    match layout {
        MarkdownLayout::Preview => {}
        MarkdownLayout::Editor => preview.update(cx, |preview, cx| {
            preview.close_and_return_to_editor(&CloseAndReturnToEditor, window, cx)
        }),
        MarkdownLayout::SideBySide => {
            let Some(editor) = preview.read(cx).switchable_source_editor() else {
                return;
            };
            match item_showing_editor(workspace, &editor, cx) {
                Some(owner) => {
                    switch_layout(workspace, owner.as_ref(), layout, window, cx);
                    if let Some(owner) = item_showing_editor(workspace, &editor, cx) {
                        workspace.activate_item(owner.as_ref(), true, true, window, cx);
                    }
                    if let Some(pane) = workspace.pane_for(preview) {
                        let preview_id = preview.entity_id();
                        pane.update(cx, |pane, cx| {
                            pane.close_item_by_id(preview_id, SaveIntent::Skip, window, cx)
                        })
                        .detach_and_log_err(cx);
                    }
                }
                None => {
                    let Some(pane) = workspace.pane_for(preview) else {
                        return;
                    };
                    let split_view = MarkdownSplitView::new(
                        editor,
                        false,
                        workspace.weak_handle(),
                        workspace.project().read(cx).languages().clone(),
                        window,
                        cx,
                    );
                    replace_item_in_pane(
                        workspace,
                        &pane,
                        preview,
                        Box::new(split_view),
                        window,
                        cx,
                    );
                }
            }
        }
    }
}

/// The editor `item` shows next to its preview, if `item` is a split view laid out side by side.
pub fn side_by_side_editor(item: &dyn ItemHandle, cx: &App) -> Option<Entity<Editor>> {
    let split_view = item.downcast::<MarkdownSplitView>()?;
    let split_view = split_view.read(cx);
    (split_view.layout() == MarkdownLayout::SideBySide).then(|| split_view.editor.clone())
}

/// The pane item that shows `editor` as its own content, either the editor's tab or a split view
/// wrapping it. Previews only read the editor, so they don't count.
pub(crate) fn item_showing_editor(
    workspace: &Workspace,
    editor: &Entity<Editor>,
    cx: &App,
) -> Option<Box<dyn ItemHandle>> {
    workspace
        .items(cx)
        .find(|item| {
            item.downcast::<MarkdownPreviewView>().is_none()
                && item.act_as::<Editor>(cx).as_ref() == Some(editor)
        })
        .map(|item| item.boxed_clone())
}

/// Swaps `old` for `new` at the same position in `pane`. Returns whether `old` was in the pane.
fn replace_item_in_pane(
    workspace: &mut Workspace,
    pane: &Entity<Pane>,
    old: &dyn ItemHandle,
    new: Box<dyn ItemHandle>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> bool {
    let pane_is_active = workspace.active_pane() == pane;
    let focus = old.item_focus_handle(cx).contains_focused(window, cx);
    let old_id = old.item_id();
    let new_id = new.item_id();
    let replaced = pane.update(cx, |pane, cx| {
        let index_for_item_id =
            |pane: &Pane, item_id| pane.items().position(|item| item.item_id() == item_id);
        let Some(index) = index_for_item_id(pane, old_id) else {
            return false;
        };
        let was_active = pane.active_item_index() == index;
        let active_item_id = pane.active_item().map(|item| item.item_id());
        let was_temporary = pane.is_active_preview_item(old_id);
        // Pinning is positional, and removing a pinned item unpins the last pinned position.
        let pinned_count = pane.pinned_count();
        // Remove first: the new item can report the same project entry, and adding it while the
        // old one is still there would just move the old one.
        pane.remove_item(old_id, false, false, window, cx);
        pane.add_item_inner(
            new,
            pane_is_active && was_active,
            focus,
            was_active,
            Some(index),
            window,
            cx,
        );
        pane.set_pinned_count(pinned_count);
        if was_active {
            if was_temporary {
                pane.replace_preview_item_id(new_id, window, cx);
            }
        } else if let Some(active_index) =
            active_item_id.and_then(|item_id| index_for_item_id(pane, item_id))
            && pane.active_item_index() != active_index
        {
            pane.activate_item(active_index, false, false, window, cx);
        }
        true
    });
    if replaced {
        // The workspace drops a removed item's dirty state only when the item is released, but
        // the old item lives on inside (or as) the new one.
        workspace.forget_item_dirty_state(old_id, window, cx);
    }
    replaced
}

impl Focusable for MarkdownSplitView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<MarkdownSplitViewEvent> for MarkdownSplitView {}

impl Render for MarkdownSplitView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .id("markdown-split-view")
            .key_context("MarkdownSplitView")
            .track_focus(&self.focus_handle)
            .size_full()
            .items_start()
            .on_action(cx.listener(Self::close_and_return_to_editor))
            .on_drag_move(cx.listener(Self::resize))
            .when(!self.preview_only, |this| {
                this.child(
                    div()
                        .flex_none()
                        .h_full()
                        .w(relative(self.editor_ratio))
                        .overflow_hidden()
                        .child(self.editor.clone()),
                )
                .child(self.render_divider(cx))
            })
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(self.preview.clone()),
            )
    }
}

impl Item for MarkdownSplitView {
    type Event = MarkdownSplitViewEvent;

    fn act_as_type<'a>(
        &'a self,
        type_id: TypeId,
        self_handle: &'a Entity<Self>,
        cx: &'a App,
    ) -> Option<gpui::AnyEntity> {
        if type_id == TypeId::of::<Self>() {
            Some(self_handle.clone().into())
        } else if type_id == TypeId::of::<Editor>() {
            Some(self.editor.clone().into())
        } else if type_id == TypeId::of::<MultiBuffer>() {
            Some(self.editor.read(cx).buffer().clone().into())
        } else {
            None
        }
    }

    fn tab_content(&self, params: TabContentParams, window: &Window, cx: &App) -> AnyElement {
        Item::tab_content(self.editor.read(cx), params, window, cx)
    }

    fn tab_content_text(&self, detail: usize, cx: &App) -> SharedString {
        Item::tab_content_text(self.editor.read(cx), detail, cx)
    }

    fn tab_icon(&self, window: &Window, cx: &App) -> Option<Icon> {
        Item::tab_icon(self.editor.read(cx), window, cx)
    }

    fn tab_tooltip_text(&self, cx: &App) -> Option<SharedString> {
        Item::tab_tooltip_text(self.editor.read(cx), cx)
    }

    fn suggested_filename(&self, cx: &App) -> SharedString {
        Item::suggested_filename(self.editor.read(cx), cx)
    }

    fn telemetry_event_text(&self) -> Option<&'static str> {
        Some("Markdown Split View Opened")
    }

    fn to_item_events(event: &Self::Event, f: &mut dyn FnMut(workspace::item::ItemEvent)) {
        match event {
            MarkdownSplitViewEvent::Editor(event) => Editor::to_item_events(event, f),
            MarkdownSplitViewEvent::LayoutChanged => {}
        }
    }

    fn deactivated(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editor
            .update(cx, |editor, cx| Item::deactivated(editor, window, cx));
    }

    fn workspace_deactivated(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |editor, cx| {
            Item::workspace_deactivated(editor, window, cx)
        });
    }

    fn pane_changed(&mut self, new_pane_id: EntityId, cx: &mut Context<Self>) {
        self.editor
            .update(cx, |editor, cx| Item::pane_changed(editor, new_pane_id, cx));
    }

    fn navigate(
        &mut self,
        data: Arc<dyn Any + Send>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.editor
            .update(cx, |editor, cx| Item::navigate(editor, data, window, cx))
    }

    fn for_each_project_item(
        &self,
        cx: &App,
        f: &mut dyn FnMut(EntityId, &dyn project::ProjectItem),
    ) {
        Item::for_each_project_item(self.editor.read(cx), cx, f)
    }

    fn buffer_kind(&self, cx: &App) -> ItemBufferKind {
        Item::buffer_kind(self.editor.read(cx), cx)
    }

    fn set_nav_history(
        &mut self,
        history: ItemNavHistory,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.editor.update(cx, |editor, cx| {
            Item::set_nav_history(editor, history, window, cx)
        });
    }

    fn can_split(&self) -> bool {
        true
    }

    fn clone_on_split(
        &self,
        workspace_id: Option<WorkspaceId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Option<Entity<Self>>> {
        let editor = self.editor.read(cx);
        let (Some(workspace), Some(project)) = (editor.workspace(), editor.project().cloned())
        else {
            return Task::ready(None);
        };
        let clone_editor = self.editor.update(cx, |editor, cx| {
            Item::clone_on_split(editor, workspace_id, window, cx)
        });
        let preview_only = self.preview_only;
        let editor_ratio = self.editor_ratio;
        cx.spawn_in(window, async move |_, cx| {
            let editor = clone_editor.await?;
            cx.update(|window, cx| {
                let split_view = Self::new(
                    editor,
                    preview_only,
                    workspace.downgrade(),
                    project.read(cx).languages().clone(),
                    window,
                    cx,
                );
                split_view.update(cx, |split_view, _| split_view.editor_ratio = editor_ratio);
                split_view
            })
            .ok()
        })
    }

    fn is_dirty(&self, cx: &App) -> bool {
        Item::is_dirty(self.editor.read(cx), cx)
    }

    fn capability(&self, cx: &App) -> language::Capability {
        Item::capability(self.editor.read(cx), cx)
    }

    fn toggle_read_only(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editor
            .update(cx, |editor, cx| Item::toggle_read_only(editor, window, cx));
        cx.notify();
    }

    fn has_deleted_file(&self, cx: &App) -> bool {
        Item::has_deleted_file(self.editor.read(cx), cx)
    }

    fn has_conflict(&self, cx: &App) -> bool {
        Item::has_conflict(self.editor.read(cx), cx)
    }

    fn can_save(&self, cx: &App) -> bool {
        Item::can_save(self.editor.read(cx), cx)
    }

    fn can_save_as(&self, cx: &App) -> bool {
        Item::can_save_as(self.editor.read(cx), cx)
    }

    fn save(
        &mut self,
        options: SaveOptions,
        project: Entity<Project>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        self.editor.update(cx, |editor, cx| {
            Item::save(editor, options, project, window, cx)
        })
    }

    fn save_as(
        &mut self,
        project: Entity<Project>,
        path: ProjectPath,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        self.editor.update(cx, |editor, cx| {
            Item::save_as(editor, project, path, window, cx)
        })
    }

    fn reload(
        &mut self,
        project: Entity<Project>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        self.editor
            .update(cx, |editor, cx| Item::reload(editor, project, window, cx))
    }

    fn as_searchable(&self, _: &Entity<Self>, _: &App) -> Option<Box<dyn SearchableItemHandle>> {
        if self.preview_only {
            Some(Box::new(self.preview.clone()))
        } else {
            Some(Box::new(self.editor.clone()))
        }
    }

    fn breadcrumb_location(&self, cx: &App) -> ToolbarItemLocation {
        Item::breadcrumb_location(self.editor.read(cx), cx)
    }

    fn breadcrumbs(&self, cx: &App) -> Option<(Vec<HighlightedText>, Option<Font>)> {
        Item::breadcrumbs(self.editor.read(cx), cx)
    }

    fn added_to_workspace(
        &mut self,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.editor.update(cx, |editor, cx| {
            Item::added_to_workspace(editor, workspace, window, cx)
        });
    }

    fn pixel_position_of_cursor(&self, cx: &App) -> Option<gpui::Point<Pixels>> {
        if self.preview_only {
            None
        } else {
            Item::pixel_position_of_cursor(self.editor.read(cx), cx)
        }
    }

    fn preserve_preview(&self, cx: &App) -> bool {
        Item::preserve_preview(self.editor.read(cx), cx)
    }
}

impl SerializableItem for MarkdownSplitView {
    fn serialized_item_kind() -> &'static str {
        "MarkdownSplitView"
    }

    fn cleanup(
        workspace_id: WorkspaceId,
        alive_items: Vec<ItemId>,
        _window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<()>> {
        let db = &*persistence::MARKDOWN_SPLIT_VIEW_DB;
        delete_unloaded_items(alive_items, workspace_id, "markdown_split_views", &db, cx)
    }

    fn deserialize(
        project: Entity<Project>,
        workspace: WeakEntity<Workspace>,
        workspace_id: WorkspaceId,
        item_id: ItemId,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Entity<Self>>> {
        let restore_unsaved_buffers = ProjectSettings::get_global(cx)
            .session
            .restore_unsaved_buffers;
        let db = &*persistence::MARKDOWN_SPLIT_VIEW_DB;
        window.spawn(cx, async move |cx| {
            let serialized = db
                .get_split_view(item_id, workspace_id)?
                .context("No markdown split view entry found")?;
            let contents = serialized.contents.filter(|_| restore_unsaved_buffers);

            let buffer = match serialized.abs_path {
                Some(abs_path) => {
                    let (worktree, path) = project
                        .update(cx, |project, cx| {
                            project.find_or_create_worktree(abs_path, false, cx)
                        })
                        .await
                        .context("Path not found")?;
                    let project_path = ProjectPath {
                        worktree_id: worktree.read_with(cx, |worktree, _| worktree.id()),
                        path,
                    };
                    let buffer = project
                        .update(cx, |project, cx| project.open_buffer(project_path, cx))
                        .await?;
                    if let Some(contents) = contents {
                        buffer.update(cx, |buffer, cx| {
                            restore_buffer_contents(buffer, contents, serialized.mtime, cx)
                        });
                    }
                    buffer
                }
                None => {
                    let contents = contents.context("No contents for an unsaved split view")?;
                    let language_registry =
                        project.read_with(cx, |project, _| project.languages().clone());
                    let language = match serialized.language {
                        Some(name) => language_registry.language_for_name(&name).await.ok(),
                        None => None,
                    };
                    let buffer = project
                        .update(cx, |project, cx| project.create_buffer(language, true, cx))
                        .await?;
                    buffer.update(cx, |buffer, cx| {
                        restore_buffer_contents(buffer, contents, None, cx)
                    });
                    buffer
                }
            };

            cx.update(|window, cx| {
                let language_registry = project.read(cx).languages().clone();
                let editor = cx.new(|cx| {
                    let mut editor = Editor::for_buffer(buffer, Some(project.clone()), window, cx);
                    restore_selections(&mut editor, &serialized.selections, window, cx);
                    editor
                });
                let split_view = Self::new(
                    editor,
                    serialized.preview_only,
                    workspace,
                    language_registry,
                    window,
                    cx,
                );
                split_view.update(cx, |split_view, _| {
                    split_view.editor_ratio = serialized
                        .editor_ratio
                        .clamp(MIN_EDITOR_RATIO, MAX_EDITOR_RATIO);
                });
                split_view
            })
        })
    }

    fn serialize(
        &mut self,
        workspace: &mut Workspace,
        item_id: ItemId,
        closing: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<()>>> {
        let restore_unsaved_buffers = ProjectSettings::get_global(cx)
            .session
            .restore_unsaved_buffers;
        // Without restoring unsaved buffers, the edits can't outlive the workspace, so let it
        // prompt for them.
        if closing && !restore_unsaved_buffers {
            return None;
        }
        let workspace_id = workspace.database_id()?;
        let project = workspace.project().read(cx);
        let buffer = self.editor.read(cx).buffer().read(cx).as_singleton()?;
        let buffer = buffer.read(cx);
        let abs_path = buffer.file().and_then(|file| {
            project
                .worktree_for_id(file.worktree_id(cx), cx)
                .map(|worktree| worktree.read(cx).absolutize(file.path()))
        });
        let save_contents = restore_unsaved_buffers && buffer.is_dirty();
        if abs_path.is_none() && !save_contents {
            return None;
        }
        let snapshot = buffer.snapshot();
        let mtime = buffer.saved_mtime();
        let preview_only = self.preview_only;
        let editor_ratio = self.editor_ratio;
        let editor = self.editor.read(cx);
        let multi_buffer_snapshot = editor.buffer().read(cx).snapshot(cx);
        let selections = editor
            .selections
            .disjoint_anchors()
            .iter()
            .map(|selection| {
                (
                    selection.start.to_offset(&multi_buffer_snapshot).0,
                    selection.end.to_offset(&multi_buffer_snapshot).0,
                )
            })
            .collect();
        Some(cx.background_spawn(async move {
            let (contents, language) = if save_contents {
                (
                    Some(snapshot.text()),
                    snapshot
                        .language()
                        .map(|language| language.name().to_string()),
                )
            } else {
                (None, None)
            };
            let serialized = persistence::SerializedSplitView {
                abs_path,
                contents,
                language,
                mtime,
                preview_only,
                editor_ratio,
                selections,
            };
            persistence::MARKDOWN_SPLIT_VIEW_DB
                .save_split_view(item_id, workspace_id, serialized)
                .await
        }))
    }

    fn should_serialize(&self, event: &Self::Event) -> bool {
        match event {
            MarkdownSplitViewEvent::LayoutChanged => true,
            MarkdownSplitViewEvent::Editor(event) => matches!(
                event,
                EditorEvent::Saved
                    | EditorEvent::DirtyChanged
                    | EditorEvent::BufferEdited
                    | EditorEvent::FileHandleChanged
                    | EditorEvent::SelectionsChanged { local: true }
            ),
        }
    }
}

fn restore_selections(
    editor: &mut Editor,
    selections: &[(usize, usize)],
    window: &mut Window,
    cx: &mut Context<Editor>,
) {
    if selections.is_empty() {
        return;
    }
    let snapshot = editor.buffer().read(cx).snapshot(cx);
    editor.change_selections(
        SelectionEffects::scroll(Autoscroll::center()),
        window,
        cx,
        |selections_collection| {
            selections_collection.select_ranges(selections.iter().map(|&(start, end)| {
                snapshot.clip_offset(MultiBufferOffset(start), Bias::Left)
                    ..snapshot.clip_offset(MultiBufferOffset(end), Bias::Right)
            }))
        },
    );
}

/// Puts unsaved contents back into a reopened buffer, keeping the saved mtime so that the buffer
/// stays dirty and later disk changes still count as conflicts. Mirrors how editors restore their
/// unsaved buffers.
fn restore_buffer_contents(
    buffer: &mut Buffer,
    contents: String,
    mtime: Option<MTime>,
    cx: &mut Context<Buffer>,
) {
    if mtime.is_some() {
        buffer.did_reload(buffer.version(), buffer.line_ending(), mtime, cx);
    }
    buffer.set_text(contents, cx);
    if let Some(entry) = buffer.peek_undo_stack() {
        buffer.forget_transaction(entry.transaction_id());
    }
}

// In tests each static connection opens its own in-memory database, so the workspace row that
// the split view table's foreign key needs has to be inserted into it directly.
#[cfg(test)]
pub(crate) async fn insert_workspace_row_for_tests(workspace_id: WorkspaceId) {
    persistence::MARKDOWN_SPLIT_VIEW_DB
        .insert_workspace(workspace_id)
        .await
        .unwrap();
}

mod persistence {
    use std::path::PathBuf;

    use anyhow::Result;
    use db::{
        query,
        sqlez::{
            bindable::{Bind, Column, StaticColumnCount},
            domain::Domain,
            statement::Statement,
            thread_safe_connection::ThreadSafeConnection,
        },
        sqlez_macros::sql,
    };
    use fs::MTime;
    use util::ResultExt as _;
    use workspace::{ItemId, WorkspaceDb, WorkspaceId};

    pub(super) struct SerializedSplitView {
        pub(super) abs_path: Option<PathBuf>,
        pub(super) contents: Option<String>,
        pub(super) language: Option<String>,
        pub(super) mtime: Option<MTime>,
        pub(super) preview_only: bool,
        pub(super) editor_ratio: f32,
        /// The editor's selections as offsets into the buffer.
        pub(super) selections: Vec<(usize, usize)>,
    }

    impl StaticColumnCount for SerializedSplitView {
        fn column_count() -> usize {
            8
        }
    }

    impl Bind for SerializedSplitView {
        fn bind(&self, statement: &Statement, start_index: i32) -> Result<i32> {
            let start_index = statement.bind(&self.abs_path, start_index)?;
            let start_index = statement.bind(&self.contents, start_index)?;
            let start_index = statement.bind(&self.language, start_index)?;
            let (mtime_seconds, mtime_nanos) = self
                .mtime
                .and_then(|mtime| mtime.to_seconds_and_nanos_for_persistence())
                .map(|(seconds, nanos)| (seconds as i64, nanos as i32))
                .unzip();
            let start_index = statement.bind(&mtime_seconds, start_index)?;
            let start_index = statement.bind(&mtime_nanos, start_index)?;
            let start_index = statement.bind(&self.preview_only, start_index)?;
            let start_index = statement.bind(&(self.editor_ratio as f64), start_index)?;
            let selections = if self.selections.is_empty() {
                None
            } else {
                Some(serde_json::to_string(&self.selections)?)
            };
            statement.bind(&selections, start_index)
        }
    }

    impl Column for SerializedSplitView {
        fn column(statement: &mut Statement, start_index: i32) -> Result<(Self, i32)> {
            let (abs_path, start_index): (Option<PathBuf>, i32) =
                Column::column(statement, start_index)?;
            let (contents, start_index): (Option<String>, i32) =
                Column::column(statement, start_index)?;
            let (language, start_index): (Option<String>, i32) =
                Column::column(statement, start_index)?;
            let (mtime_seconds, start_index): (Option<i64>, i32) =
                Column::column(statement, start_index)?;
            let (mtime_nanos, start_index): (Option<i32>, i32) =
                Column::column(statement, start_index)?;
            let (preview_only, start_index): (bool, i32) = Column::column(statement, start_index)?;
            let (editor_ratio, start_index): (f64, i32) = Column::column(statement, start_index)?;
            let (selections, start_index): (Option<String>, i32) =
                Column::column(statement, start_index)?;
            let selections = selections
                .and_then(|selections| serde_json::from_str(&selections).log_err())
                .unwrap_or_default();
            let mtime = mtime_seconds.zip(mtime_nanos).map(|(seconds, nanos)| {
                MTime::from_seconds_and_nanos(seconds as u64, nanos as u32)
            });
            Ok((
                Self {
                    abs_path,
                    contents,
                    language,
                    mtime,
                    preview_only,
                    editor_ratio: editor_ratio as f32,
                    selections,
                },
                start_index,
            ))
        }
    }

    pub struct MarkdownSplitViewDb(ThreadSafeConnection);

    impl Domain for MarkdownSplitViewDb {
        const NAME: &str = stringify!(MarkdownSplitViewDb);

        const MIGRATIONS: &[&str] = &[
            sql!(
                CREATE TABLE markdown_split_views (
                    workspace_id INTEGER,
                    item_id INTEGER,
                    abs_path BLOB,
                    contents TEXT,
                    language TEXT,
                    mtime_seconds INTEGER,
                    mtime_nanos INTEGER,
                    preview_only INTEGER NOT NULL DEFAULT 0,
                    editor_ratio REAL NOT NULL DEFAULT 0.5,

                    PRIMARY KEY(workspace_id, item_id),
                    FOREIGN KEY(workspace_id) REFERENCES workspaces(workspace_id)
                    ON DELETE CASCADE
                ) STRICT;
            ),
            sql!(
                ALTER TABLE markdown_split_views ADD COLUMN selections TEXT;
            ),
        ];
    }

    db::static_connection!(MARKDOWN_SPLIT_VIEW_DB, MarkdownSplitViewDb, [WorkspaceDb]);

    impl MarkdownSplitViewDb {
        query! {
            pub(super) async fn save_split_view(item_id: ItemId, workspace_id: WorkspaceId, split_view: SerializedSplitView) -> Result<()> {
                INSERT OR REPLACE INTO markdown_split_views(
                    item_id, workspace_id, abs_path, contents, language,
                    mtime_seconds, mtime_nanos, preview_only, editor_ratio, selections
                )
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            }
        }

        #[cfg(test)]
        query! {
            pub(super) async fn insert_workspace(workspace_id: WorkspaceId) -> Result<()> {
                INSERT OR IGNORE INTO workspaces(workspace_id) VALUES (?)
            }
        }

        query! {
            pub(super) fn get_split_view(item_id: ItemId, workspace_id: WorkspaceId) -> Result<Option<SerializedSplitView>> {
                SELECT abs_path, contents, language, mtime_seconds, mtime_nanos, preview_only, editor_ratio, selections
                FROM markdown_split_views
                WHERE item_id = ? AND workspace_id = ?
            }
        }
    }
}
