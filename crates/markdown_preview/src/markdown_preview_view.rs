use std::any::TypeId;
use std::borrow::Cow;
use std::cmp::min;
use std::collections::HashSet;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use editor::scroll::Autoscroll;
use editor::{
    Editor, EditorEvent, EditorSettings, MAX_TAB_TITLE_LEN, MultiBufferOffset, SelectionEffects,
};
use gpui::{
    Action, App, ClipboardItem, Context, Entity, EntityId, EventEmitter, FocusHandle, Focusable,
    Font, Global, ImageSource, InteractiveElement, IntoElement, IsZero, Pixels, Render, Resource,
    RetainAllImageCache, ScrollHandle, SharedString, SharedUri, Subscription, Task, WeakEntity,
    Window, point, px,
};
use language::{Buffer, LanguageRegistry, Point};
use markdown::{
    CodeBlockRenderer, CopyButtonVisibility, Markdown, MarkdownElement, MarkdownFont,
    MarkdownOptions, MarkdownStyle,
};
use project::search::SearchQuery;
use project::{Project, ProjectPath};
use settings::{Settings, update_settings_file};
use theme::ThemeSettings;
use theme::{SystemAppearance, Theme, ThemeRegistry};
use ui::utils::WithRemSize;
use ui::{
    ContextMenu, LinkPreview, ScrollAxes, Scrollbars, WithScrollbar, prelude::*, right_click_menu,
};
use util::{
    ResultExt,
    markdown::{source_position_from_fragment, split_local_url_fragment},
};
use workspace::item::{
    HighlightedText, Item, ItemBufferKind, ItemHandle, SaveOptions, SerializableItem,
    TabContentParams,
};
use workspace::notifications::NotifyResultExt;
use workspace::searchable::{
    Direction, SearchEvent, SearchOptions, SearchToken, SearchableItem, SearchableItemHandle,
};
use workspace::{
    ItemId, ItemNavHistory, Pane, SaveIntent, ToolbarItemLocation, Workspace, WorkspaceId,
    delete_unloaded_items,
};
use zed_actions::{DecreaseBufferFontSize, IncreaseBufferFontSize, ResetBufferFontSize};

use crate::markdown_preview_settings::MarkdownPreviewSettings;
use crate::markdown_split_view::{self, MarkdownLayout, MarkdownSplitView};
use crate::{
    CloseAndReturnToEditor, OpenFollowingPreview, OpenPreview, OpenPreviewToTheSide, ScrollDown,
    ScrollDownByItem,
};
use crate::{ScrollPageDown, ScrollPageUp, ScrollToBottom, ScrollToTop, ScrollUp, ScrollUpByItem};

const REPARSE_DEBOUNCE: Duration = Duration::from_millis(200);

fn reset_persisted_font_size(settings: &mut settings::SettingsContent) {
    if let Some(markdown_preview) = settings.markdown_preview.as_mut() {
        markdown_preview.font_size = None;
    }
}

pub struct MarkdownPreviewView {
    workspace: WeakEntity<Workspace>,
    active_editor: Option<EditorState>,
    focus_handle: FocusHandle,
    markdown: Entity<Markdown>,
    _markdown_subscription: Subscription,
    active_source_index: Option<usize>,
    scroll_handle: ScrollHandle,
    image_cache: Entity<RetainAllImageCache>,
    base_directory: Option<PathBuf>,
    pending_update_task: Option<Task<Result<()>>>,
    hovered_url: Option<SharedString>,
    mode: MarkdownPreviewMode,
    /// Search results depend on the parsed markdown, which lags behind the source while a
    /// background parse is in flight. Tracked so matches can be invalidated once it lands.
    markdown_parse_pending: bool,
    nav_history: Option<ItemNavHistory>,
    /// Whether the preview is part of a [`MarkdownSplitView`], which owns the source editor.
    embedded: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MarkdownPreviewMode {
    /// The preview will always show the contents of the provided editor.
    Default,
    /// The preview will "follow" the currently active editor.
    Follow,
}

impl MarkdownPreviewMode {
    fn to_db(self) -> i64 {
        match self {
            Self::Default => 0,
            Self::Follow => 1,
        }
    }

    fn from_db(value: i64) -> Self {
        match value {
            1 => Self::Follow,
            _ => Self::Default,
        }
    }
}

struct EditorState {
    editor: Entity<Editor>,
    _subscription: Subscription,
}

/// Editors whose next `ItemAdded` must not open an automatic preview, because they are being given
/// a tab of their own on purpose.
#[derive(Default)]
pub(crate) struct SuppressedAutoPreviews(pub(crate) HashSet<EntityId>);

impl Global for SuppressedAutoPreviews {}

#[derive(Clone, Copy, Debug)]
pub enum MarkdownPreviewEvent {
    SourceEditorChanged,
    SourceFileHandleChanged,
    SourceDirtyChanged,
    SourceSelectionChanged,
}

impl MarkdownPreviewView {
    pub fn register(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
        cx.default_global::<SuppressedAutoPreviews>();
        let workspace_entity = cx.entity();
        cx.subscribe_in(
            &workspace_entity,
            window,
            |workspace, _, event: &workspace::Event, window, cx| {
                let workspace::Event::ItemAdded { item } = event else {
                    return;
                };
                if cx
                    .global_mut::<SuppressedAutoPreviews>()
                    .0
                    .remove(&item.item_id())
                {
                    return;
                }
                let Some(layout) =
                    MarkdownPreviewSettings::get_global(cx).layout_for_opened_files()
                else {
                    return;
                };
                if workspace.is_restoring() {
                    return;
                }
                let Some(editor) = item.downcast::<Editor>() else {
                    return;
                };
                if !Self::is_markdown_editor(workspace, &editor, cx) {
                    return;
                }
                markdown_split_view::switch_layout(workspace, item.as_ref(), layout, window, cx);
            },
        )
        .detach();

        workspace.register_action(move |workspace, _: &OpenPreview, window, cx| {
            if let Some(editor) = Self::resolve_active_item_as_markdown_editor(workspace, cx) {
                let pane = workspace.active_pane().clone();
                Self::open_preview_in_pane(workspace, editor, pane, window, cx);
            }
        });

        workspace.register_action(move |workspace, _: &OpenPreviewToTheSide, window, cx| {
            if let Some(editor) = Self::resolve_active_item_as_markdown_editor(workspace, cx) {
                let pane = workspace.active_pane().clone();
                Self::open_preview_to_the_side_of_pane(workspace, editor, pane, window, cx);
            }
        });

        workspace.register_action(move |workspace, _: &OpenFollowingPreview, window, cx| {
            if let Some(editor) = Self::resolve_active_item_as_markdown_editor(workspace, cx) {
                // Check if there's already a following preview
                let existing_follow_view_idx = {
                    let active_pane = workspace.active_pane().read(cx);
                    active_pane
                        .items_of_type::<MarkdownPreviewView>()
                        .find(|view| view.read(cx).mode == MarkdownPreviewMode::Follow)
                        .and_then(|view| active_pane.index_for_item(&view))
                };

                if let Some(existing_follow_view_idx) = existing_follow_view_idx {
                    workspace.active_pane().update(cx, |pane, cx| {
                        pane.activate_item(existing_follow_view_idx, true, true, window, cx);
                    });
                } else {
                    let view = Self::create_following_markdown_view(workspace, editor, window, cx);
                    workspace.active_pane().update(cx, |pane, cx| {
                        pane.add_item(Box::new(view.clone()), true, true, None, window, cx)
                    });
                }
                cx.notify();
            }
        });
    }

    pub fn open_preview_in_pane(
        workspace: &mut Workspace,
        editor: Entity<Editor>,
        pane: Entity<Pane>,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        Self::activate_or_add_preview(workspace, editor, pane, true, true, window, cx);
    }

    pub fn open_preview_to_the_side_of_pane(
        workspace: &mut Workspace,
        editor: Entity<Editor>,
        origin_pane: Entity<Pane>,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let target_pane = workspace.adjacent_pane_of(&origin_pane, window, cx);
        Self::activate_or_add_preview(
            workspace,
            editor.clone(),
            target_pane,
            false,
            false,
            window,
            cx,
        );
        editor.focus_handle(cx).focus(window, cx);
    }

    fn activate_or_add_preview(
        workspace: &mut Workspace,
        editor: Entity<Editor>,
        pane: Entity<Pane>,
        activate: bool,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Entity<MarkdownPreviewView> {
        let existing_view =
            Self::find_existing_independent_preview_item_idx(pane.read(cx), &editor, cx);
        let view = if let Some((existing_view, existing_view_idx)) = existing_view {
            pane.update(cx, |pane, cx| {
                pane.activate_item(existing_view_idx, activate, focus, window, cx);
            });
            existing_view
        } else {
            let view = Self::create_markdown_view(workspace, editor, window, cx);
            pane.update(cx, |pane, cx| {
                pane.add_item(Box::new(view.clone()), activate, focus, None, window, cx)
            });
            view
        };
        cx.notify();
        view
    }

    fn find_existing_independent_preview_item_idx(
        pane: &Pane,
        editor: &Entity<Editor>,
        cx: &App,
    ) -> Option<(Entity<MarkdownPreviewView>, usize)> {
        let target_buffer = editor.read(cx).buffer().read(cx).as_singleton()?;
        let view = pane.items_of_type::<MarkdownPreviewView>().find(|view| {
            // Only look for independent (Default mode) previews, not Follow previews.
            // Match by buffer entity rather than editor entity so the lookup survives
            // workspace restoration, where the preview's bound editor may differ from
            // the editor the user is currently invoking the action on even though both
            // wrap the same source buffer.
            view.read(cx).mode == MarkdownPreviewMode::Default
                && view.read(cx).is_previewing(&target_buffer, cx)
        })?;
        let index = pane.index_for_item(&view)?;
        Some((view, index))
    }

    /// The source editor of a standalone preview bound to one file, which the layout toggle can
    /// switch to.
    pub(crate) fn switchable_source_editor(&self) -> Option<Entity<Editor>> {
        if self.embedded || self.mode != MarkdownPreviewMode::Default {
            return None;
        }
        self.active_editor
            .as_ref()
            .map(|editor_state| editor_state.editor.clone())
    }

    fn is_previewing(&self, buffer: &Entity<Buffer>, cx: &App) -> bool {
        self.active_editor.as_ref().is_some_and(|state| {
            state
                .editor
                .read(cx)
                .buffer()
                .read(cx)
                .as_singleton()
                .as_ref()
                == Some(buffer)
        })
    }

    pub fn resolve_active_item_as_markdown_editor(
        workspace: &Workspace,
        cx: &mut Context<Workspace>,
    ) -> Option<Entity<Editor>> {
        if let Some(editor) = workspace
            .active_item(cx)
            .and_then(|item| item.act_as::<Editor>(cx))
            && Self::is_markdown_file(&editor, cx)
        {
            return Some(editor);
        }
        None
    }

    pub fn create_markdown_view(
        workspace: &mut Workspace,
        editor: Entity<Editor>,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Entity<MarkdownPreviewView> {
        let language_registry = workspace.project().read(cx).languages().clone();
        let workspace_handle = workspace.weak_handle();
        MarkdownPreviewView::new(
            MarkdownPreviewMode::Default,
            editor,
            workspace_handle,
            language_registry,
            window,
            cx,
        )
    }

    fn create_following_markdown_view(
        workspace: &mut Workspace,
        editor: Entity<Editor>,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Entity<MarkdownPreviewView> {
        let language_registry = workspace.project().read(cx).languages().clone();
        let workspace_handle = workspace.weak_handle();
        MarkdownPreviewView::new(
            MarkdownPreviewMode::Follow,
            editor,
            workspace_handle,
            language_registry,
            window,
            cx,
        )
    }

    pub fn new(
        mode: MarkdownPreviewMode,
        active_editor: Entity<Editor>,
        workspace: WeakEntity<Workspace>,
        language_registry: Arc<LanguageRegistry>,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        Self::build(
            mode,
            false,
            active_editor,
            workspace,
            language_registry,
            window,
            cx,
        )
    }

    /// A preview for a [`MarkdownSplitView`]. It stays bound to the split view's editor and leaves
    /// tab-level actions to the split view.
    pub(crate) fn new_embedded(
        editor: Entity<Editor>,
        workspace: WeakEntity<Workspace>,
        language_registry: Arc<LanguageRegistry>,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        Self::build(
            MarkdownPreviewMode::Default,
            true,
            editor,
            workspace,
            language_registry,
            window,
            cx,
        )
    }

    fn build(
        mode: MarkdownPreviewMode,
        embedded: bool,
        active_editor: Entity<Editor>,
        workspace: WeakEntity<Workspace>,
        language_registry: Arc<LanguageRegistry>,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        cx.new(|cx| {
            let markdown = cx.new(|cx| {
                Markdown::new_with_options(
                    SharedString::default(),
                    Some(language_registry),
                    None,
                    MarkdownOptions {
                        parse_html: true,
                        render_mermaid_diagrams: true,
                        parse_heading_slugs: true,
                        render_metadata_blocks: true,
                        ..Default::default()
                    },
                    cx,
                )
            });
            let mut this = Self {
                active_editor: None,
                focus_handle: cx.focus_handle(),
                workspace: workspace.clone(),
                _markdown_subscription: cx.observe(
                    &markdown,
                    |this: &mut Self, markdown: Entity<Markdown>, cx| {
                        this.sync_active_root_block(cx);
                        let is_parsing = markdown.read(cx).is_parsing();
                        if this.markdown_parse_pending && !is_parsing {
                            cx.emit(SearchEvent::MatchesInvalidated);
                        }
                        this.markdown_parse_pending = is_parsing;
                    },
                ),
                markdown,
                active_source_index: None,
                scroll_handle: ScrollHandle::new(),
                image_cache: RetainAllImageCache::new(cx),
                base_directory: None,
                embedded,
                pending_update_task: None,
                hovered_url: None,
                mode,
                markdown_parse_pending: false,
                nav_history: None,
            };

            this.set_editor(active_editor, window, cx);

            match mode {
                MarkdownPreviewMode::Follow => {
                    if let Some(workspace) = &workspace.upgrade() {
                        cx.observe_in(workspace, window, |this, workspace, window, cx| {
                            let item = workspace.read(cx).active_item(cx);
                            this.workspace_updated(item, window, cx);
                        })
                        .detach();
                    } else {
                        log::error!("Failed to listen to workspace updates");
                    }
                }
                MarkdownPreviewMode::Default if embedded => {}
                MarkdownPreviewMode::Default => {
                    // After workspace restoration the bound editor may be an orphan that
                    // wraps the right buffer but isn't the canonical Editor instance in
                    // any pane. Re-binding to the workspace's editor for our buffer is
                    // what restores cursor-driven scroll sync — `SelectionsChanged` only
                    // fires from the editor the user actually interacts with.
                    //
                    // Subscribing to `workspace::Event` (rather than `observe`) keeps the
                    // rebind check off the cursor-move hot path; `observe` would fire on
                    // every workspace `cx.notify`.
                    if let Some(workspace) = &workspace.upgrade() {
                        cx.subscribe_in(workspace, window, Self::on_workspace_event)
                            .detach();
                    }
                }
            }

            this
        })
    }

    fn workspace_updated(
        &mut self,
        active_item: Option<Box<dyn ItemHandle>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(item) = active_item
            && item.item_id() != cx.entity_id()
            && let Some(editor) = item.act_as::<Editor>(cx)
            && Self::is_markdown_file(&editor, cx)
        {
            self.set_editor(editor, window, cx);
        }
    }

    pub fn is_markdown_path(path: impl AsRef<Path>) -> bool {
        path.as_ref().extension().is_some_and(|ext| {
            ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown")
        })
    }

    /// Opens the file as a preview-only tab. The tab owns the file's editor, so an existing tab for
    /// the file switches to the preview instead of gaining a second owner.
    pub fn open_for_project_path(
        project_path: ProjectPath,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let open_path = workspace.open_path(project_path, None, true, window, cx);
        cx.spawn_in(window, async move |workspace, cx| {
            let Some(item) = open_path
                .await
                .notify_workspace_async_err(workspace.clone(), cx)
            else {
                return;
            };
            workspace
                .update_in(cx, |workspace, window, cx| {
                    // Opening a new editor can already have wrapped it in a split view.
                    let item = item
                        .act_as::<Editor>(cx)
                        .and_then(|editor| {
                            markdown_split_view::item_showing_editor(workspace, &editor, cx)
                        })
                        .unwrap_or(item);
                    markdown_split_view::switch_layout(
                        workspace,
                        item.as_ref(),
                        MarkdownLayout::Preview,
                        window,
                        cx,
                    );
                })
                .ok();
        })
        .detach();
    }

    pub fn is_markdown_file(editor: &Entity<Editor>, cx: &App) -> bool {
        let buffer = editor.read(cx).buffer().read(cx);
        if let Some(buffer) = buffer.as_singleton()
            && let Some(language) = buffer.read(cx).language()
        {
            return language.name() == "Markdown";
        }
        false
    }

    fn is_markdown_editor(workspace: &Workspace, editor: &Entity<Editor>, cx: &App) -> bool {
        let Some(buffer) = editor.read(cx).buffer().read(cx).as_singleton() else {
            return false;
        };
        let buffer = buffer.read(cx);
        if let Some(language) = buffer.language() {
            return language.name() == "Markdown";
        }
        let Some(file) = buffer.file() else {
            return false;
        };
        let languages = workspace.project().read(cx).languages().clone();
        let Some(markdown) = languages.available_language_for_name("Markdown") else {
            return false;
        };
        languages
            .language_for_file(file, None, cx)
            .is_some_and(|language| language.name() == markdown.name())
    }

    fn set_editor(&mut self, editor: Entity<Editor>, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(active) = &self.active_editor
            && active.editor == editor
        {
            return;
        }

        let had_active_editor = self.active_editor.is_some();
        let subscription = cx.subscribe_in(
            &editor,
            window,
            |this, editor, event: &EditorEvent, window, cx| {
                match event {
                    EditorEvent::Edited { .. }
                    | EditorEvent::BufferEdited { .. }
                    | EditorEvent::ExcerptsEdited { .. } => {
                        this.update_markdown_from_active_editor(true, false, window, cx);
                    }
                    EditorEvent::DirtyChanged => {
                        this.update_markdown_from_active_editor(true, false, window, cx);
                        cx.emit(MarkdownPreviewEvent::SourceDirtyChanged);
                    }
                    EditorEvent::Saved => {
                        cx.emit(MarkdownPreviewEvent::SourceDirtyChanged);
                    }
                    EditorEvent::FileHandleChanged => {
                        this.base_directory =
                            Self::get_folder_for_active_editor(editor.read(cx), cx);
                        this.update_markdown_from_active_editor(false, false, window, cx);
                        cx.emit(MarkdownPreviewEvent::SourceFileHandleChanged);
                    }
                    EditorEvent::SelectionsChanged { .. } => {
                        let (selection_start, editor_is_focused) =
                            editor.update(cx, |editor, cx| {
                                let index = Self::selected_source_index(editor, cx);
                                let focused = editor.focus_handle(cx).is_focused(window);
                                (index, focused)
                            });
                        if let Some(selection_start) = selection_start {
                            let reveal = editor_is_focused || this.focus_handle.is_focused(window);
                            this.sync_preview_to_source_index(selection_start, reveal, cx);
                            cx.notify();
                        }
                        cx.emit(MarkdownPreviewEvent::SourceSelectionChanged);
                    }
                    _ => {}
                };
            },
        );

        self.base_directory = Self::get_folder_for_active_editor(editor.read(cx), cx);
        self.hovered_url = None;
        self.active_editor = Some(EditorState {
            editor,
            _subscription: subscription,
        });
        self.update_markdown_from_active_editor(false, true, window, cx);
        if had_active_editor {
            cx.emit(MarkdownPreviewEvent::SourceEditorChanged);
        }
    }

    fn on_workspace_event(
        &mut self,
        workspace: &Entity<Workspace>,
        event: &workspace::Event,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(
            event,
            workspace::Event::ItemAdded { .. } | workspace::Event::ItemRemoved { .. }
        ) {
            return;
        }
        let candidate = self.find_canonical_editor(workspace.read(cx), cx);
        if let Some(editor) = candidate
            && self
                .active_editor
                .as_ref()
                .is_none_or(|s| s.editor != editor)
        {
            self.set_editor(editor, window, cx);
        }
    }

    fn find_canonical_editor(&self, workspace: &Workspace, cx: &App) -> Option<Entity<Editor>> {
        let current = self.active_editor.as_ref()?.editor.clone();
        let our_buffer = current.read(cx).buffer().read(cx).as_singleton()?;
        let mut fallback = None;
        let editors = workspace
            .items(cx)
            .filter(|item| item.downcast::<MarkdownPreviewView>().is_none())
            .filter_map(|item| item.act_as::<Editor>(cx));
        for editor in editors {
            if editor.read(cx).buffer().read(cx).as_singleton().as_ref() != Some(&our_buffer) {
                continue;
            }
            if editor == current {
                return Some(current);
            }
            fallback.get_or_insert(editor);
        }
        fallback
    }

    fn update_markdown_from_active_editor(
        &mut self,
        wait_for_debounce: bool,
        should_reveal: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(state) = &self.active_editor {
            // if there is already a task to update the ui and the current task is also debounced (not high priority), do nothing
            if wait_for_debounce && self.pending_update_task.is_some() {
                return;
            }
            self.pending_update_task = Some(self.schedule_markdown_update(
                wait_for_debounce,
                should_reveal,
                state.editor.clone(),
                window,
                cx,
            ));
        }
    }

    fn schedule_markdown_update(
        &mut self,
        wait_for_debounce: bool,
        should_reveal_selection: bool,
        editor: Entity<Editor>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        cx.spawn_in(window, async move |view, cx| {
            if wait_for_debounce {
                // Wait for the user to stop typing
                cx.background_executor().timer(REPARSE_DEBOUNCE).await;
            }

            let update = view.update(cx, |view, cx| {
                let is_active_editor = view
                    .active_editor
                    .as_ref()
                    .is_some_and(|active_editor| active_editor.editor == editor);
                if !is_active_editor {
                    return None;
                }

                editor.update(cx, |editor, cx| {
                    let contents = editor
                        .buffer()
                        .read(cx)
                        .as_singleton()?
                        .read(cx)
                        .as_rope()
                        .to_string()
                        .into();
                    let selection_start = Self::selected_source_index(editor, cx)?;
                    Some((contents, selection_start))
                })
            })?;

            view.update(cx, move |view, cx| {
                if let Some((contents, selection_start)) = update {
                    view.hovered_url = None;
                    view.markdown.update(cx, |markdown, cx| {
                        markdown.reset(contents, cx);
                    });
                    view.markdown_parse_pending = view.markdown.read(cx).is_parsing();
                    view.sync_preview_to_source_index(selection_start, should_reveal_selection, cx);
                    cx.emit(SearchEvent::MatchesInvalidated);
                }
                view.pending_update_task = None;
                cx.notify();
            })
        })
    }

    fn selected_source_index(editor: &Editor, cx: &mut App) -> Option<usize> {
        let display_snapshot = editor.display_snapshot(cx);
        let source_offset = editor
            .selections
            .last::<MultiBufferOffset>(&display_snapshot)
            .range()
            .start;
        let buffer = editor.buffer().read(cx).as_singleton()?;
        let buffer_id = buffer.read(cx).remote_id();
        let (buffer_snapshot, buffer_offset) = display_snapshot
            .buffer_snapshot()
            .point_to_buffer_offset(source_offset)?;

        if buffer_snapshot.remote_id() == buffer_id {
            Some(buffer_offset.0)
        } else {
            None
        }
    }

    /// Source indices come from the singleton buffer's text, which the preview parses. The editor's
    /// multibuffer can also show deleted diff hunks, so its offsets don't line up with them.
    fn source_range_to_anchors(
        editor: &Editor,
        source_range: Range<usize>,
        cx: &App,
    ) -> Option<Range<editor::Anchor>> {
        let multibuffer = editor.buffer().read(cx);
        let buffer = multibuffer.as_singleton()?.read(cx);
        let snapshot = multibuffer.snapshot(cx);
        let start = snapshot.as_singleton_anchor(buffer.anchor_before(source_range.start))?;
        let end = snapshot.as_singleton_anchor(buffer.anchor_after(source_range.end))?;
        Some(start..end)
    }

    fn sync_preview_to_source_index(
        &mut self,
        source_index: usize,
        reveal: bool,
        cx: &mut Context<Self>,
    ) {
        self.active_source_index = Some(source_index);
        self.sync_active_root_block(cx);
        self.markdown.update(cx, |markdown, cx| {
            if reveal {
                markdown.request_autoscroll_to_source_index(source_index, cx);
            }
        });
    }

    fn sync_active_root_block(&mut self, cx: &mut Context<Self>) {
        self.markdown.update(cx, |markdown, cx| {
            markdown.set_active_root_for_source_index(self.active_source_index, cx);
        });
    }

    fn change_selection_to_source_index(
        editor: &Entity<Editor>,
        source_index: usize,
        move_focus: bool,
        window: &mut Window,
        cx: &mut App,
    ) {
        editor.update(cx, |editor, cx| {
            let Some(selection) =
                Self::source_range_to_anchors(editor, source_index..source_index, cx)
            else {
                return;
            };
            editor.change_selections(
                SelectionEffects::scroll(Autoscroll::center()),
                window,
                cx,
                |selections| selections.select_anchor_ranges(vec![selection]),
            );
            if move_focus {
                window.focus(&editor.focus_handle(cx), cx);
            }
        });
    }

    /// The absolute path of the file that is currently being previewed.
    fn get_folder_for_active_editor(editor: &Editor, cx: &App) -> Option<PathBuf> {
        let file = editor.file_at(MultiBufferOffset(0), cx)?;
        let absolute_path = editor
            .project()
            .and_then(|project| {
                project.read(cx).absolute_path(
                    &ProjectPath {
                        worktree_id: file.worktree_id(cx),
                        path: file.path().clone(),
                    },
                    cx,
                )
            })
            .or_else(|| file.as_local().map(|file| file.abs_path(cx)))?;
        absolute_path.parent().map(Path::to_path_buf)
    }

    fn line_scroll_amount(&self, cx: &App) -> Pixels {
        let settings = ThemeSettings::get_global(cx);
        settings.markdown_preview_font_size(cx) * settings.buffer_line_height.value()
    }

    fn increase_font_size(
        &mut self,
        action: &IncreaseBufferFontSize,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.adjust_font_size(action.persist, px(1.0), cx);
    }

    fn decrease_font_size(
        &mut self,
        action: &DecreaseBufferFontSize,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.adjust_font_size(action.persist, px(-1.0), cx);
    }

    fn adjust_font_size(&mut self, persist: bool, delta: Pixels, cx: &mut Context<Self>) {
        if persist {
            let Ok(fs) = self
                .workspace
                .read_with(cx, |workspace, _| workspace.app_state().fs.clone())
            else {
                return;
            };
            update_settings_file(fs, cx, move |settings, cx| {
                let size = ThemeSettings::get_global(cx).markdown_preview_font_size(cx) + delta;
                settings.markdown_preview.get_or_insert_default().font_size =
                    Some(f32::from(theme::clamp_font_size(size)).into());
            });
        } else {
            theme::adjust_markdown_preview_font_size(cx, |size| size + delta);
        }
    }

    fn reset_font_size(
        &mut self,
        action: &ResetBufferFontSize,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if action.persist {
            let Ok(fs) = self
                .workspace
                .read_with(cx, |workspace, _| workspace.app_state().fs.clone())
            else {
                return;
            };
            update_settings_file(fs, cx, move |settings, _| {
                reset_persisted_font_size(settings);
            });
        } else {
            theme::reset_markdown_preview_font_size(cx);
        }
    }

    fn scroll_by_amount(&self, distance: Pixels) {
        let offset = self.scroll_handle.offset();
        self.scroll_handle
            .set_offset(point(offset.x, offset.y - distance));
    }

    fn scroll_page_up(&mut self, _: &ScrollPageUp, _window: &mut Window, cx: &mut Context<Self>) {
        let viewport_height = self.scroll_handle.bounds().size.height;
        if viewport_height.is_zero() {
            return;
        }

        self.scroll_by_amount(-viewport_height);
        cx.notify();
    }

    fn scroll_page_down(
        &mut self,
        _: &ScrollPageDown,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let viewport_height = self.scroll_handle.bounds().size.height;
        if viewport_height.is_zero() {
            return;
        }

        self.scroll_by_amount(viewport_height);
        cx.notify();
    }

    fn scroll_up(&mut self, _: &ScrollUp, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(bounds) = self
            .scroll_handle
            .bounds_for_item(self.scroll_handle.top_item())
        {
            let item_height = bounds.size.height;
            // Scroll no more than the rough equivalent of a large headline
            let max_height = window.rem_size() * 2;
            let scroll_height = min(item_height, max_height);
            self.scroll_by_amount(-scroll_height);
        } else {
            let scroll_height = self.line_scroll_amount(cx);
            if !scroll_height.is_zero() {
                self.scroll_by_amount(-scroll_height);
            }
        }
        cx.notify();
    }

    fn scroll_down(&mut self, _: &ScrollDown, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(bounds) = self
            .scroll_handle
            .bounds_for_item(self.scroll_handle.top_item())
        {
            let item_height = bounds.size.height;
            // Scroll no more than the rough equivalent of a large headline
            let max_height = window.rem_size() * 2;
            let scroll_height = min(item_height, max_height);
            self.scroll_by_amount(scroll_height);
        } else {
            let scroll_height = self.line_scroll_amount(cx);
            if !scroll_height.is_zero() {
                self.scroll_by_amount(scroll_height);
            }
        }
        cx.notify();
    }

    fn scroll_up_by_item(
        &mut self,
        _: &ScrollUpByItem,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(bounds) = self
            .scroll_handle
            .bounds_for_item(self.scroll_handle.top_item())
        {
            self.scroll_by_amount(-bounds.size.height);
        }
        cx.notify();
    }

    fn scroll_down_by_item(
        &mut self,
        _: &ScrollDownByItem,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(bounds) = self
            .scroll_handle
            .bounds_for_item(self.scroll_handle.top_item())
        {
            self.scroll_by_amount(bounds.size.height);
        }
        cx.notify();
    }

    fn scroll_to_top(&mut self, _: &ScrollToTop, _window: &mut Window, cx: &mut Context<Self>) {
        self.scroll_handle.scroll_to_item(0);
        cx.notify();
    }

    fn scroll_to_bottom(
        &mut self,
        _: &ScrollToBottom,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.scroll_handle.scroll_to_bottom();
        cx.notify();
    }

    pub(crate) fn close_and_return_to_editor(
        &mut self,
        _: &CloseAndReturnToEditor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The split view the preview belongs to switches itself to its editor.
        if self.embedded {
            cx.propagate();
            return;
        }
        let Some(editor) = self
            .active_editor
            .as_ref()
            .map(|state| state.editor.clone())
        else {
            return;
        };
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let preview_id = cx.entity_id();

        window.defer(cx, move |window, cx| {
            workspace.update(cx, |workspace, cx| {
                if let Some(owner) =
                    markdown_split_view::item_showing_editor(workspace, &editor, cx)
                {
                    if owner.downcast::<MarkdownSplitView>().is_some() {
                        markdown_split_view::switch_layout(
                            workspace,
                            owner.as_ref(),
                            MarkdownLayout::SideBySide,
                            window,
                            cx,
                        );
                    }
                    workspace.activate_item(owner.as_ref(), true, true, window, cx);
                } else {
                    // Re-adding the source emits `ItemAdded`; suppress its automatic preview once.
                    cx.global_mut::<SuppressedAutoPreviews>()
                        .0
                        .insert(editor.entity_id());
                    workspace.add_item_to_active_pane(Box::new(editor), None, true, window, cx);
                }

                if let Some(pane) = workspace.pane_for_item_id(preview_id) {
                    pane.update(cx, |pane, cx| {
                        pane.close_item_by_id(preview_id, SaveIntent::Skip, window, cx)
                    })
                    .detach_and_log_err(cx);
                }
            });
        });
    }

    /// Returns the theme chosen in `markdown_preview.theme`, or `None` if the
    /// user hasn't set one or it can't be resolved.
    fn resolve_preview_theme(&self, cx: &App) -> Option<Arc<Theme>> {
        let theme_settings = ThemeSettings::get_global(cx);
        let theme_selection = theme_settings.markdown_preview_theme.as_ref()?;
        let theme_name = theme_selection.name(SystemAppearance::global(cx).0);
        ThemeRegistry::global(cx).get(&theme_name.0).ok()
    }

    fn render_markdown_element(
        &self,
        preview_theme: &Option<Arc<Theme>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> MarkdownElement {
        let active_editor = self
            .active_editor
            .as_ref()
            .map(|state| state.editor.clone());

        let mut workspace_directory = None;
        if let Some(workspace_entity) = self.workspace.upgrade() {
            let project = workspace_entity.read(cx).project();
            if let Some(tree) = project.read(cx).worktrees(cx).next() {
                workspace_directory = Some(tree.read(cx).abs_path().to_path_buf());
            }
        }

        let markdown_style = if let Some(theme) = preview_theme {
            MarkdownStyle::themed_with_overrides(
                MarkdownFont::Preview,
                theme.colors(),
                theme.syntax(),
                window,
                cx,
            )
        } else {
            MarkdownStyle::themed(MarkdownFont::Preview, window, cx)
        };

        let mut markdown_element = MarkdownElement::new(self.markdown.clone(), markdown_style)
            .input_focus_handle(self.focus_handle.clone())
            .code_block_renderer(CodeBlockRenderer::Default {
                copy_button_visibility: CopyButtonVisibility::VisibleOnHover,
                wrap_button_visibility: markdown::WrapButtonVisibility::Hidden,
                border: false,
            })
            .scroll_handle(self.scroll_handle.clone())
            .show_root_block_markers()
            .image_resolver({
                let base_directory = self.base_directory.clone();
                move |dest_url| {
                    resolve_preview_image(
                        dest_url,
                        base_directory.as_deref(),
                        workspace_directory.as_deref(),
                    )
                }
            })
            .on_url_click({
                let view_handle = cx.entity().downgrade();
                let workspace = self.workspace.clone();
                let base_directory = self.base_directory.clone();
                move |url, window, cx| {
                    handle_url_click(
                        url,
                        &view_handle,
                        base_directory.clone(),
                        &workspace,
                        window,
                        cx,
                    );
                }
            })
            .on_url_hover({
                let view_handle = cx.entity().downgrade();
                move |hovered_url, _window, cx| {
                    view_handle
                        .update(cx, |view, cx| {
                            if view.hovered_url != hovered_url {
                                view.hovered_url = hovered_url;
                                cx.notify();
                            }
                        })
                        .log_err();
                }
            });

        if let Some(active_editor) = active_editor {
            let editor_for_checkbox = active_editor.clone();
            let view_handle = cx.entity().downgrade();
            markdown_element = markdown_element
                .on_source_click(move |source_index, click_count, window, cx| {
                    if click_count == 1 {
                        Self::change_selection_to_source_index(
                            &active_editor,
                            source_index,
                            false,
                            window,
                            cx,
                        );
                    }
                    false
                })
                .on_checkbox_toggle(move |source_range, new_checked, window, cx| {
                    Self::toggle_checkbox(
                        view_handle.clone(),
                        &editor_for_checkbox,
                        source_range,
                        new_checked,
                        window,
                        cx,
                    );
                });
        }

        markdown_element
    }

    fn toggle_checkbox(
        view_handle: WeakEntity<Self>,
        editor: &Entity<Editor>,
        source_range: Range<usize>,
        new_checked: bool,
        window: &mut Window,
        cx: &mut App,
    ) {
        Self::apply_checkbox_toggle_to_editor(editor, source_range, new_checked, cx);

        // A preview never owns its source's unsaved edits. When no tab shows the source buffer,
        // nothing would report the toggle as unsaved, so save it right away instead.
        let source_has_owner = view_handle.upgrade().is_none_or(|view| {
            let view = view.read(cx);
            view.embedded || view.source_buffer_has_owner(editor, cx)
        });
        if !source_has_owner && let Some(project) = editor.read(cx).project().cloned() {
            Pane::autosave_item(editor, project, window, cx).detach_and_log_err(cx);
        }

        Self::refresh_preview(view_handle, window, cx);
    }

    fn source_buffer_has_owner(&self, editor: &Entity<Editor>, cx: &App) -> bool {
        let Some(buffer) = editor.read(cx).buffer().read(cx).as_singleton() else {
            return true;
        };
        let Some(workspace) = self.workspace.upgrade() else {
            return true;
        };
        workspace
            .read(cx)
            .items(cx)
            .filter(|item| item.downcast::<MarkdownPreviewView>().is_none())
            .filter_map(|item| item.act_as::<Editor>(cx))
            .any(|editor| {
                editor.read(cx).buffer().read(cx).as_singleton().as_ref() == Some(&buffer)
            })
    }

    fn apply_checkbox_toggle_to_editor(
        editor: &Entity<Editor>,
        source_range: std::ops::Range<usize>,
        new_checked: bool,
        cx: &mut App,
    ) {
        let task_marker = if new_checked { "[x]" } else { "[ ]" };

        editor.update(cx, |editor, cx| {
            let Some(marker_range) = Self::source_range_to_anchors(editor, source_range, cx) else {
                return;
            };
            let existing_marker: String = editor
                .buffer()
                .read(cx)
                .snapshot(cx)
                .text_for_range(marker_range.clone())
                .collect();

            // The renderer treats both `[x]` and `[X]` as checked.
            debug_assert!(
                if new_checked {
                    existing_marker == "[ ]"
                } else {
                    existing_marker.eq_ignore_ascii_case("[x]")
                },
                "unexpected task marker {existing_marker:?}"
            );

            editor.edit([(marker_range, task_marker)], cx);
        });
    }

    fn refresh_preview(view_handle: WeakEntity<Self>, window: &mut Window, cx: &mut App) {
        if let Some(view) = view_handle.upgrade() {
            let preview_is_focused = view.read(cx).focus_handle.contains_focused(window, cx);
            if !preview_is_focused {
                return;
            }

            cx.update_entity(&view, |this, cx| {
                this.update_markdown_from_active_editor(false, false, window, cx);
            });
        }
    }
}

fn handle_url_click(
    url: SharedString,
    view: &WeakEntity<MarkdownPreviewView>,
    base_directory: Option<PathBuf>,
    workspace: &WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut App,
) {
    let (path_part, fragment) = split_local_url_fragment(url.as_ref());

    if path_part.is_empty() {
        if let Some(fragment) = fragment {
            let view = view.clone();
            let slug = SharedString::from(fragment.to_string());
            window.defer(cx, move |window, cx| {
                if let Some(view) = view.upgrade() {
                    let markdown = view.read(cx).markdown.clone();
                    let active_editor = view
                        .read(cx)
                        .active_editor
                        .as_ref()
                        .map(|state| state.editor.clone());
                    // A source editor without a tab isn't rendered, so focusing it would only take
                    // keyboard input away from the preview. A split view keeps the focus on its
                    // preview by itself while the editor is hidden.
                    let move_focus = view.read(cx).embedded
                        || active_editor.as_ref().is_some_and(|editor| {
                            view.read(cx).workspace.upgrade().is_some_and(|workspace| {
                                markdown_split_view::item_showing_editor(
                                    workspace.read(cx),
                                    editor,
                                    cx,
                                )
                                .is_some()
                            })
                        });

                    let source_index =
                        markdown.update(cx, |markdown, cx| markdown.scroll_to_heading(&slug, cx));

                    if let Some(source_index) = source_index {
                        if let Some(editor) = active_editor {
                            MarkdownPreviewView::change_selection_to_source_index(
                                &editor,
                                source_index,
                                move_focus,
                                window,
                                cx,
                            );
                        }
                    }
                }
            });
        }
    } else {
        open_preview_url(
            SharedString::from(path_part.to_string()),
            fragment.map(|fragment| SharedString::from(fragment.to_string())),
            base_directory,
            workspace,
            window,
            cx,
        );
    }
}

fn open_preview_url(
    url: SharedString,
    fragment: Option<SharedString>,
    base_directory: Option<PathBuf>,
    workspace: &WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut App,
) {
    // Percent escapes in web and file URLs are part of the URL, so only local paths are decoded.
    let decoded_path = if url.contains("://") {
        Cow::Borrowed(url.as_ref())
    } else {
        urlencoding::decode(&url).unwrap_or_else(|_| Cow::Borrowed(&url))
    };

    if let Some(row) = fragment
        .as_deref()
        .and_then(|fragment| fragment.strip_prefix('L'))
        .and_then(source_position_from_fragment)
        .map(|(row, _)| row)
        && open_preview_path_at_line(
            decoded_path.to_string(),
            base_directory.clone(),
            workspace,
            row,
            window,
            cx,
        )
    {
        return;
    }

    if let Some(workspace) = workspace.upgrade() {
        workspace.update(cx, |workspace, cx| {
            workspace.open_url_or_file(&decoded_path, base_directory.as_deref(), window, cx);
        });
    } else {
        cx.open_url(url.as_ref());
    }
}

enum PreviewPathTarget {
    Project(ProjectPath),
    Absolute(PathBuf),
}

fn open_preview_path_at_line(
    path: String,
    base_directory: Option<PathBuf>,
    workspace: &WeakEntity<Workspace>,
    row: u32,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    let Some(workspace) = workspace.upgrade() else {
        return false;
    };
    let project = workspace.read(cx).project().clone();
    let path_style = project.read(cx).path_style(cx);
    let target = if path_style.is_absolute(&path) {
        Some(PreviewPathTarget::Absolute(PathBuf::from(path)))
    } else if project.read(cx).is_local()
        && let Some(resolved) = base_directory
            .as_deref()
            .map(|base_directory| base_directory.join(&path))
        && resolved.exists()
    {
        Some(PreviewPathTarget::Absolute(resolved))
    } else {
        project
            .update(cx, |project, cx| project.find_project_path(&path, cx))
            .map(PreviewPathTarget::Project)
    };
    let Some(target) = target else {
        return false;
    };

    let task = workspace.update(cx, |workspace, cx| match target {
        PreviewPathTarget::Project(project_path) => {
            workspace.open_path(project_path, None, true, window, cx)
        }
        PreviewPathTarget::Absolute(abs_path) => workspace.open_abs_path(
            abs_path,
            workspace::OpenOptions {
                focus: Some(true),
                ..Default::default()
            },
            window,
            cx,
        ),
    });
    window
        .spawn(cx, async move |cx| {
            let item = task.await?;
            let Some(editor) = cx.update(|_, cx| item.act_as::<Editor>(cx))? else {
                return anyhow::Ok(());
            };
            editor.update_in(cx, |editor, window, cx| {
                let point = Point::new(row, 0);
                editor.change_selections(
                    SelectionEffects::scroll(Autoscroll::center()),
                    window,
                    cx,
                    |selections| selections.select_ranges([point..point]),
                );
            })?;
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    true
}

fn resolve_preview_image(
    dest_url: &str,
    base_directory: Option<&Path>,
    workspace_directory: Option<&Path>,
) -> Option<ImageSource> {
    if dest_url.starts_with("data:") {
        return None;
    }

    if dest_url.starts_with("http://") || dest_url.starts_with("https://") {
        return Some(ImageSource::Resource(Resource::Uri(SharedUri::from(
            dest_url.to_string(),
        ))));
    }

    let decoded = urlencoding::decode(dest_url)
        .map(|decoded| decoded.into_owned())
        .unwrap_or_else(|_| dest_url.to_string());

    if let Some(stripped) = ['/', '\\']
        .iter()
        .find_map(|prefix| decoded.strip_prefix(*prefix))
    {
        // A leading slash can mean the workspace root, but fall through to the filesystem path
        // when no such file exists there.
        if let Some(root) = workspace_directory {
            let absolute_path = root.join(stripped);
            if absolute_path.exists() {
                return Some(ImageSource::Resource(Resource::Path(Arc::from(
                    absolute_path.as_path(),
                ))));
            }
        }
    }

    let path = if Path::new(&decoded).is_absolute() {
        PathBuf::from(decoded)
    } else {
        base_directory?.join(decoded)
    };

    path.exists()
        .then(|| ImageSource::Resource(Resource::Path(Arc::from(path.as_path()))))
}

impl Focusable for MarkdownPreviewView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<MarkdownPreviewEvent> for MarkdownPreviewView {}
impl EventEmitter<SearchEvent> for MarkdownPreviewView {}

impl Item for MarkdownPreviewView {
    type Event = MarkdownPreviewEvent;

    fn act_as_type<'a>(
        &'a self,
        type_id: TypeId,
        self_handle: &'a Entity<Self>,
        _: &'a App,
    ) -> Option<gpui::AnyEntity> {
        if type_id == TypeId::of::<Self>() {
            Some(self_handle.clone().into())
        } else if type_id == TypeId::of::<Editor>() {
            self.active_editor
                .as_ref()
                .map(|state| state.editor.clone().into())
        } else {
            None
        }
    }

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<Icon> {
        Some(Icon::new(IconName::FileDoc))
    }

    fn tab_content(&self, params: TabContentParams, _window: &Window, cx: &App) -> AnyElement {
        let editor = self
            .active_editor
            .as_ref()
            .map(|editor_state| editor_state.editor.read(cx));
        let max_title_len = MAX_TAB_TITLE_LEN;

        // Like editor tabs, the title is only the file name, since `tab_content_text` can be a
        // full path for files outside the project. The directories needed to tell apart files
        // with the same name are shown in the description.
        let title = match editor {
            Some(editor) => format!(
                "Preview {}",
                util::truncate_and_trailoff(&editor.title(cx), max_title_len)
            ),
            None => "Markdown Preview".to_string(),
        };
        let description = editor
            .zip(params.detail)
            .and_then(|(editor, detail)| editor.tab_description(detail, cx))
            .map(|description| util::truncate_and_trailoff(&description, max_title_len));

        h_flex()
            .gap_1()
            .child(Label::new(title).single_line().color(params.text_color()))
            .when_some(description, |this, description| {
                this.child(
                    Label::new(description)
                        .single_line()
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                )
            })
            .into_any_element()
    }

    fn tab_content_text(&self, detail: usize, cx: &App) -> SharedString {
        self.active_editor
            .as_ref()
            .map(|editor_state| {
                let title = editor_state.editor.read(cx).tab_content_text(detail, cx);
                format!("Preview {title}").into()
            })
            .unwrap_or_else(|| SharedString::from("Markdown Preview"))
    }

    // Save As writes the source file, so suggest its name rather than the tab text, which can be a
    // full path for files outside the project.
    fn suggested_filename(&self, cx: &App) -> SharedString {
        self.active_editor
            .as_ref()
            .map(|editor_state| editor_state.editor.read(cx).suggested_filename(cx))
            .unwrap_or_else(|| SharedString::from("Markdown Preview"))
    }

    fn tab_tooltip_text(&self, cx: &App) -> Option<SharedString> {
        self.active_editor
            .as_ref()?
            .editor
            .read(cx)
            .tab_tooltip_text(cx)
    }

    fn telemetry_event_text(&self) -> Option<&'static str> {
        Some("Markdown Preview Opened")
    }

    fn tab_extra_context_menu_actions(
        &self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<(SharedString, Box<dyn Action>)> {
        let preview_id = cx.entity_id();
        // Inactive tabs aren't rendered, so the action can't be dispatched to them.
        let is_active_in_pane = self
            .workspace
            .upgrade()
            .and_then(|workspace| workspace.read(cx).pane_for_item_id(preview_id))
            .and_then(|pane| pane.read(cx).active_item())
            .is_some_and(|item| item.item_id() == preview_id);
        if !is_active_in_pane {
            return Vec::new();
        }
        vec![("Show Source".into(), Box::new(CloseAndReturnToEditor))]
    }

    fn added_to_workspace(
        &mut self,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.mode != MarkdownPreviewMode::Default {
            return;
        }
        if let Some(editor) = self.find_canonical_editor(workspace, cx)
            && self
                .active_editor
                .as_ref()
                .is_none_or(|s| s.editor != editor)
        {
            self.set_editor(editor, window, cx);
        }
    }

    fn can_save(&self, cx: &App) -> bool {
        self.active_editor
            .as_ref()
            .is_some_and(|editor_state| editor_state.editor.read(cx).can_save(cx))
    }

    fn can_save_as(&self, cx: &App) -> bool {
        self.active_editor
            .as_ref()
            .is_some_and(|editor_state| editor_state.editor.read(cx).can_save_as(cx))
    }

    fn save(
        &mut self,
        options: SaveOptions,
        project: Entity<Project>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        self.active_editor
            .as_ref()
            .map(|editor_state| {
                editor_state
                    .editor
                    .update(cx, |editor, cx| editor.save(options, project, window, cx))
            })
            .unwrap_or_else(|| Task::ready(Ok(())))
    }

    fn save_as(
        &mut self,
        project: Entity<Project>,
        path: project::ProjectPath,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        self.active_editor
            .as_ref()
            .map(|editor_state| {
                editor_state
                    .editor
                    .update(cx, |editor, cx| editor.save_as(project, path, window, cx))
            })
            .unwrap_or_else(|| Task::ready(Ok(())))
    }

    // A preview never owns its source's unsaved edits, so force-closing it has nothing to discard.
    fn reload(
        &mut self,
        _project: Entity<Project>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        Task::ready(Ok(()))
    }

    fn to_item_events(event: &Self::Event, f: &mut dyn FnMut(workspace::item::ItemEvent)) {
        match event {
            MarkdownPreviewEvent::SourceEditorChanged
            | MarkdownPreviewEvent::SourceFileHandleChanged => {
                f(workspace::item::ItemEvent::UpdateTab);
                f(workspace::item::ItemEvent::UpdateBreadcrumbs);
            }
            MarkdownPreviewEvent::SourceDirtyChanged => {
                f(workspace::item::ItemEvent::UpdateTab);
            }
            MarkdownPreviewEvent::SourceSelectionChanged => {
                f(workspace::item::ItemEvent::UpdateBreadcrumbs);
            }
        }
    }

    fn buffer_kind(&self, _cx: &App) -> ItemBufferKind {
        ItemBufferKind::Singleton
    }

    fn as_searchable(
        &self,
        handle: &Entity<Self>,
        _: &App,
    ) -> Option<Box<dyn SearchableItemHandle>> {
        Some(Box::new(handle.clone()))
    }

    fn breadcrumb_location(&self, cx: &App) -> ToolbarItemLocation {
        match &self.active_editor {
            Some(editor_state) => editor_state.editor.read(cx).breadcrumb_location(cx),
            None => ToolbarItemLocation::Hidden,
        }
    }

    fn breadcrumbs(&self, cx: &App) -> Option<(Vec<HighlightedText>, Option<Font>)> {
        self.active_editor.as_ref()?.editor.read(cx).breadcrumbs(cx)
    }

    fn can_split(&self) -> bool {
        true
    }

    fn clone_on_split(
        &self,
        _workspace_id: Option<WorkspaceId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Option<Entity<Self>>>
    where
        Self: Sized,
    {
        let Some(active_editor) = self.active_editor.as_ref() else {
            return Task::ready(None);
        };

        let Some(project) = active_editor.editor.read(cx).project() else {
            return Task::ready(None);
        };

        let language_registry = project.read(cx).languages().clone();

        Task::ready(Some(MarkdownPreviewView::new(
            MarkdownPreviewMode::Default,
            active_editor.editor.clone(),
            self.workspace.clone(),
            language_registry,
            window,
            cx,
        )))
    }

    fn set_nav_history(
        &mut self,
        history: ItemNavHistory,
        _window: &mut Window,
        _: &mut Context<Self>,
    ) {
        self.nav_history = Some(history);
    }

    fn deactivated(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(nav_history) = self.nav_history.as_mut() {
            nav_history.push::<()>(None, cx);
        }
    }
}

impl Render for MarkdownPreviewView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let preview_theme = self.resolve_preview_theme(cx);
        let bg_color = preview_theme
            .as_ref()
            .map(|theme| theme.colors().editor_background)
            .unwrap_or_else(|| cx.theme().colors().editor_background);
        let preview_font_size = ThemeSettings::get_global(cx).markdown_preview_font_size(cx);
        let hovered_url = self.hovered_url.clone();
        div()
            .image_cache(self.image_cache.clone())
            .id("MarkdownPreview")
            .key_context("MarkdownPreview")
            .track_focus(&self.focus_handle(cx))
            .on_hover(cx.listener(|view, hovered, _window, cx| {
                if !hovered && view.hovered_url.take().is_some() {
                    cx.notify();
                }
            }))
            .on_action(cx.listener(MarkdownPreviewView::scroll_page_up))
            .on_action(cx.listener(MarkdownPreviewView::scroll_page_down))
            .on_action(cx.listener(MarkdownPreviewView::scroll_up))
            .on_action(cx.listener(MarkdownPreviewView::scroll_down))
            .on_action(cx.listener(MarkdownPreviewView::scroll_up_by_item))
            .on_action(cx.listener(MarkdownPreviewView::scroll_down_by_item))
            .on_action(cx.listener(MarkdownPreviewView::scroll_to_top))
            .on_action(cx.listener(MarkdownPreviewView::scroll_to_bottom))
            .on_action(cx.listener(MarkdownPreviewView::close_and_return_to_editor))
            .on_action(cx.listener(MarkdownPreviewView::increase_font_size))
            .on_action(cx.listener(MarkdownPreviewView::decrease_font_size))
            .on_action(cx.listener(MarkdownPreviewView::reset_font_size))
            .w_full()
            .flex_1()
            .min_h_0()
            .relative()
            .bg(bg_color)
            .child(
                WithRemSize::new(preview_font_size).size_full().child(
                    div()
                        .id("markdown-preview-scroll-container")
                        .size_full()
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll_handle)
                        .p_4()
                        .child({
                            let markdown_element =
                                self.render_markdown_element(&preview_theme, window, cx);
                            let markdown = self.markdown.clone();
                            let max_width = MarkdownPreviewSettings::get_global(cx).max_width;
                            let content = right_click_menu("markdown-preview-context-menu")
                                .trigger(move |_, _, _| markdown_element)
                                .maybe_menu(move |window, cx| {
                                    let focus = window.focused(cx);
                                    let markdown = markdown.read(cx);
                                    let context_menu_link = markdown.context_menu_link().cloned();
                                    let selected_text =
                                        markdown.context_menu_selected_text().cloned();
                                    let selected_markdown =
                                        markdown.context_menu_selected_markdown().cloned();
                                    if context_menu_link.is_none()
                                        && selected_text.is_none()
                                        && selected_markdown.is_none()
                                    {
                                        return None;
                                    }
                                    Some(ContextMenu::build(window, cx, move |menu, _, _cx| {
                                        menu.when_some(focus, |menu, focus| menu.context(focus))
                                            .when_some(selected_text, |menu, text| {
                                                menu.entry(
                                                    "Copy",
                                                    Some(Box::new(markdown::Copy)),
                                                    move |_, cx| {
                                                        cx.write_to_clipboard(
                                                            ClipboardItem::new_string(
                                                                text.to_string(),
                                                            ),
                                                        );
                                                    },
                                                )
                                            })
                                            .when_some(selected_markdown, |menu, text| {
                                                menu.entry(
                                                    "Copy as Markdown",
                                                    Some(Box::new(markdown::CopyAsMarkdown)),
                                                    move |_, cx| {
                                                        cx.write_to_clipboard(
                                                            ClipboardItem::new_string(
                                                                text.to_string(),
                                                            ),
                                                        );
                                                    },
                                                )
                                            })
                                            .when_some(context_menu_link, |menu, url| {
                                                menu.entry("Copy Link", None, move |_, cx| {
                                                    cx.write_to_clipboard(
                                                        ClipboardItem::new_string(url.to_string()),
                                                    );
                                                })
                                            })
                                    }))
                                });
                            div()
                                .w_full()
                                .when_some(max_width, |this, max_width| {
                                    this.max_w(max_width).mx_auto()
                                })
                                .child(content)
                        }),
                ),
            )
            .custom_scrollbars(
                Scrollbars::for_settings::<EditorSettings>()
                    .show_along(ScrollAxes::Vertical)
                    .tracked_scroll_handle(&self.scroll_handle),
                window,
                cx,
            )
            .when_some(hovered_url, |this, hovered_url| {
                this.child(
                    div()
                        .absolute()
                        .bottom_2()
                        .left_0()
                        .max_w_full()
                        .overflow_hidden()
                        .child(LinkPreview::new(hovered_url.as_ref(), cx)),
                )
            })
    }
}

impl SearchableItem for MarkdownPreviewView {
    type Match = Range<usize>;

    fn supported_options(&self) -> SearchOptions {
        SearchOptions {
            case: true,
            word: true,
            regex: true,
            replacement: false,
            selection: false,
            select_all: false,
            find_in_results: false,
        }
    }

    fn get_matches(&self, _window: &mut Window, cx: &mut App) -> (Vec<Self::Match>, SearchToken) {
        (
            self.markdown.read(cx).search_highlights().to_vec(),
            SearchToken::default(),
        )
    }

    fn clear_matches(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let had_highlights = !self.markdown.read(cx).search_highlights().is_empty();
        self.markdown.update(cx, |markdown, cx| {
            markdown.clear_search_highlights(cx);
        });
        if had_highlights {
            cx.emit(SearchEvent::MatchesInvalidated);
        }
    }

    fn update_matches(
        &mut self,
        matches: &[Self::Match],
        active_match_index: Option<usize>,
        _token: SearchToken,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        debug_assert!(
            matches
                .windows(2)
                .all(|ranges| (ranges[0].start, ranges[0].end) <= (ranges[1].start, ranges[1].end))
        );
        let old_highlights = self.markdown.read(cx).search_highlights();
        let changed = old_highlights != matches;
        self.markdown.update(cx, |markdown, cx| {
            markdown.set_search_highlights(matches.to_vec(), active_match_index, cx);
        });
        if changed {
            cx.emit(SearchEvent::MatchesInvalidated);
        }
    }

    fn query_suggestion(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> String {
        self.markdown
            .read(cx)
            .selected_source()
            .unwrap_or_default()
            .to_string()
    }

    fn activate_match(
        &mut self,
        index: usize,
        matches: &[Self::Match],
        _token: SearchToken,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(match_range) = matches.get(index) {
            let start = match_range.start;
            self.markdown.update(cx, |markdown, cx| {
                markdown.set_active_search_highlight(Some(index), cx);
                markdown.request_autoscroll_to_source_index(start, cx);
            });
            cx.emit(SearchEvent::ActiveMatchChanged);
        }
    }

    fn select_matches(
        &mut self,
        _matches: &[Self::Match],
        _token: SearchToken,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
    }

    fn replace(
        &mut self,
        _: &Self::Match,
        _: &SearchQuery,
        _token: SearchToken,
        _window: &mut Window,
        _: &mut Context<Self>,
    ) {
    }

    fn find_matches(
        &mut self,
        query: Arc<SearchQuery>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Vec<Self::Match>> {
        let markdown = self.markdown.read(cx);
        let source = markdown.source().to_string();
        let non_rendered_ranges = markdown.non_rendered_source_ranges();
        cx.background_spawn(async move {
            filter_non_rendered_matches(query.search_str(&source), &non_rendered_ranges)
        })
    }

    fn active_match_index(
        &mut self,
        direction: Direction,
        matches: &[Self::Match],
        _token: SearchToken,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<usize> {
        if matches.is_empty() {
            return None;
        }

        let markdown = self.markdown.read(cx);
        let current_source_index = markdown
            .active_search_highlight()
            .and_then(|i| markdown.search_highlights().get(i))
            .map(|m| m.start)
            .or(self.active_source_index)
            .unwrap_or(0);

        match direction {
            Direction::Next => matches
                .iter()
                .position(|m| m.start >= current_source_index)
                .or(Some(0)),
            Direction::Prev => matches
                .iter()
                .rposition(|m| m.start <= current_source_index)
                .or(Some(matches.len().saturating_sub(1))),
        }
    }
}

/// `non_rendered_ranges` must be sorted by start and disjoint, as returned by
/// [`Markdown::non_rendered_source_ranges`]. Matches may arrive in any order.
fn filter_non_rendered_matches(
    matches: Vec<Range<usize>>,
    non_rendered_ranges: &[Range<usize>],
) -> Vec<Range<usize>> {
    matches
        .into_iter()
        .filter(|match_range| {
            let candidate =
                non_rendered_ranges.partition_point(|range| range.end <= match_range.start);

            !non_rendered_ranges
                .get(candidate)
                .is_some_and(|range| range.start < match_range.end)
        })
        .collect()
}

impl SerializableItem for MarkdownPreviewView {
    fn serialized_item_kind() -> &'static str {
        "MarkdownPreviewView"
    }

    fn deserialize(
        project: Entity<Project>,
        workspace: WeakEntity<Workspace>,
        workspace_id: WorkspaceId,
        item_id: ItemId,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Entity<Self>>> {
        let db = &*persistence::MARKDOWN_PREVIEW_DB;
        window.spawn(cx, async move |cx| {
            let (abs_path, mode_value) = db
                .get_preview(item_id, workspace_id)?
                .context("No markdown preview entry found")?;
            let mode = MarkdownPreviewMode::from_db(mode_value);

            let (worktree, relative_path) = project
                .update(cx, |project, cx| {
                    project.find_or_create_worktree(abs_path.clone(), false, cx)
                })
                .await
                .context("Path not found")?;
            let worktree_id = worktree.read_with(cx, |worktree, _| worktree.id());

            let project_path = ProjectPath {
                worktree_id,
                path: relative_path,
            };

            let buffer = project
                .update(cx, |project, cx| project.open_buffer(project_path, cx))
                .await?;

            cx.update(|window, cx| {
                let language_registry = project.read(cx).languages().clone();
                // Rebinds to the file's tab once the workspace has restored it.
                let editor =
                    cx.new(|cx| Editor::for_buffer(buffer, Some(project.clone()), window, cx));
                MarkdownPreviewView::new(mode, editor, workspace, language_registry, window, cx)
            })
        })
    }

    fn cleanup(
        workspace_id: WorkspaceId,
        alive_items: Vec<ItemId>,
        _window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<()>> {
        let db = &*persistence::MARKDOWN_PREVIEW_DB;
        delete_unloaded_items(alive_items, workspace_id, "markdown_previews", &db, cx)
    }

    fn serialize(
        &mut self,
        workspace: &mut Workspace,
        item_id: ItemId,
        _closing: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<()>>> {
        let workspace_id = workspace.database_id()?;
        let editor = self.active_editor.as_ref()?.editor.clone();
        let buffer = editor.read(cx).buffer().read(cx).as_singleton()?;
        let file = buffer.read(cx).file()?;
        let worktree_id = file.worktree_id(cx);
        let abs_path = workspace
            .project()
            .read(cx)
            .worktree_for_id(worktree_id, cx)?
            .read(cx)
            .absolutize(file.path());
        let mode = self.mode.to_db();
        let db = &*persistence::MARKDOWN_PREVIEW_DB;
        Some(cx.background_spawn(async move {
            db.save_preview(item_id, workspace_id, abs_path, mode).await
        }))
    }

    fn should_serialize(&self, event: &Self::Event) -> bool {
        matches!(
            event,
            MarkdownPreviewEvent::SourceEditorChanged
                | MarkdownPreviewEvent::SourceFileHandleChanged
        )
    }
}

mod persistence {
    use std::path::PathBuf;

    use db::{
        query,
        sqlez::{domain::Domain, thread_safe_connection::ThreadSafeConnection},
        sqlez_macros::sql,
    };
    use workspace::{ItemId, WorkspaceDb, WorkspaceId};

    pub struct MarkdownPreviewDb(ThreadSafeConnection);

    impl Domain for MarkdownPreviewDb {
        const NAME: &str = stringify!(MarkdownPreviewDb);

        const MIGRATIONS: &[&str] = &[sql!(
            CREATE TABLE markdown_previews (
                workspace_id INTEGER,
                item_id INTEGER,
                abs_path BLOB,
                mode INTEGER NOT NULL DEFAULT 0,

                PRIMARY KEY(workspace_id, item_id),
                FOREIGN KEY(workspace_id) REFERENCES workspaces(workspace_id)
                ON DELETE CASCADE
            ) STRICT;
        )];
    }

    db::static_connection!(MARKDOWN_PREVIEW_DB, MarkdownPreviewDb, [WorkspaceDb]);

    impl MarkdownPreviewDb {
        query! {
            pub async fn save_preview(
                item_id: ItemId,
                workspace_id: WorkspaceId,
                abs_path: PathBuf,
                mode: i64
            ) -> Result<()> {
                INSERT OR REPLACE INTO markdown_previews(item_id, workspace_id, abs_path, mode)
                VALUES (?, ?, ?, ?)
            }
        }

        query! {
            pub fn get_preview(item_id: ItemId, workspace_id: WorkspaceId) -> Result<Option<(PathBuf, i64)>> {
                SELECT abs_path, mode
                FROM markdown_previews
                WHERE item_id = ? AND workspace_id = ?
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::CloseAndReturnToEditor;
    use crate::markdown_preview_view::ImageSource;
    use crate::markdown_preview_view::Resource;
    use crate::markdown_preview_view::handle_url_click;
    use crate::markdown_preview_view::resolve_preview_image;
    use buffer_diff::BufferDiff;
    use editor::{Editor, MultiBufferOffset, SelectionEffects};
    use fs::FakeFs;
    use gpui::UpdateGlobal as _;
    use gpui::{App, AppContext as _, Entity, Focusable, TestAppContext, WindowHandle};
    use language::Point;
    use project::{Project, ProjectPath};
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;
    use util::path;
    use util::rel_path::{RelPath, rel_path};
    use util::test::TempTree;
    use workspace::item::{Item, ItemHandle, SerializableItem};
    use workspace::{
        AppState, ItemId, MultiWorkspace, OpenOptions, OpenVisible, Pane, SaveIntent, Workspace,
        WorkspaceId, open_paths,
    };

    use super::{
        MarkdownPreviewView, filter_non_rendered_matches, open_preview_url,
        reset_persisted_font_size,
    };
    use crate::markdown_split_view::{MarkdownLayout, MarkdownSplitView};

    #[test]
    fn filters_matches_in_non_rendered_link_source() {
        let matches = vec![1..9, 30..37, 58..65];
        let non_rendered_ranges = vec![0..1, 9..38];

        assert_eq!(
            filter_non_rendered_matches(matches, &non_rendered_ranges),
            vec![1..9, 58..65]
        );
    }

    #[test]
    fn filters_matches_regardless_of_match_order() {
        let non_rendered_ranges = vec![0..1, 9..38];

        assert_eq!(
            filter_non_rendered_matches(vec![58..65, 1..9, 30..37], &non_rendered_ranges),
            vec![58..65, 1..9]
        );
    }

    #[gpui::test]
    async fn preview_tab_tooltip_matches_source_file_path(cx: &mut TestAppContext) {
        let (project, _, multi_workspace) = markdown_workspace(
            cx,
            json!({
                "docs": {
                    "guide.md": "# Guide"
                }
            }),
            false,
        )
        .await;
        let source_item =
            open_project_file(cx, &project, &multi_workspace, "docs/guide.md", None, true).await;
        let source_editor = cx.update(|cx| source_item.act_as::<Editor>(cx).unwrap());
        let preview = open_preview_for_active_editor(cx, &multi_workspace);

        let source_tooltip = source_editor.read_with(cx, |editor, cx| editor.tab_tooltip_text(cx));
        assert!(source_tooltip.is_some());
        assert_eq!(
            preview.read_with(cx, |preview, cx| preview.tab_tooltip_text(cx)),
            source_tooltip
        );
    }

    #[gpui::test]
    async fn preview_tabs_for_files_with_the_same_name_show_their_directory(
        cx: &mut TestAppContext,
    ) {
        let (project, _, multi_workspace) = markdown_workspace(
            cx,
            json!({
                "docs": { "README.md": "# Docs\n" },
                "guide": { "README.md": "# Guide\n" },
            }),
            false,
        )
        .await;
        open_project_file(cx, &project, &multi_workspace, "docs/README.md", None, true).await;
        let docs_preview = open_preview_for_active_editor(cx, &multi_workspace);
        open_project_file(
            cx,
            &project,
            &multi_workspace,
            "guide/README.md",
            None,
            true,
        )
        .await;
        open_preview_for_active_editor(cx, &multi_workspace);
        cx.run_until_parked();

        assert_eq!(
            docs_preview.read_with(cx, |preview, cx| preview.tab_content_text(0, cx)),
            "Preview README.md"
        );

        let mut preview_titles = multi_workspace
            .update(cx, |multi_workspace, window, cx| {
                let items = multi_workspace
                    .workspace()
                    .read(cx)
                    .active_pane()
                    .read(cx)
                    .items()
                    .map(|item| item.boxed_clone())
                    .collect::<Vec<_>>();
                let details = workspace::tab_details(&items, window, cx);
                items
                    .iter()
                    .zip(details)
                    .filter(|(item, _)| item.downcast::<MarkdownPreviewView>().is_some())
                    .map(|(item, detail)| item.tab_content_text(detail, cx).to_string())
                    .collect::<Vec<_>>()
            })
            .unwrap();
        preview_titles.sort();

        assert_eq!(
            preview_titles,
            [
                format!("Preview {}", path!("docs/README.md")),
                format!("Preview {}", path!("guide/README.md")),
            ]
        );
    }

    #[gpui::test]
    async fn preview_suggests_source_file_name_for_file_outside_the_project(
        cx: &mut TestAppContext,
    ) {
        let (project, workspace, multi_workspace) = markdown_workspace(cx, json!({}), false).await;
        let fs = project.read_with(cx, |project, _| project.fs().as_fake());
        fs.insert_tree(path!("/outside"), json!({ "README.md": "# Readme\n" }))
            .await;
        multi_workspace
            .update(cx, |_, window, cx| {
                workspace.update(cx, |workspace, cx| {
                    workspace.open_abs_path(
                        PathBuf::from(path!("/outside/README.md")),
                        OpenOptions {
                            visible: Some(OpenVisible::None),
                            ..Default::default()
                        },
                        window,
                        cx,
                    )
                })
            })
            .unwrap()
            .await
            .unwrap();
        let preview = open_preview_for_active_editor(cx, &multi_workspace);

        assert_eq!(
            preview.read_with(cx, |preview, cx| preview.suggested_filename(cx)),
            "README.md"
        );
    }

    #[gpui::test]
    fn resetting_persisted_font_size_preserves_unrelated_settings(cx: &mut App) {
        let settings_store = settings::SettingsStore::test(cx);
        cx.set_global(settings_store);
        let without_markdown_preview = "{\n  \"buffer_font_size\": 14\n}\n".to_string();
        assert_eq!(
            cx.global::<settings::SettingsStore>()
                .new_text_for_update(without_markdown_preview.clone(), reset_persisted_font_size)
                .unwrap(),
            without_markdown_preview
        );

        let updated = cx
            .global::<settings::SettingsStore>()
            .new_text_for_update(
                r#"{"markdown_preview":{"font_size":18,"open_markdown_files_in_preview":true}}"#
                    .into(),
                reset_persisted_font_size,
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&updated).unwrap(),
            json!({"markdown_preview": {"open_markdown_files_in_preview": true}})
        );
    }

    #[test]
    fn resolves_workspace_absolute_preview_image_path_and_rejects_missing() {
        let tree = TempTree::new(json!({
            "docs": {},
            "test_image.png": "mock data"
        }));
        let workspace_directory = tree.path();
        let base_directory = markdown_fixture_directory(&tree);
        let image_file = workspace_directory.join("test_image.png");

        for workspace_root_relative_path in ["/test_image.png", "\\test_image.png"] {
            let resolved = resolve_preview_image(
                workspace_root_relative_path,
                Some(&base_directory),
                Some(workspace_directory),
            );
            assert_resolved_preview_image_path(resolved, image_file.as_path());
        }

        let missing = resolve_preview_image(
            "/missing_image.png",
            Some(&base_directory),
            Some(workspace_directory),
        );
        assert!(missing.is_none());
    }

    #[test]
    fn resolves_absolute_preview_image_path_outside_the_workspace() {
        let workspace_tree = TempTree::new(json!({ "docs": {} }));
        let image_tree = TempTree::new(json!({ "outside.png": "mock data" }));
        let image_file = image_tree.path().join("outside.png");

        let resolved = resolve_preview_image(
            image_file.to_str().unwrap(),
            Some(&workspace_tree.path().join("docs")),
            Some(workspace_tree.path()),
        );
        assert_resolved_preview_image_path(resolved, image_file.as_path());
    }

    #[gpui::test]
    async fn opens_preview_file_link_at_line(cx: &mut TestAppContext) {
        init_test(cx);

        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/project"),
            json!({"src": {"main.rs": "first\nsecond\nthird\n"}}),
        )
        .await;
        let project = Project::test(fs, [path!("/project").as_ref()], cx).await;
        let (multi_workspace, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project, window, cx));
        let workspace =
            multi_workspace.read_with(cx, |multi_workspace, _| multi_workspace.workspace().clone());
        let workspace_weak = workspace.downgrade();

        multi_workspace.update_in(cx, |_, window, cx| {
            open_preview_url(
                "src/main.rs".into(),
                Some("L2".into()),
                None,
                &workspace_weak,
                window,
                cx,
            );
        });
        cx.run_until_parked();

        let editor = workspace.read_with(cx, |workspace, cx| {
            workspace
                .active_item(cx)
                .and_then(|item| item.downcast::<Editor>())
                .expect("file should be open in an editor")
        });
        editor.update_in(cx, |editor, window, cx| {
            let snapshot = editor.snapshot(window, cx);
            assert_eq!(editor.selections.newest::<Point>(&snapshot).head().row, 1);
        });
    }

    #[gpui::test]
    async fn opens_web_links_without_decoding_them(cx: &mut TestAppContext) {
        init_test(cx);
        let project = Project::test(FakeFs::new(cx.executor()), [], cx).await;
        let (multi_workspace, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project, window, cx));
        let workspace =
            multi_workspace.read_with(cx, |multi_workspace, _| multi_workspace.workspace().clone());
        let workspace_weak = workspace.downgrade();

        let url = "https://example.com/a%23b?value=a%26b";
        multi_workspace.update_in(cx, |_, window, cx| {
            open_preview_url(url.into(), None, None, &workspace_weak, window, cx);
        });
        cx.run_until_parked();

        assert_eq!(cx.opened_url(), Some(url.to_string()));
    }

    #[gpui::test]
    async fn toggles_task_checkbox_and_saves_when_preview_is_active(cx: &mut TestAppContext) {
        let app_state = init_test(cx);
        app_state
            .fs
            .as_fake()
            .insert_tree(
                path!("/dir"),
                json!({
                    "todo.md": "- [ ] Finish work\n"
                }),
            )
            .await;

        cx.update(|cx| {
            open_paths(
                &[PathBuf::from(path!("/dir/todo.md"))],
                app_state.clone(),
                workspace::OpenOptions::default(),
                cx,
            )
        })
        .await
        .unwrap();

        let multi_workspace = cx.update(|cx| cx.windows()[0].downcast::<MultiWorkspace>().unwrap());
        let preview = multi_workspace
            .update(cx, |multi_workspace, window, cx| {
                let workspace = multi_workspace.workspace().clone();
                let editor: Entity<Editor> = workspace
                    .read(cx)
                    .active_item(cx)
                    .and_then(|item| item.act_as::<Editor>(cx))
                    .unwrap();

                workspace.update(cx, |workspace, cx| {
                    let preview = MarkdownPreviewView::create_markdown_view(
                        workspace,
                        editor.clone(),
                        window,
                        cx,
                    );
                    workspace.active_pane().update(cx, |pane, cx| {
                        pane.add_item(Box::new(preview.clone()), true, true, None, window, cx)
                    });
                    preview
                })
            })
            .unwrap();
        cx.run_until_parked();

        let save_task = multi_workspace
            .update(cx, |multi_workspace, window, cx| {
                let workspace: Entity<Workspace> = multi_workspace.workspace().clone();
                let view_handle = preview.downgrade();
                assert!(preview.read(cx).focus_handle.contains_focused(window, cx));
                preview.update(cx, |preview, cx| {
                    let editor = preview.active_editor.as_ref().unwrap().editor.clone();
                    MarkdownPreviewView::apply_checkbox_toggle_to_editor(&editor, 2..5, true, cx);
                });
                MarkdownPreviewView::refresh_preview(view_handle, window, cx);

                workspace.update(cx, |workspace: &mut Workspace, cx| {
                    workspace.save_active_item(SaveIntent::Save, window, cx)
                })
            })
            .unwrap();

        save_task.await.unwrap();
        cx.run_until_parked();

        assert_eq!(
            app_state
                .fs
                .load(path!("/dir/todo.md").as_ref())
                .await
                .unwrap(),
            "- [x] Finish work\n"
        );
    }

    #[gpui::test]
    async fn preview_uses_buffer_contents_instead_of_diff_contents(cx: &mut TestAppContext) {
        let app_state = init_test(cx);
        app_state
            .fs
            .as_fake()
            .insert_tree(
                path!("/dir"),
                json!({
                    "note.md": "new\n"
                }),
            )
            .await;

        cx.update(|cx| {
            open_paths(
                &[PathBuf::from(path!("/dir/note.md"))],
                app_state.clone(),
                workspace::OpenOptions::default(),
                cx,
            )
        })
        .await
        .unwrap();

        let multi_workspace = cx.update(|cx| cx.windows()[0].downcast::<MultiWorkspace>().unwrap());
        let preview = multi_workspace
            .update(cx, |multi_workspace, window, cx| {
                let workspace = multi_workspace.workspace().clone();
                workspace.update(cx, |workspace, cx| {
                    let editor: Entity<Editor> = workspace
                        .active_item(cx)
                        .and_then(|item| item.act_as::<Editor>(cx))
                        .unwrap();
                    let buffer = editor.read(cx).buffer().read(cx).as_singleton().unwrap();
                    let diff = cx.new(|cx| {
                        BufferDiff::new_with_base_text(
                            "old\n",
                            &buffer.read(cx).text_snapshot(),
                            cx,
                        )
                    });
                    let multibuffer = editor.read(cx).buffer().clone();
                    multibuffer.update(cx, |multibuffer, cx| {
                        multibuffer.add_diff(diff, cx);
                        multibuffer.set_all_diff_hunks_expanded(cx);
                    });

                    let diff_text = multibuffer.read(cx).snapshot(cx).text();
                    assert!(diff_text.contains("old"));
                    assert!(diff_text.contains("new"));

                    let preview =
                        MarkdownPreviewView::create_markdown_view(workspace, editor, window, cx);
                    workspace.active_pane().update(cx, |pane, cx| {
                        pane.add_item(Box::new(preview.clone()), true, true, None, window, cx)
                    });
                    preview
                })
            })
            .unwrap();
        cx.run_until_parked();

        assert_eq!(
            preview.read_with(cx, |preview, cx| preview
                .markdown
                .read(cx)
                .source()
                .to_string()),
            "new\n"
        );
    }

    #[gpui::test]
    async fn source_offsets_map_past_expanded_deleted_hunks(cx: &mut TestAppContext) {
        let app_state = init_test(cx);
        app_state
            .fs
            .as_fake()
            .insert_tree(path!("/dir"), json!({ "note.md": "- [ ] a\n- [ ] b\n" }))
            .await;
        cx.update(|cx| {
            open_paths(
                &[PathBuf::from(path!("/dir/note.md"))],
                app_state.clone(),
                workspace::OpenOptions::default(),
                cx,
            )
        })
        .await
        .unwrap();

        let multi_workspace = cx.update(|cx| cx.windows()[0].downcast::<MultiWorkspace>().unwrap());
        let editor = multi_workspace
            .update(cx, |multi_workspace, _, cx| {
                let workspace = multi_workspace.workspace().clone();
                let editor: Entity<Editor> = workspace
                    .read(cx)
                    .active_item(cx)
                    .and_then(|item| item.act_as::<Editor>(cx))
                    .unwrap();
                let buffer = editor.read(cx).buffer().read(cx).as_singleton().unwrap();
                let diff = cx.new(|cx| {
                    BufferDiff::new_with_base_text(
                        "removed line\n- [ ] a\n- [ ] b\n",
                        &buffer.read(cx).text_snapshot(),
                        cx,
                    )
                });
                editor
                    .read(cx)
                    .buffer()
                    .clone()
                    .update(cx, |multibuffer, cx| {
                        multibuffer.add_diff(diff, cx);
                        multibuffer.set_all_diff_hunks_expanded(cx);
                    });
                editor
            })
            .unwrap();
        cx.run_until_parked();

        let source = "- [ ] a\n- [ ] b\n";
        let second_marker = source.rfind("[ ]").unwrap();
        cx.update(|cx| {
            MarkdownPreviewView::apply_checkbox_toggle_to_editor(
                &editor,
                second_marker..second_marker + 3,
                true,
                cx,
            );
        });
        let buffer_text = editor.read_with(cx, |editor, cx| {
            editor
                .buffer()
                .read(cx)
                .as_singleton()
                .unwrap()
                .read(cx)
                .text()
        });
        assert_eq!(buffer_text, "- [ ] a\n- [x] b\n");

        multi_workspace
            .update(cx, |_, window, cx| {
                MarkdownPreviewView::change_selection_to_source_index(
                    &editor,
                    second_marker,
                    false,
                    window,
                    cx,
                );
                editor.update(cx, |editor, cx| {
                    assert_eq!(
                        MarkdownPreviewView::selected_source_index(editor, cx),
                        Some(second_marker)
                    );
                });
            })
            .unwrap();
    }

    #[gpui::test]
    async fn force_closing_preview_preserves_source_editor_changes(cx: &mut TestAppContext) {
        let app_state = init_test(cx);
        app_state
            .fs
            .as_fake()
            .insert_tree(
                path!("/dir"),
                json!({
                    "todo.md": "- [ ] Finish work\n"
                }),
            )
            .await;

        cx.update(|cx| {
            open_paths(
                &[PathBuf::from(path!("/dir/todo.md"))],
                app_state.clone(),
                workspace::OpenOptions::default(),
                cx,
            )
        })
        .await
        .unwrap();

        let multi_workspace = cx.update(|cx| cx.windows()[0].downcast::<MultiWorkspace>().unwrap());
        let (preview, editor) = multi_workspace
            .update(cx, |multi_workspace, window, cx| {
                let workspace = multi_workspace.workspace().clone();
                let editor: Entity<Editor> = workspace
                    .read(cx)
                    .active_item(cx)
                    .and_then(|item| item.act_as::<Editor>(cx))
                    .unwrap();

                let preview = workspace.update(cx, |workspace, cx| {
                    let preview = MarkdownPreviewView::create_markdown_view(
                        workspace,
                        editor.clone(),
                        window,
                        cx,
                    );
                    workspace.active_pane().update(cx, |pane, cx| {
                        pane.add_item(Box::new(preview.clone()), true, true, None, window, cx)
                    });
                    preview
                });

                (preview, editor)
            })
            .unwrap();
        cx.run_until_parked();

        multi_workspace
            .update(cx, |_, window, cx| {
                let view_handle = preview.downgrade();
                assert!(preview.read(cx).focus_handle.contains_focused(window, cx));
                MarkdownPreviewView::apply_checkbox_toggle_to_editor(&editor, 2..5, true, cx);
                MarkdownPreviewView::refresh_preview(view_handle, window, cx);
            })
            .unwrap();

        assert_eq!(
            editor.read_with(cx, |editor, cx| editor.buffer().read(cx).read(cx).text()),
            "- [x] Finish work\n"
        );

        let close_task = multi_workspace
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace.workspace().update(cx, |workspace, cx| {
                    workspace.active_pane().update(cx, |pane, cx| {
                        pane.close_item_by_id(preview.entity_id(), SaveIntent::Skip, window, cx)
                    })
                })
            })
            .unwrap();

        close_task.await.unwrap();
        cx.run_until_parked();

        assert_eq!(
            editor.read_with(cx, |editor, cx| editor.buffer().read(cx).read(cx).text()),
            "- [x] Finish work\n"
        );
    }

    #[gpui::test]
    async fn tab_context_menu_offers_show_source(cx: &mut TestAppContext) {
        let (multi_workspace, _editor) =
            open_markdown_file(cx, "note.md", "# Note\n\nBody text\n").await;
        let preview = open_preview_for_active_editor(cx, &multi_workspace);
        cx.run_until_parked();

        multi_workspace
            .update(cx, |_, window, cx| {
                let actions = preview.update(cx, |preview, cx| {
                    preview.tab_extra_context_menu_actions(window, cx)
                });
                let [(label, action)] = actions.as_slice() else {
                    panic!("expected exactly one tab menu action");
                };
                assert_eq!(label.as_ref(), "Show Source");
                assert!(action.as_any().is::<CloseAndReturnToEditor>());
            })
            .unwrap();
    }

    #[gpui::test]
    async fn tab_context_menu_hides_show_source_for_inactive_preview(cx: &mut TestAppContext) {
        let (multi_workspace, editor) =
            open_markdown_file(cx, "note.md", "# Note\n\nBody text\n").await;
        let preview = open_preview_for_active_editor(cx, &multi_workspace);
        cx.run_until_parked();

        multi_workspace
            .update(cx, |multi_workspace, window, cx| {
                let workspace = multi_workspace.workspace().clone();
                workspace.update(cx, |workspace, cx| {
                    assert!(workspace.activate_item(&editor, true, true, window, cx));
                });
                let actions = preview.update(cx, |preview, cx| {
                    preview.tab_extra_context_menu_actions(window, cx)
                });
                assert!(actions.is_empty());
            })
            .unwrap();
    }

    #[gpui::test]
    async fn tab_context_menu_offers_open_preview_only_for_active_editor(cx: &mut TestAppContext) {
        let (multi_workspace, editor) =
            open_markdown_file(cx, "note.md", "# Note\n\nBody text\n").await;
        editor.update(cx, |editor, cx| {
            let buffer = editor.buffer().read(cx).as_singleton().unwrap();
            buffer.update(cx, |buffer, cx| {
                buffer.set_language(
                    Some(Arc::new(language::Language::new(
                        markdown_language_config(),
                        None,
                    ))),
                    cx,
                );
            });
        });

        multi_workspace
            .update(cx, |_, window, cx| {
                let actions = editor.update(cx, |editor, cx| {
                    editor.tab_extra_context_menu_actions(window, cx)
                });
                let [(label, _)] = actions.as_slice() else {
                    panic!("expected exactly one tab menu action");
                };
                assert_eq!(label.as_ref(), "Open Markdown Preview");
            })
            .unwrap();

        open_preview_for_active_editor(cx, &multi_workspace);
        cx.run_until_parked();

        multi_workspace
            .update(cx, |_, window, cx| {
                let actions = editor.update(cx, |editor, cx| {
                    editor.tab_extra_context_menu_actions(window, cx)
                });
                assert!(actions.is_empty());
            })
            .unwrap();
    }

    #[gpui::test]
    async fn splitting_preview_clones_into_new_pane(cx: &mut TestAppContext) {
        let (multi_workspace, _) = open_markdown_file(cx, "note.md", "# Note\n\nBody text\n").await;
        let _preview = open_preview_for_active_editor(cx, &multi_workspace);
        cx.run_until_parked();

        let new_pane = multi_workspace
            .update(cx, |multi_workspace, window, cx| {
                let workspace = multi_workspace.workspace().clone();
                workspace.update(cx, |workspace, cx| {
                    workspace.split_and_clone(
                        workspace.active_pane().clone(),
                        workspace::SplitDirection::Right,
                        window,
                        cx,
                    )
                })
            })
            .unwrap()
            .await
            .unwrap();
        cx.run_until_parked();

        new_pane.read_with(cx, |pane, _| {
            assert!(
                pane.active_item()
                    .and_then(|item| item.downcast::<MarkdownPreviewView>())
                    .is_some(),
                "splitting a preview should clone it into a new pane"
            );
        });
    }

    #[gpui::test]
    async fn go_back_returns_to_preview_after_opening_another_file(cx: &mut TestAppContext) {
        let (project, workspace, multi_workspace) = markdown_workspace(
            cx,
            json!({ "note.md": "# Note\n", "other.txt": "Other File\n" }),
            false,
        )
        .await;

        open_project_file(cx, &project, &multi_workspace, "note.md", None, true).await;
        let preview = open_preview_for_active_editor(cx, &multi_workspace);
        open_project_file(cx, &project, &multi_workspace, "other.txt", None, true).await;

        let go_back = multi_workspace
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace.workspace().update(cx, |workspace, cx| {
                    let pane = workspace.active_pane().downgrade();
                    workspace.go_back(pane, window, cx)
                })
            })
            .unwrap();

        go_back.await.unwrap();
        cx.run_until_parked();

        workspace.read_with(cx, |workspace, cx| {
            assert_eq!(
                workspace.active_item_as::<MarkdownPreviewView>(cx),
                Some(preview.clone()),
                "going back should return to the preview"
            );
        });
    }

    #[gpui::test]
    async fn close_and_return_to_editor_closes_preview_and_focuses_source_editor(
        cx: &mut TestAppContext,
    ) {
        let (multi_workspace, editor) =
            open_markdown_file(cx, "note.md", "# Note\n\nBody text\n").await;
        let preview = open_preview_for_active_editor(cx, &multi_workspace);
        cx.run_until_parked();

        dispatch_close_and_return_to_editor(cx, &multi_workspace, &preview);
        cx.run_until_parked();

        assert_editor_is_active_and_focused(cx, &multi_workspace, &editor);
        assert_no_markdown_preview_items(cx, &multi_workspace);
    }

    #[gpui::test]
    async fn close_and_return_to_editor_reopens_source_editor_when_editor_tab_was_closed(
        cx: &mut TestAppContext,
    ) {
        let (multi_workspace, editor) = open_markdown_file(cx, "note.md", "# Note\n").await;
        let preview = open_preview_for_active_editor(cx, &multi_workspace);
        cx.run_until_parked();

        let close_editor_task = multi_workspace
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace.workspace().update(cx, |workspace, cx| {
                    workspace.active_pane().update(cx, |pane, cx| {
                        pane.close_item_by_id(editor.entity_id(), SaveIntent::Skip, window, cx)
                    })
                })
            })
            .unwrap();
        close_editor_task.await.unwrap();
        cx.run_until_parked();

        multi_workspace
            .update(cx, |multi_workspace, window, cx| {
                let workspace = multi_workspace.workspace().read(cx);
                assert!(
                    preview.read(cx).focus_handle.contains_focused(window, cx),
                    "preview should remain focused after closing the source editor tab"
                );
                assert!(
                    workspace
                        .items_of_type::<Editor>(cx)
                        .all(|open_editor| open_editor != editor),
                    "source editor should no longer be open in any pane"
                );
                assert_eq!(
                    workspace.items_of_type::<MarkdownPreviewView>(cx).count(),
                    1
                );
            })
            .unwrap();

        dispatch_close_and_return_to_editor(cx, &multi_workspace, &preview);
        cx.run_until_parked();

        assert_editor_is_active_and_focused(cx, &multi_workspace, &editor);
        assert_no_markdown_preview_items(cx, &multi_workspace);
    }

    #[gpui::test]
    async fn close_and_return_to_editor_returns_to_source_when_auto_preview_is_enabled(
        cx: &mut TestAppContext,
    ) {
        let (project, workspace, multi_workspace) =
            markdown_workspace(cx, json!({ "note.md": "# Note\n" }), false).await;
        cx.update(|cx| set_auto_preview_enabled(cx, true));

        let editor = open_project_file(cx, &project, &multi_workspace, "note.md", None, true)
            .await
            .downcast::<Editor>()
            .unwrap();
        let split_view = active_split_view(cx, &workspace);

        dispatch_close_and_return_to_editor(cx, &multi_workspace, &split_view);
        cx.run_until_parked();

        assert_editor_is_active_and_focused(cx, &multi_workspace, &editor);
        assert_no_markdown_preview_items(cx, &multi_workspace);
    }

    #[gpui::test]
    async fn show_source_keeps_unsaved_edits_from_a_preview_tab(cx: &mut TestAppContext) {
        let (project, workspace, multi_workspace) =
            markdown_workspace(cx, json!({ "note.md": "- [ ] task\n" }), false).await;
        cx.update(|cx| set_auto_preview_enabled(cx, true));
        let editor = open_project_file(cx, &project, &multi_workspace, "note.md", None, true)
            .await
            .downcast::<Editor>()
            .unwrap();
        let split_view = active_split_view(cx, &workspace);
        cx.update(|cx| {
            MarkdownPreviewView::apply_checkbox_toggle_to_editor(&editor, 2..5, true, cx)
        });

        dispatch_close_and_return_to_editor(cx, &multi_workspace, &split_view);
        cx.run_until_parked();

        assert_editor_is_active_and_focused(cx, &multi_workspace, &editor);
        let text = editor.read_with(cx, |editor, cx| {
            editor.buffer().read(cx).snapshot(cx).text()
        });
        assert_eq!(
            text, "- [x] task\n",
            "Show Source must not discard the edit"
        );
        cx.update(|cx| assert!(ItemHandle::is_dirty(&editor, cx)));
    }

    #[gpui::test]
    async fn preview_serialized_path_updates_when_source_file_is_renamed(cx: &mut TestAppContext) {
        let app_state = init_test(cx);
        app_state
            .fs
            .as_fake()
            .insert_tree(
                path!("/dir"),
                json!({
                    "todo.md": "![image](image.png)\n",
                    "subdir": {},
                }),
            )
            .await;

        cx.update(|cx| {
            open_paths(
                &[PathBuf::from(path!("/dir"))],
                app_state.clone(),
                workspace::OpenOptions::default(),
                cx,
            )
        })
        .await
        .unwrap();

        let multi_workspace = cx.update(|cx| cx.windows()[0].downcast::<MultiWorkspace>().unwrap());
        let open_task = multi_workspace
            .update(cx, |multi_workspace, window, cx| {
                let workspace = multi_workspace.workspace().clone();
                workspace.update(cx, |workspace, cx| {
                    let worktree_id = workspace
                        .project()
                        .read(cx)
                        .worktrees(cx)
                        .next()
                        .unwrap()
                        .read(cx)
                        .id();
                    workspace.open_path((worktree_id, rel_path("todo.md")), None, true, window, cx)
                })
            })
            .unwrap();
        open_task.await.unwrap();
        cx.run_until_parked();

        let (preview, project, workspace_id) = multi_workspace
            .update(cx, |multi_workspace, window, cx| {
                let workspace = multi_workspace.workspace().clone();
                workspace.update(cx, |workspace, cx| {
                    workspace.set_random_database_id();
                    let workspace_id = workspace.database_id().unwrap();
                    let project = workspace.project().clone();
                    let editor: Entity<Editor> = workspace
                        .active_item(cx)
                        .and_then(|item| item.act_as::<Editor>(cx))
                        .unwrap();
                    let preview =
                        MarkdownPreviewView::create_markdown_view(workspace, editor, window, cx);
                    workspace.active_pane().update(cx, |pane, cx| {
                        pane.add_item(Box::new(preview.clone()), true, true, None, window, cx)
                    });
                    (preview, project, workspace_id)
                })
            })
            .unwrap();
        let workspace_serialization_tasks = multi_workspace
            .update(cx, |multi_workspace, window, cx| {
                let workspace = multi_workspace.workspace().clone();
                vec![workspace.update(cx, |workspace, cx| {
                    workspace.flush_serialization(window, cx)
                })]
            })
            .unwrap();
        for task in workspace_serialization_tasks {
            task.await;
        }
        insert_workspace_row(workspace_id).await;

        let serialize_task = multi_workspace
            .update(cx, |multi_workspace, window, cx| {
                let workspace = multi_workspace.workspace().clone();
                workspace.update(cx, |workspace, cx| {
                    preview
                        .update(cx, |preview, cx| {
                            preview.serialize(workspace, cx.entity_id().as_u64(), false, window, cx)
                        })
                        .unwrap()
                })
            })
            .unwrap();
        serialize_task.await.unwrap();

        assert_eq!(
            saved_preview_path(cx, preview.entity_id().as_u64(), workspace_id),
            PathBuf::from(path!("/dir/todo.md"))
        );

        let (entry_id, worktree_id, destination_path) = preview.read_with(cx, |preview, cx| {
            let editor = &preview.active_editor.as_ref().unwrap().editor;
            let buffer = editor.read(cx).buffer().read(cx).as_singleton().unwrap();
            let buffer = buffer.read(cx);
            let file = buffer.file().unwrap();
            let worktree_id = file.worktree_id(cx);
            let source_path = file.path();
            let mut destination_path = source_path.to_rel_path_buf();
            destination_path.pop();
            destination_path.push(rel_path("subdir/renamed.md"));
            let worktree = project.read(cx).worktree_for_id(worktree_id, cx).unwrap();
            let entry_id = worktree.read(cx).entry_for_path(source_path).unwrap().id;
            (
                entry_id,
                worktree_id,
                destination_path.as_rel_path().into_arc(),
            )
        });
        project
            .update(cx, |project, cx| {
                project.rename_entry(entry_id, (worktree_id, destination_path).into(), cx)
            })
            .await
            .unwrap();
        wait_for_preview_serialization(cx).await;

        assert_eq!(
            preview.read_with(cx, |preview, _| preview.base_directory.clone()),
            Some(PathBuf::from(path!("/dir/subdir")))
        );
        assert_eq!(
            saved_preview_path(cx, preview.entity_id().as_u64(), workspace_id),
            PathBuf::from(path!("/dir/subdir/renamed.md"))
        );
    }

    #[gpui::test]
    async fn follow_preview_serialized_path_updates_when_followed_editor_changes(
        cx: &mut TestAppContext,
    ) {
        let app_state = init_test(cx);
        app_state
            .fs
            .as_fake()
            .insert_tree(
                path!("/dir"),
                json!({
                    "a.md": "# A\n",
                    "b.md": "# B\n",
                }),
            )
            .await;

        cx.update(|cx| {
            open_paths(
                &[PathBuf::from(path!("/dir"))],
                app_state.clone(),
                workspace::OpenOptions::default(),
                cx,
            )
        })
        .await
        .unwrap();

        let multi_workspace = cx.update(|cx| cx.windows()[0].downcast::<MultiWorkspace>().unwrap());
        let worktree_id = multi_workspace
            .update(cx, |multi_workspace, _, cx| {
                multi_workspace
                    .workspace()
                    .read(cx)
                    .project()
                    .read(cx)
                    .worktrees(cx)
                    .next()
                    .unwrap()
                    .read(cx)
                    .id()
            })
            .unwrap();

        let open_task = multi_workspace
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace.workspace().update(cx, |workspace, cx| {
                    workspace.open_path((worktree_id, rel_path("a.md")), None, true, window, cx)
                })
            })
            .unwrap();
        let opened_item = open_task.await.unwrap();
        cx.run_until_parked();
        let editor_a = cx.update(|cx| opened_item.act_as::<Editor>(cx).unwrap());

        let open_task = multi_workspace
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace.workspace().update(cx, |workspace, cx| {
                    workspace.open_path((worktree_id, rel_path("b.md")), None, true, window, cx)
                })
            })
            .unwrap();
        let opened_item = open_task.await.unwrap();
        cx.run_until_parked();
        let editor_b = cx.update(|cx| opened_item.act_as::<Editor>(cx).unwrap());
        let editor_b_path = editor_source_path(cx, &editor_b);
        assert_eq!(editor_b_path.as_ref(), rel_path("b.md"));

        let (preview, workspace_id) = multi_workspace
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace.workspace().update(cx, |workspace, cx| {
                    workspace.set_random_database_id();
                    let workspace_id = workspace.database_id().unwrap();
                    let preview = MarkdownPreviewView::create_following_markdown_view(
                        workspace, editor_a, window, cx,
                    );
                    workspace.active_pane().update(cx, |pane, cx| {
                        pane.add_item(Box::new(preview.clone()), true, true, None, window, cx)
                    });
                    (preview, workspace_id)
                })
            })
            .unwrap();
        let workspace_serialization_tasks = multi_workspace
            .update(cx, |multi_workspace, window, cx| {
                let workspace = multi_workspace.workspace().clone();
                vec![workspace.update(cx, |workspace, cx| {
                    workspace.flush_serialization(window, cx)
                })]
            })
            .unwrap();
        for task in workspace_serialization_tasks {
            task.await;
        }
        insert_workspace_row(workspace_id).await;
        wait_for_preview_serialization(cx).await;

        let serialize_task = multi_workspace
            .update(cx, |multi_workspace, window, cx| {
                let workspace = multi_workspace.workspace().clone();
                workspace.update(cx, |workspace, cx| {
                    preview
                        .update(cx, |preview, cx| {
                            preview.serialize(workspace, cx.entity_id().as_u64(), false, window, cx)
                        })
                        .unwrap()
                })
            })
            .unwrap();
        serialize_task.await.unwrap();

        assert_eq!(
            saved_preview_path(cx, preview.entity_id().as_u64(), workspace_id),
            PathBuf::from(path!("/dir/a.md"))
        );

        multi_workspace
            .update(cx, |_, window, cx| {
                preview.update(cx, |preview, cx| {
                    preview.set_editor(editor_b, window, cx);
                });
            })
            .unwrap();
        wait_for_preview_serialization(cx).await;

        let followed_path = preview_source_path(cx, &preview);
        assert_eq!(followed_path.as_ref(), rel_path("b.md"));

        assert_eq!(
            saved_preview_path(cx, preview.entity_id().as_u64(), workspace_id),
            PathBuf::from(path!("/dir/b.md")),
            "a Follow preview should persist the source editor it most recently followed"
        );
    }

    #[gpui::test]
    async fn default_preview_stays_bound_to_invoking_editor_across_splits(cx: &mut TestAppContext) {
        let app_state = init_test(cx);
        app_state
            .fs
            .as_fake()
            .insert_tree(
                path!("/dir"),
                json!({
                    "todo.md": "- [ ] Finish work\n"
                }),
            )
            .await;

        cx.update(|cx| {
            open_paths(
                &[PathBuf::from(path!("/dir/todo.md"))],
                app_state.clone(),
                workspace::OpenOptions::default(),
                cx,
            )
        })
        .await
        .unwrap();

        let multi_workspace = cx.update(|cx| cx.windows()[0].downcast::<MultiWorkspace>().unwrap());
        let (preview, second_editor) = multi_workspace
            .update(cx, |multi_workspace, window, cx| {
                let workspace = multi_workspace.workspace().clone();
                workspace.update(cx, |workspace, cx| {
                    let first_editor: Entity<Editor> = workspace
                        .active_item(cx)
                        .and_then(|item| item.act_as::<Editor>(cx))
                        .unwrap();
                    let buffer = first_editor
                        .read(cx)
                        .buffer()
                        .read(cx)
                        .as_singleton()
                        .unwrap();
                    let project = workspace.project().clone();

                    let second_editor =
                        cx.new(|cx| Editor::for_buffer(buffer, Some(project), window, cx));
                    let new_pane = workspace.split_pane(
                        workspace.active_pane().clone(),
                        workspace::SplitDirection::Right,
                        window,
                        cx,
                    );
                    new_pane.update(cx, |pane, cx| {
                        pane.add_item(
                            Box::new(second_editor.clone()),
                            true,
                            true,
                            None,
                            window,
                            cx,
                        )
                    });

                    let preview = MarkdownPreviewView::create_markdown_view(
                        workspace,
                        second_editor.clone(),
                        window,
                        cx,
                    );
                    new_pane.update(cx, |pane, cx| {
                        pane.add_item(Box::new(preview.clone()), true, true, None, window, cx)
                    });
                    (preview, second_editor)
                })
            })
            .unwrap();
        cx.run_until_parked();

        let bound_editor = preview.read_with(cx, |preview, _| {
            preview.active_editor.as_ref().unwrap().editor.clone()
        });
        assert_eq!(
            bound_editor, second_editor,
            "a Default preview must stay bound to the editor it was opened from, not another \
             editor that happens to share the same buffer in a different split"
        );
    }

    async fn open_markdown_file(
        cx: &mut TestAppContext,
        file_name: &str,
        contents: &str,
    ) -> (WindowHandle<MultiWorkspace>, Entity<Editor>) {
        let app_state = init_test(cx);
        let mut entries = serde_json::Map::new();
        entries.insert(file_name.to_string(), json!(contents));
        app_state
            .fs
            .as_fake()
            .insert_tree(path!("/dir"), serde_json::Value::Object(entries))
            .await;

        cx.update(|cx| {
            open_paths(
                &[PathBuf::from(path!("/dir")).join(file_name)],
                app_state.clone(),
                workspace::OpenOptions::default(),
                cx,
            )
        })
        .await
        .unwrap();

        let multi_workspace = cx.update(|cx| cx.windows()[0].downcast::<MultiWorkspace>().unwrap());
        let editor = multi_workspace
            .update(cx, |multi_workspace, _, cx| {
                multi_workspace
                    .workspace()
                    .read(cx)
                    .active_item_as::<Editor>(cx)
                    .unwrap()
            })
            .unwrap();
        (multi_workspace, editor)
    }

    fn open_preview_for_active_editor(
        cx: &mut TestAppContext,
        multi_workspace: &WindowHandle<MultiWorkspace>,
    ) -> Entity<MarkdownPreviewView> {
        multi_workspace
            .update(cx, |multi_workspace, window, cx| {
                let workspace = multi_workspace.workspace().clone();
                workspace.update(cx, |workspace, cx| {
                    let editor: Entity<Editor> = workspace.active_item_as(cx).unwrap();
                    let preview =
                        MarkdownPreviewView::create_markdown_view(workspace, editor, window, cx);
                    workspace.active_pane().update(cx, |pane, cx| {
                        pane.add_item(Box::new(preview.clone()), true, true, None, window, cx)
                    });
                    preview
                })
            })
            .unwrap()
    }

    fn dispatch_close_and_return_to_editor(
        cx: &mut TestAppContext,
        multi_workspace: &WindowHandle<MultiWorkspace>,
        preview: &impl Focusable,
    ) {
        multi_workspace
            .update(cx, |_, window, cx| {
                assert!(
                    preview.focus_handle(cx).contains_focused(window, cx),
                    "preview must be focused for the keyboard action to dispatch to it"
                );
                window.dispatch_action(Box::new(CloseAndReturnToEditor), cx);
            })
            .unwrap();
    }

    fn assert_editor_is_active_and_focused(
        cx: &mut TestAppContext,
        multi_workspace: &WindowHandle<MultiWorkspace>,
        expected_editor: &Entity<Editor>,
    ) {
        multi_workspace
            .update(cx, |multi_workspace, window, cx| {
                let workspace = multi_workspace.workspace().read(cx);
                let active_editor = workspace.active_item_as::<Editor>(cx).unwrap();
                assert_eq!(active_editor, *expected_editor);
                assert!(
                    expected_editor
                        .read(cx)
                        .focus_handle(cx)
                        .contains_focused(window, cx),
                    "source editor should be focused"
                );
            })
            .unwrap();
    }

    fn assert_no_markdown_preview_items(
        cx: &mut TestAppContext,
        multi_workspace: &WindowHandle<MultiWorkspace>,
    ) {
        multi_workspace
            .update(cx, |multi_workspace, _, cx| {
                let workspace = multi_workspace.workspace().read(cx);
                assert_eq!(
                    workspace.items_of_type::<MarkdownPreviewView>(cx).count(),
                    0
                );
                assert_eq!(workspace.items_of_type::<MarkdownSplitView>(cx).count(), 0);
            })
            .unwrap();
    }

    #[gpui::test]
    async fn opens_markdown_files_in_preview_when_enabled(cx: &mut TestAppContext) {
        let (project, workspace, multi_workspace) = markdown_workspace(
            cx,
            json!({
                "automatic.md": "# Automatic\n",
                "plain.txt": "Plain text\n",
                "targeted.md": "# Targeted\n",
            }),
            false,
        )
        .await;
        cx.update(|cx| set_auto_preview_enabled(cx, true));

        let automatic_editor =
            open_project_file(cx, &project, &multi_workspace, "automatic.md", None, true).await;
        cx.update(|cx| {
            assert_editor_replaced_by_preview(workspace.read(cx), automatic_editor.as_ref(), cx);
        });

        open_project_file(cx, &project, &multi_workspace, "plain.txt", None, true).await;
        cx.update(|cx| {
            assert!(workspace.read(cx).active_item_as::<Editor>(cx).is_some());
        });

        let (target_pane, other_pane) = multi_workspace
            .update(cx, |_, window, cx| {
                workspace.update(cx, |workspace, cx| {
                    let target_pane = workspace.active_pane().clone();
                    let other_pane = workspace.split_pane(
                        target_pane.clone(),
                        workspace::SplitDirection::Right,
                        window,
                        cx,
                    );
                    (target_pane, other_pane)
                })
            })
            .unwrap();
        open_project_file(
            cx,
            &project,
            &multi_workspace,
            "targeted.md",
            Some(&target_pane),
            true,
        )
        .await;
        let split_view = cx.update(|cx| {
            let split_view = target_pane
                .read(cx)
                .active_item()
                .and_then(|item| item.downcast::<MarkdownSplitView>())
                .expect("the Markdown preview should open in the requested pane");
            assert_eq!(
                other_pane
                    .read(cx)
                    .items_of_type::<MarkdownSplitView>()
                    .count(),
                0
            );
            split_view
        });
        assert_eq!(
            split_view_source_path(cx, &split_view).as_ref(),
            rel_path("targeted.md")
        );
    }

    #[gpui::test]
    async fn first_markdown_file_opens_in_preview_before_language_load(cx: &mut TestAppContext) {
        let (project, workspace, multi_workspace) =
            markdown_workspace(cx, json!({ "note.md": "# Note\n" }), true).await;
        cx.update(|cx| set_auto_preview_enabled(cx, true));
        let editor = open_project_file(cx, &project, &multi_workspace, "note.md", None, true)
            .await
            .downcast::<Editor>()
            .unwrap();

        cx.update(|cx| {
            assert_editor_replaced_by_preview(workspace.read(cx), &editor, cx);
        });
    }

    #[gpui::test]
    async fn workspace_restore_does_not_auto_preview_markdown(cx: &mut TestAppContext) {
        let (project, workspace, multi_workspace) = markdown_workspace(
            cx,
            json!({
                "restored.md": "# Restored\n",
                "ordinary.md": "# Ordinary\n",
            }),
            false,
        )
        .await;
        cx.update(|cx| set_auto_preview_enabled(cx, true));
        workspace.update(cx, |workspace, _| workspace.set_restoring_workspace(true));

        let restored_editor =
            open_project_file(cx, &project, &multi_workspace, "restored.md", None, true)
                .await
                .downcast::<Editor>()
                .unwrap();
        cx.update(|cx| {
            let workspace = workspace.read(cx);
            assert_eq!(
                workspace.active_item_as::<Editor>(cx),
                Some(restored_editor)
            );
            assert_eq!(workspace.items_of_type::<MarkdownSplitView>(cx).count(), 0);
        });

        workspace.update(cx, |workspace, _| workspace.set_restoring_workspace(false));
        let ordinary_editor =
            open_project_file(cx, &project, &multi_workspace, "ordinary.md", None, true)
                .await
                .downcast::<Editor>()
                .unwrap();
        cx.update(|cx| {
            assert_editor_replaced_by_preview(workspace.read(cx), &ordinary_editor, cx);
        });
    }

    #[gpui::test]
    async fn automatic_preview_takes_over_a_dirty_editor(cx: &mut TestAppContext) {
        let (project, workspace, multi_workspace) =
            markdown_workspace(cx, json!({ "note.md": "# Note\n" }), false).await;
        let editor = open_project_file(cx, &project, &multi_workspace, "note.md", None, true)
            .await
            .downcast::<Editor>()
            .unwrap();
        editor.update(cx, |editor, cx| {
            let end = editor.buffer().read(cx).snapshot(cx).len();
            editor.edit([(end..end, "dirty\n")], cx);
        });
        cx.update(|cx| set_auto_preview_enabled(cx, true));

        multi_workspace
            .update(cx, |_, window, cx| {
                workspace.update(cx, |workspace, cx| {
                    workspace.add_item_to_active_pane(
                        Box::new(editor.clone()),
                        None,
                        true,
                        window,
                        cx,
                    );
                });
            })
            .unwrap();
        cx.run_until_parked();

        let split_view = active_split_view(cx, &workspace);
        cx.update(|cx| {
            assert_editor_replaced_by_preview(workspace.read(cx), &editor, cx);
            assert_eq!(split_view.read(cx).editor(), &editor);
            assert!(
                ItemHandle::is_dirty(&split_view, cx),
                "the split view owns the editor, so it must report the unsaved edit"
            );
        });
    }

    #[gpui::test]
    async fn automatic_preview_reports_unsaved_source_edits(cx: &mut TestAppContext) {
        let (project, workspace, multi_workspace) =
            markdown_workspace(cx, json!({ "note.md": "- [ ] task\n" }), false).await;
        cx.update(|cx| set_auto_preview_enabled(cx, true));
        let editor = open_project_file(cx, &project, &multi_workspace, "note.md", None, true).await;
        let split_view = workspace.read_with(cx, |workspace, cx| {
            assert_editor_replaced_by_preview(workspace, editor.as_ref(), cx);
            workspace.active_item_as::<MarkdownSplitView>(cx).unwrap()
        });

        cx.update(|cx| assert!(!ItemHandle::is_dirty(&split_view, cx)));

        let source_editor = split_view.read_with(cx, |split_view, _| split_view.editor().clone());
        cx.update(|cx| {
            MarkdownPreviewView::apply_checkbox_toggle_to_editor(&source_editor, 2..5, true, cx)
        });
        cx.run_until_parked();

        cx.update(|cx| {
            assert!(
                ItemHandle::is_dirty(&split_view, cx),
                "save all and close prompts only see the split view, so it must report the edit"
            );
        });
    }

    #[gpui::test]
    async fn project_panel_preview_switches_the_files_tab_to_its_preview(cx: &mut TestAppContext) {
        let (project, workspace, multi_workspace) = markdown_workspace(
            cx,
            json!({ "alone.md": "- [ ] task\n", "opened.md": "- [ ] task\n" }),
            false,
        )
        .await;
        let opened_editor =
            open_project_file(cx, &project, &multi_workspace, "opened.md", None, true)
                .await
                .downcast::<Editor>()
                .unwrap();

        for path in ["alone.md", "opened.md"] {
            multi_workspace
                .update(cx, |_, window, cx| {
                    workspace.update(cx, |workspace, cx| {
                        let project_path = test_project_path(&project, path, cx);
                        MarkdownPreviewView::open_for_project_path(
                            project_path,
                            workspace,
                            window,
                            cx,
                        );
                    });
                })
                .unwrap();
            cx.run_until_parked();
        }

        let split_views = workspace.read_with(cx, |workspace, cx| {
            assert_eq!(workspace.items_of_type::<Editor>(cx).count(), 0);
            assert_eq!(
                workspace.items_of_type::<MarkdownPreviewView>(cx).count(),
                0
            );
            workspace
                .items_of_type::<MarkdownSplitView>(cx)
                .collect::<Vec<_>>()
        });
        assert_eq!(split_views.len(), 2);
        assert!(
            split_views.iter().any(|split_view| split_view
                .read_with(cx, |split_view, _| split_view.editor() == &opened_editor)),
            "the open editor tab must turn into the preview rather than gain a second owner"
        );
        for split_view in split_views {
            let editor = split_view.read_with(cx, |split_view, _| {
                assert_eq!(split_view.layout(), MarkdownLayout::Preview);
                split_view.editor().clone()
            });
            cx.update(|cx| {
                MarkdownPreviewView::apply_checkbox_toggle_to_editor(&editor, 2..5, true, cx)
            });
            cx.update(|cx| assert!(ItemHandle::is_dirty(&split_view, cx)));
        }
    }

    #[gpui::test]
    async fn split_of_a_split_view_keeps_reporting_unsaved_edits(cx: &mut TestAppContext) {
        let (project, workspace, multi_workspace) =
            markdown_workspace(cx, json!({ "note.md": "- [ ] task\n" }), false).await;
        cx.update(|cx| set_auto_preview_enabled(cx, true));
        open_project_file(cx, &project, &multi_workspace, "note.md", None, true).await;
        let split_view = active_split_view(cx, &workspace);

        let split = multi_workspace
            .update(cx, |_, window, cx| {
                split_view.update(cx, |split_view, cx| {
                    split_view.clone_on_split(None, window, cx)
                })
            })
            .unwrap()
            .await
            .expect("split views can be split");
        let source_editor = split.read_with(cx, |split, _| {
            assert_eq!(split.layout(), MarkdownLayout::Preview);
            split.editor().clone()
        });
        cx.update(|cx| {
            MarkdownPreviewView::apply_checkbox_toggle_to_editor(&source_editor, 2..5, true, cx)
        });

        cx.update(|cx| {
            assert!(ItemHandle::is_dirty(&split, cx));
            assert!(ItemHandle::is_dirty(&split_view, cx));
        });
    }

    #[gpui::test]
    async fn unchecks_uppercase_task_markers(cx: &mut TestAppContext) {
        let (project, _workspace, multi_workspace) =
            markdown_workspace(cx, json!({ "note.md": "- [X] task\n" }), false).await;
        let editor = open_project_file(cx, &project, &multi_workspace, "note.md", None, true)
            .await
            .downcast::<Editor>()
            .unwrap();

        cx.update(|cx| {
            MarkdownPreviewView::apply_checkbox_toggle_to_editor(&editor, 2..5, false, cx)
        });

        let text = editor.read_with(cx, |editor, cx| {
            editor.buffer().read(cx).snapshot(cx).text()
        });
        assert_eq!(text, "- [ ] task\n");
    }

    #[gpui::test]
    async fn heading_links_keep_focus_in_a_preview_tab(cx: &mut TestAppContext) {
        let (project, workspace, multi_workspace) = markdown_workspace(
            cx,
            json!({ "note.md": "[Jump](#section)\n\n# Section\n\nBody\n" }),
            false,
        )
        .await;
        cx.update(|cx| set_auto_preview_enabled(cx, true));
        open_project_file(cx, &project, &multi_workspace, "note.md", None, true).await;
        let preview = active_split_view(cx, &workspace)
            .read_with(cx, |split_view, _| split_view.preview().clone());

        multi_workspace
            .update(cx, |_, window, cx| {
                window.focus(&preview.focus_handle(cx), cx);
                let workspace = workspace.downgrade();
                handle_url_click(
                    "#section".into(),
                    &preview.downgrade(),
                    None,
                    &workspace,
                    window,
                    cx,
                );
            })
            .unwrap();
        cx.run_until_parked();

        multi_workspace
            .update(cx, |_, window, cx| {
                assert!(
                    preview.focus_handle(cx).contains_focused(window, cx),
                    "the hidden source editor must not take focus from the preview"
                );
            })
            .unwrap();
    }

    #[gpui::test]
    async fn reloading_a_split_view_discards_source_edits(cx: &mut TestAppContext) {
        let (project, workspace, multi_workspace) =
            markdown_workspace(cx, json!({ "note.md": "- [ ] task\n" }), false).await;
        cx.update(|cx| set_auto_preview_enabled(cx, true));
        open_project_file(cx, &project, &multi_workspace, "note.md", None, true).await;
        let split_view = active_split_view(cx, &workspace);
        let source_editor = split_view.read_with(cx, |split_view, _| split_view.editor().clone());
        cx.update(|cx| {
            MarkdownPreviewView::apply_checkbox_toggle_to_editor(&source_editor, 2..5, true, cx)
        });
        cx.update(|cx| assert!(ItemHandle::is_dirty(&split_view, cx)));

        let reload = multi_workspace
            .update(cx, |_, window, cx| {
                split_view.update(cx, |split_view, cx| {
                    split_view.reload(project.clone(), window, cx)
                })
            })
            .unwrap();
        reload.await.unwrap();
        cx.run_until_parked();

        let text = source_editor.read_with(cx, |editor, cx| {
            editor.buffer().read(cx).snapshot(cx).text()
        });
        assert_eq!(text, "- [ ] task\n");
        cx.update(|cx| assert!(!ItemHandle::is_dirty(&split_view, cx)));
    }

    #[gpui::test]
    async fn split_view_restores_unsaved_edits_and_selections_after_closing(
        cx: &mut TestAppContext,
    ) {
        let (project, workspace, multi_workspace) =
            markdown_workspace(cx, json!({ "note.md": "- [ ] task\n" }), false).await;
        cx.update(|cx| set_auto_preview_enabled(cx, true));
        open_project_file(cx, &project, &multi_workspace, "note.md", None, true).await;
        let split_view = active_split_view(cx, &workspace);
        let editor = split_view.read_with(cx, |split_view, _| split_view.editor().clone());
        cx.update(|cx| {
            MarkdownPreviewView::apply_checkbox_toggle_to_editor(&editor, 2..5, true, cx)
        });
        multi_workspace
            .update(cx, |_, window, cx| {
                editor.update(cx, |editor, cx| {
                    editor.change_selections(SelectionEffects::no_scroll(), window, cx, |s| {
                        s.select_ranges([MultiBufferOffset(6)..MultiBufferOffset(10)])
                    })
                })
            })
            .unwrap();

        let workspace_id = workspace.update(cx, |workspace, _| {
            workspace.set_random_database_id();
            workspace.database_id().unwrap()
        });
        insert_split_view_workspace_row(workspace_id).await;
        let item_id = split_view.entity_id().as_u64();
        let serialize = multi_workspace
            .update(cx, |_, window, cx| {
                workspace.update(cx, |workspace, cx| {
                    split_view.update(cx, |split_view, cx| {
                        split_view.serialize(workspace, item_id, true, window, cx)
                    })
                })
            })
            .unwrap()
            .expect("unsaved edits are restored, so closing must not prompt for them");
        serialize.await.unwrap();

        // Discard the edit on disk's side, as quitting would, then restore the tab.
        let reload = multi_workspace
            .update(cx, |_, window, cx| {
                split_view.update(cx, |split_view, cx| {
                    split_view.reload(project.clone(), window, cx)
                })
            })
            .unwrap();
        reload.await.unwrap();

        let restored = multi_workspace
            .update(cx, |_, window, cx| {
                MarkdownSplitView::deserialize(
                    project.clone(),
                    workspace.downgrade(),
                    workspace_id,
                    item_id,
                    window,
                    cx,
                )
            })
            .unwrap()
            .await
            .unwrap();
        cx.run_until_parked();

        restored.read_with(cx, |restored, cx| {
            assert_eq!(restored.layout(), MarkdownLayout::Preview);
            assert_eq!(
                restored
                    .editor()
                    .read(cx)
                    .buffer()
                    .read(cx)
                    .snapshot(cx)
                    .text(),
                "- [x] task\n"
            );
            assert!(Item::is_dirty(restored, cx));
        });
        let restored_editor = restored.read_with(cx, |restored, _| restored.editor().clone());
        let restored_selections = restored_editor.update(cx, |editor, cx| {
            let snapshot = editor.display_snapshot(cx);
            editor
                .selections
                .all::<MultiBufferOffset>(&snapshot)
                .into_iter()
                .map(|selection| selection.range())
                .collect::<Vec<_>>()
        });
        assert_eq!(
            restored_selections,
            [MultiBufferOffset(6)..MultiBufferOffset(10)]
        );
    }

    #[gpui::test]
    async fn preview_saves_checkbox_toggles_when_no_tab_shows_the_source(cx: &mut TestAppContext) {
        let (project, workspace, multi_workspace) = markdown_workspace(
            cx,
            json!({ "first.md": "- [ ] task\n", "second.md": "# Second\n" }),
            false,
        )
        .await;
        let second_editor =
            open_project_file(cx, &project, &multi_workspace, "second.md", None, true)
                .await
                .downcast::<Editor>()
                .unwrap();
        let first_path = cx.update(|cx| test_project_path(&project, "first.md", cx));
        let first_buffer = project
            .update(cx, |project, cx| project.open_buffer(first_path, cx))
            .await
            .unwrap();

        // Like a restored following preview: it has an editor no tab shows.
        let preview = multi_workspace
            .update(cx, |_, window, cx| {
                workspace.update(cx, |workspace, cx| {
                    let first_editor = cx.new(|cx| {
                        Editor::for_buffer(first_buffer.clone(), Some(project.clone()), window, cx)
                    });
                    let preview = MarkdownPreviewView::new(
                        super::MarkdownPreviewMode::Follow,
                        first_editor,
                        workspace.weak_handle(),
                        workspace.project().read(cx).languages().clone(),
                        window,
                        cx,
                    );
                    workspace.add_item_to_active_pane(
                        Box::new(preview.clone()),
                        None,
                        true,
                        window,
                        cx,
                    );
                    preview
                })
            })
            .unwrap();
        cx.run_until_parked();

        let first_editor = preview.read_with(cx, |preview, _| {
            preview.active_editor.as_ref().unwrap().editor.clone()
        });
        multi_workspace
            .update(cx, |_, window, cx| {
                MarkdownPreviewView::toggle_checkbox(
                    preview.downgrade(),
                    &first_editor,
                    2..5,
                    true,
                    window,
                    cx,
                );
            })
            .unwrap();
        cx.run_until_parked();

        // Following another file must not lose the toggle, which no tab would report.
        multi_workspace
            .update(cx, |_, window, cx| {
                workspace.update(cx, |workspace, cx| {
                    workspace.activate_item(&second_editor, true, true, window, cx);
                })
            })
            .unwrap();
        cx.run_until_parked();

        let fs = project.read_with(cx, |project, _| project.fs().clone());
        assert_eq!(
            fs.load(path!("/project/first.md").as_ref()).await.unwrap(),
            "- [x] task\n"
        );
        cx.update(|cx| assert!(!first_buffer.read(cx).is_dirty()));
    }

    #[gpui::test]
    async fn preview_next_to_its_editor_leaves_unsaved_edits_to_the_editor(
        cx: &mut TestAppContext,
    ) {
        let (project, workspace, multi_workspace) =
            markdown_workspace(cx, json!({ "note.md": "- [ ] task\n" }), false).await;
        let editor = open_project_file(cx, &project, &multi_workspace, "note.md", None, true)
            .await
            .downcast::<Editor>()
            .unwrap();
        let preview = multi_workspace
            .update(cx, |_, window, cx| {
                workspace.update(cx, |workspace, cx| {
                    let preview = MarkdownPreviewView::create_markdown_view(
                        workspace,
                        editor.clone(),
                        window,
                        cx,
                    );
                    workspace.add_item_to_active_pane(
                        Box::new(preview.clone()),
                        None,
                        true,
                        window,
                        cx,
                    );
                    preview
                })
            })
            .unwrap();
        editor.update(cx, |editor, cx| {
            editor.edit(
                [(
                    editor::MultiBufferOffset(3)..editor::MultiBufferOffset(4),
                    "x",
                )],
                cx,
            );
        });
        cx.run_until_parked();

        cx.update(|cx| {
            assert!(ItemHandle::is_dirty(&editor, cx));
            assert!(
                !ItemHandle::is_dirty(&preview, cx),
                "the editor tab owns the unsaved edit, so closing the preview must not prompt"
            );
        });
    }

    #[gpui::test]
    async fn automatic_previews_preserve_temporary_tab_semantics(cx: &mut TestAppContext) {
        let (project, workspace, multi_workspace) = markdown_workspace(
            cx,
            json!({
                "first.md": "# First\n",
                "second.md": "# Second\n",
            }),
            false,
        )
        .await;
        cx.update(|cx| set_auto_preview_enabled(cx, true));

        for path in ["first.md", "second.md"] {
            let open = multi_workspace
                .update(cx, |_, window, cx| {
                    workspace.update(cx, |workspace, cx| {
                        workspace.open_path_preview(
                            test_project_path(&project, path, cx),
                            None,
                            true,
                            true,
                            true,
                            window,
                            cx,
                        )
                    })
                })
                .unwrap();
            open.await.unwrap();
            cx.run_until_parked();

            let split_view = cx.update(|cx| {
                let pane = workspace.read(cx).active_pane().read(cx);
                let split_view = pane
                    .preview_item()
                    .and_then(|item| item.downcast::<MarkdownSplitView>())
                    .unwrap();
                assert_eq!(pane.items_len(), 1);
                split_view
            });
            assert_eq!(
                split_view_source_path(cx, &split_view).as_ref(),
                rel_path(path)
            );
        }
    }

    #[gpui::test]
    async fn automatic_preview_preserves_pane_activation_and_focus(cx: &mut TestAppContext) {
        let (project, workspace, multi_workspace) = markdown_workspace(
            cx,
            json!({
                "plain.txt": "Plain\n",
                "active.md": "# Active\n",
                "background.md": "# Background\n",
            }),
            false,
        )
        .await;
        cx.update(|cx| set_auto_preview_enabled(cx, true));
        let plain_editor =
            open_project_file(cx, &project, &multi_workspace, "plain.txt", None, true)
                .await
                .downcast::<Editor>()
                .unwrap();

        let (plain_pane, target_pane) = multi_workspace
            .update(cx, |_, window, cx| {
                workspace.update(cx, |workspace, cx| {
                    let plain_pane = workspace.active_pane().clone();
                    let target_pane = workspace.split_pane(
                        plain_pane.clone(),
                        workspace::SplitDirection::Right,
                        window,
                        cx,
                    );
                    plain_editor.focus_handle(cx).focus(window, cx);
                    (plain_pane, target_pane)
                })
            })
            .unwrap();
        open_project_file(
            cx,
            &project,
            &multi_workspace,
            "active.md",
            Some(&target_pane),
            false,
        )
        .await;

        let target_preview = multi_workspace
            .update(cx, |_, window, cx| {
                assert_eq!(workspace.read(cx).active_pane(), &target_pane);
                let target_preview = target_pane
                    .read(cx)
                    .active_item()
                    .and_then(|item| item.downcast::<MarkdownSplitView>())
                    .unwrap();
                assert!(plain_editor.focus_handle(cx).contains_focused(window, cx));
                target_preview
            })
            .unwrap();
        multi_workspace
            .update(cx, |_, window, cx| {
                target_preview.focus_handle(cx).focus(window, cx);
            })
            .unwrap();

        let open_background = multi_workspace
            .update(cx, |_, window, cx| {
                workspace.update(cx, |workspace, cx| {
                    workspace.open_path_preview(
                        test_project_path(&project, "background.md", cx),
                        Some(plain_pane.downgrade()),
                        false,
                        false,
                        false,
                        window,
                        cx,
                    )
                })
            })
            .unwrap();
        open_background.await.unwrap();
        cx.run_until_parked();

        multi_workspace
            .update(cx, |_, window, cx| {
                assert_eq!(workspace.read(cx).active_pane(), &target_pane);
                let plain_pane = plain_pane.read(cx);
                assert_eq!(plain_pane.items_of_type::<MarkdownSplitView>().count(), 1);
                assert_eq!(
                    plain_pane.active_item().map(|item| item.item_id()),
                    Some(plain_editor.entity_id()),
                    "a file opened in the background must not become the active tab"
                );
                assert!(target_preview.focus_handle(cx).contains_focused(window, cx));
            })
            .unwrap();
    }

    #[gpui::test]
    async fn preview_opens_for_the_given_pane_not_the_focused_editor(cx: &mut TestAppContext) {
        let app_state = init_test(cx);
        app_state
            .fs
            .as_fake()
            .insert_tree(
                path!("/dir"),
                json!({
                    "a.md": "# A\n",
                    "b.md": "# B\n"
                }),
            )
            .await;

        cx.update(|cx| {
            open_paths(
                &[PathBuf::from(path!("/dir/a.md"))],
                app_state.clone(),
                workspace::OpenOptions::default(),
                cx,
            )
        })
        .await
        .unwrap();

        let multi_workspace = cx.update(|cx| cx.windows()[0].downcast::<MultiWorkspace>().unwrap());
        let workspace = multi_workspace
            .update(cx, |multi_workspace, _, _| {
                multi_workspace.workspace().clone()
            })
            .unwrap();
        let project = workspace.read_with(cx, |workspace, _| workspace.project().clone());
        let b_buffer = project
            .update(cx, |project, cx| {
                project.open_local_buffer(path!("/dir/b.md"), cx)
            })
            .await
            .unwrap();

        let (first_pane, a_editor, second_pane, b_editor) = multi_workspace
            .update(cx, |multi_workspace, window, cx| {
                let workspace = multi_workspace.workspace().clone();
                workspace.update(cx, |workspace, cx| {
                    let first_pane = workspace.active_pane().clone();
                    let a_editor: Entity<Editor> = workspace
                        .active_item(cx)
                        .and_then(|item| item.act_as::<Editor>(cx))
                        .unwrap();
                    let project = workspace.project().clone();
                    let second_pane = workspace.split_pane(
                        first_pane.clone(),
                        workspace::SplitDirection::Right,
                        window,
                        cx,
                    );
                    let b_editor =
                        cx.new(|cx| Editor::for_buffer(b_buffer, Some(project), window, cx));
                    second_pane.update(cx, |pane, cx| {
                        pane.add_item(Box::new(b_editor.clone()), true, true, None, window, cx)
                    });
                    (first_pane, a_editor, second_pane, b_editor)
                })
            })
            .unwrap();
        cx.run_until_parked();

        // With focus in the second pane (`b.md`), simulate clicking the
        // preview button in the first pane's toolbar, which targets that
        // pane's own editor (`a.md`).
        multi_workspace
            .update(cx, |multi_workspace, window, cx| {
                let workspace = multi_workspace.workspace().clone();
                workspace.update(cx, |workspace, cx| {
                    assert_eq!(
                        workspace.active_pane(),
                        &second_pane,
                        "test precondition: focus must be in the second pane"
                    );
                    MarkdownPreviewView::open_preview_in_pane(
                        workspace,
                        a_editor.clone(),
                        first_pane.clone(),
                        window,
                        cx,
                    );
                })
            })
            .unwrap();
        cx.run_until_parked();

        cx.update(|cx| {
            let preview = first_pane
                .read(cx)
                .active_item()
                .and_then(|item| item.downcast::<MarkdownPreviewView>())
                .expect("the preview must open in the pane whose button was clicked");
            let bound_editor = preview
                .read(cx)
                .active_editor
                .as_ref()
                .unwrap()
                .editor
                .clone();
            assert_eq!(
                bound_editor, a_editor,
                "the preview must be bound to the clicked pane's editor, not the focused editor"
            );
            assert_eq!(
                second_pane
                    .read(cx)
                    .active_item()
                    .and_then(|item| item.downcast::<Editor>()),
                Some(b_editor),
                "the focused pane's content must be unaffected"
            );
        });
    }

    #[gpui::test]
    async fn switching_layouts_keeps_one_owner_for_the_editor(cx: &mut TestAppContext) {
        let (project, workspace, multi_workspace) = markdown_workspace(
            cx,
            json!({ "before.txt": "", "note.md": "- [ ] task\n", "after.txt": "" }),
            false,
        )
        .await;
        for path in ["before.txt", "note.md", "after.txt"] {
            open_project_file(cx, &project, &multi_workspace, path, None, true).await;
        }
        let editor = workspace.read_with(cx, |workspace, cx| {
            workspace
                .items_of_type::<Editor>(cx)
                .find(|editor| MarkdownPreviewView::is_markdown_file(editor, cx))
                .unwrap()
        });
        multi_workspace
            .update(cx, |_, window, cx| {
                workspace.update(cx, |workspace, cx| {
                    workspace.activate_item(&editor, true, true, window, cx);
                })
            })
            .unwrap();
        cx.update(|cx| {
            MarkdownPreviewView::apply_checkbox_toggle_to_editor(&editor, 2..5, true, cx)
        });

        for layout in [
            MarkdownLayout::SideBySide,
            MarkdownLayout::Preview,
            MarkdownLayout::SideBySide,
            MarkdownLayout::Editor,
        ] {
            switch_active_layout(cx, &multi_workspace, &workspace, layout);
            workspace.read_with(cx, |workspace, cx| {
                let pane = workspace.active_pane().read(cx);
                assert_eq!(pane.items_len(), 3);
                assert_eq!(
                    pane.active_item_index(),
                    1,
                    "the tab must keep its position"
                );
                let item = pane.active_item().unwrap();
                assert_eq!(MarkdownLayout::of_item(item.as_ref(), cx), Some(layout));
                assert_eq!(item.act_as::<Editor>(cx), Some(editor.clone()));
                assert!(
                    item.is_dirty(cx),
                    "the unsaved edit must move with the editor"
                );
                let owners = workspace
                    .items(cx)
                    .filter(|item| item.act_as::<Editor>(cx).as_ref() == Some(&editor))
                    .count();
                assert_eq!(owners, 1);
            });
        }
        workspace.read_with(cx, |workspace, cx| {
            assert_eq!(workspace.active_item_as::<Editor>(cx), Some(editor.clone()));
        });
        multi_workspace
            .update(cx, |_, window, cx| {
                assert!(editor.focus_handle(cx).contains_focused(window, cx));
            })
            .unwrap();
    }

    #[gpui::test]
    async fn split_view_searches_the_side_it_shows(cx: &mut TestAppContext) {
        let (project, workspace, multi_workspace) =
            markdown_workspace(cx, json!({ "note.md": "# Note\n" }), false).await;
        open_project_file(cx, &project, &multi_workspace, "note.md", None, true).await;
        switch_active_layout(cx, &multi_workspace, &workspace, MarkdownLayout::SideBySide);
        let split_view = active_split_view(cx, &workspace);
        let searchable_id = |cx: &mut TestAppContext| {
            split_view.read_with(cx, |split_view, cx| {
                split_view
                    .as_searchable(&split_view_handle(split_view, cx), cx)
                    .unwrap()
                    .item_id()
            })
        };
        let (editor_id, preview_id) = split_view.read_with(cx, |split_view, _| {
            (
                split_view.editor().entity_id(),
                split_view.preview().entity_id(),
            )
        });
        assert_eq!(searchable_id(cx), editor_id);
        switch_active_layout(cx, &multi_workspace, &workspace, MarkdownLayout::Preview);
        assert_eq!(searchable_id(cx), preview_id);
    }

    #[gpui::test]
    async fn opens_markdown_files_side_by_side_when_enabled(cx: &mut TestAppContext) {
        let (project, workspace, multi_workspace) =
            markdown_workspace(cx, json!({ "note.md": "# Note\n" }), false).await;
        cx.update(|cx| {
            set_auto_preview_enabled(cx, true);
            settings::SettingsStore::update_global(cx, |store, cx| {
                store.update_user_settings(cx, |settings| {
                    settings
                        .markdown_preview
                        .get_or_insert_default()
                        .open_markdown_files_side_by_side = Some(true);
                });
            });
        });
        let editor = open_project_file(cx, &project, &multi_workspace, "note.md", None, true)
            .await
            .downcast::<Editor>()
            .unwrap();

        let split_view = active_split_view(cx, &workspace);
        split_view.read_with(cx, |split_view, _| {
            assert_eq!(split_view.layout(), MarkdownLayout::SideBySide);
            assert_eq!(split_view.editor(), &editor);
        });
        multi_workspace
            .update(cx, |_, window, cx| {
                assert!(editor.focus_handle(cx).contains_focused(window, cx));
            })
            .unwrap();
    }

    #[gpui::test]
    async fn project_panel_preview_opens_preview_only_when_side_by_side_is_enabled(
        cx: &mut TestAppContext,
    ) {
        let (project, workspace, multi_workspace) =
            markdown_workspace(cx, json!({ "note.md": "# Note\n" }), false).await;
        cx.update(|cx| {
            settings::SettingsStore::update_global(cx, |store, cx| {
                store.update_user_settings(cx, |settings| {
                    settings
                        .markdown_preview
                        .get_or_insert_default()
                        .open_markdown_files_side_by_side = Some(true);
                });
            });
        });

        multi_workspace
            .update(cx, |_, window, cx| {
                workspace.update(cx, |workspace, cx| {
                    let project_path = test_project_path(&project, "note.md", cx);
                    MarkdownPreviewView::open_for_project_path(project_path, workspace, window, cx);
                });
            })
            .unwrap();
        cx.run_until_parked();

        let split_view = active_split_view(cx, &workspace);
        split_view.read_with(cx, |split_view, _| {
            assert_eq!(split_view.layout(), MarkdownLayout::Preview);
        });
        workspace.read_with(cx, |workspace, cx| {
            assert_eq!(workspace.items_of_type::<MarkdownSplitView>(cx).count(), 1);
            assert_eq!(workspace.items_of_type::<Editor>(cx).count(), 0);
        });
    }

    #[gpui::test]
    async fn switching_layouts_keeps_pinned_tabs_pinned(cx: &mut TestAppContext) {
        let (project, workspace, multi_workspace) = markdown_workspace(
            cx,
            json!({ "first.md": "# First\n", "second.md": "# Second\n" }),
            false,
        )
        .await;
        let first = open_project_file(cx, &project, &multi_workspace, "first.md", None, true).await;
        open_project_file(cx, &project, &multi_workspace, "second.md", None, true).await;
        let pane = workspace.read_with(cx, |workspace, _| workspace.active_pane().clone());
        multi_workspace
            .update(cx, |_, window, cx| {
                pane.update(cx, |pane, cx| {
                    pane.set_pinned_count(2);
                    let index = pane.index_for_item(first.as_ref()).unwrap();
                    pane.activate_item(index, true, true, window, cx);
                });
            })
            .unwrap();

        for layout in [MarkdownLayout::SideBySide, MarkdownLayout::Editor] {
            switch_active_layout(cx, &multi_workspace, &workspace, layout);
            pane.read_with(cx, |pane, cx| {
                assert_eq!(pane.pinned_count(), 2);
                assert_eq!(pane.active_item_index(), 0);
                let active_item = pane.active_item().unwrap();
                assert_eq!(
                    MarkdownLayout::of_item(active_item.as_ref(), cx),
                    Some(layout)
                );
            });
        }
    }

    #[gpui::test]
    async fn saving_a_split_view_clears_its_editors_unsaved_state(cx: &mut TestAppContext) {
        let (project, workspace, multi_workspace) =
            markdown_workspace(cx, json!({ "note.md": "- [ ] task\n" }), false).await;
        let editor = open_project_file(cx, &project, &multi_workspace, "note.md", None, true)
            .await
            .downcast::<Editor>()
            .unwrap();
        cx.update(|cx| {
            MarkdownPreviewView::apply_checkbox_toggle_to_editor(&editor, 2..5, true, cx)
        });
        cx.run_until_parked();
        workspace.read_with(cx, |workspace, _| assert!(workspace.is_edited()));

        switch_active_layout(cx, &multi_workspace, &workspace, MarkdownLayout::SideBySide);
        let save = multi_workspace
            .update(cx, |_, window, cx| {
                workspace.update(cx, |workspace, cx| {
                    workspace.save_active_item(SaveIntent::Save, window, cx)
                })
            })
            .unwrap();
        save.await.unwrap();
        cx.run_until_parked();

        workspace.read_with(cx, |workspace, _| {
            assert!(
                !workspace.is_edited(),
                "the editor's dirty state from before the swap must not outlive the save"
            )
        });
    }

    #[gpui::test]
    async fn line_links_select_the_line_in_an_open_split_view(cx: &mut TestAppContext) {
        let (project, workspace, multi_workspace) =
            markdown_workspace(cx, json!({ "note.md": "first\nsecond\nthird\n" }), false).await;
        open_project_file(cx, &project, &multi_workspace, "note.md", None, true).await;
        switch_active_layout(cx, &multi_workspace, &workspace, MarkdownLayout::SideBySide);
        let split_view = active_split_view(cx, &workspace);

        let workspace_weak = workspace.downgrade();
        multi_workspace
            .update(cx, |_, window, cx| {
                open_preview_url(
                    "note.md".into(),
                    Some("L3".into()),
                    None,
                    &workspace_weak,
                    window,
                    cx,
                );
            })
            .unwrap();
        cx.run_until_parked();

        assert_eq!(active_split_view(cx, &workspace), split_view);
        let editor = split_view.read_with(cx, |split_view, _| split_view.editor().clone());
        let row = editor.update(cx, |editor, cx| {
            let snapshot = editor.display_snapshot(cx);
            editor.selections.newest::<Point>(&snapshot).head().row
        });
        assert_eq!(row, 2);
    }

    #[gpui::test]
    async fn reopening_a_file_activates_its_split_view(cx: &mut TestAppContext) {
        let (project, workspace, multi_workspace) =
            markdown_workspace(cx, json!({ "note.md": "# Note\n", "other.txt": "" }), false).await;
        cx.update(|cx| set_auto_preview_enabled(cx, true));
        open_project_file(cx, &project, &multi_workspace, "note.md", None, true).await;
        let split_view = active_split_view(cx, &workspace);
        open_project_file(cx, &project, &multi_workspace, "other.txt", None, true).await;

        let reopened =
            open_project_file(cx, &project, &multi_workspace, "note.md", None, true).await;

        assert_eq!(reopened.item_id(), split_view.entity_id());
        workspace.read_with(cx, |workspace, cx| {
            assert_eq!(workspace.items_of_type::<MarkdownSplitView>(cx).count(), 1);
            assert_eq!(
                workspace
                    .items_of_type::<Editor>(cx)
                    .filter(|editor| MarkdownPreviewView::is_markdown_file(editor, cx))
                    .count(),
                0
            );
        });
    }

    #[gpui::test]
    async fn show_source_from_a_preview_reveals_the_split_view_that_owns_the_editor(
        cx: &mut TestAppContext,
    ) {
        let (project, workspace, multi_workspace) =
            markdown_workspace(cx, json!({ "note.md": "# Note\n" }), false).await;
        cx.update(|cx| set_auto_preview_enabled(cx, true));
        open_project_file(cx, &project, &multi_workspace, "note.md", None, true).await;
        let split_view = active_split_view(cx, &workspace);
        let editor = split_view.read_with(cx, |split_view, _| split_view.editor().clone());
        let preview = multi_workspace
            .update(cx, |_, window, cx| {
                workspace.update(cx, |workspace, cx| {
                    let preview = MarkdownPreviewView::create_markdown_view(
                        workspace,
                        editor.clone(),
                        window,
                        cx,
                    );
                    workspace.add_item_to_active_pane(
                        Box::new(preview.clone()),
                        None,
                        true,
                        window,
                        cx,
                    );
                    preview
                })
            })
            .unwrap();
        cx.run_until_parked();

        dispatch_close_and_return_to_editor(cx, &multi_workspace, &preview);
        cx.run_until_parked();

        workspace.read_with(cx, |workspace, cx| {
            assert_eq!(
                workspace.active_item_as::<MarkdownSplitView>(cx),
                Some(split_view.clone())
            );
            assert_eq!(split_view.read(cx).layout(), MarkdownLayout::SideBySide);
            assert_eq!(
                workspace.items_of_type::<MarkdownPreviewView>(cx).count(),
                0
            );
            assert_eq!(
                workspace.items_of_type::<Editor>(cx).count(),
                0,
                "the editor must not get a second tab"
            );
        });
        multi_workspace
            .update(cx, |_, window, cx| {
                assert!(editor.focus_handle(cx).contains_focused(window, cx));
            })
            .unwrap();
    }

    #[gpui::test]
    async fn side_by_side_from_a_preview_tab_turns_its_editor_tab_into_a_split_view(
        cx: &mut TestAppContext,
    ) {
        let (project, workspace, multi_workspace) =
            markdown_workspace(cx, json!({ "note.md": "# Note\n" }), false).await;
        let editor = open_project_file(cx, &project, &multi_workspace, "note.md", None, true)
            .await
            .downcast::<Editor>()
            .unwrap();
        let preview = open_preview_for_active_editor(cx, &multi_workspace);
        cx.run_until_parked();
        workspace.read_with(cx, |_, cx| {
            assert_eq!(
                MarkdownLayout::of_item(&preview, cx),
                Some(MarkdownLayout::Preview)
            );
            assert_eq!(
                MarkdownLayout::of_item(&editor, cx),
                Some(MarkdownLayout::Editor)
            );
        });

        switch_active_layout(cx, &multi_workspace, &workspace, MarkdownLayout::SideBySide);

        let split_view = active_split_view(cx, &workspace);
        workspace.read_with(cx, |workspace, cx| {
            assert_eq!(split_view.read(cx).editor(), &editor);
            assert_eq!(split_view.read(cx).layout(), MarkdownLayout::SideBySide);
            assert_eq!(
                workspace.items_of_type::<MarkdownPreviewView>(cx).count(),
                0
            );
            assert_eq!(workspace.items_of_type::<Editor>(cx).count(), 0);
        });
    }

    #[gpui::test]
    async fn previews_show_their_sources_breadcrumbs(cx: &mut TestAppContext) {
        let (multi_workspace, editor) =
            open_markdown_file(cx, "note.md", "# Note\n\nBody text\n").await;
        let preview = open_preview_for_active_editor(cx, &multi_workspace);
        cx.run_until_parked();

        cx.update(|cx| {
            let editor_breadcrumbs = editor.breadcrumbs(cx).map(|(segments, _)| {
                segments
                    .into_iter()
                    .map(|segment| segment.text)
                    .collect::<Vec<_>>()
            });
            let preview_breadcrumbs = preview.breadcrumbs(cx).map(|(segments, _)| {
                segments
                    .into_iter()
                    .map(|segment| segment.text)
                    .collect::<Vec<_>>()
            });
            assert!(editor_breadcrumbs.is_some());
            assert_eq!(preview_breadcrumbs, editor_breadcrumbs);
            assert_eq!(
                preview.breadcrumb_location(cx),
                editor.breadcrumb_location(cx)
            );
        });
    }

    fn switch_active_layout(
        cx: &mut TestAppContext,
        multi_workspace: &WindowHandle<MultiWorkspace>,
        workspace: &Entity<Workspace>,
        layout: MarkdownLayout,
    ) {
        multi_workspace
            .update(cx, |_, window, cx| {
                workspace.update(cx, |workspace, cx| {
                    let item = workspace.active_pane().read(cx).active_item().unwrap();
                    crate::markdown_split_view::switch_layout(
                        workspace,
                        item.as_ref(),
                        layout,
                        window,
                        cx,
                    );
                })
            })
            .unwrap();
        cx.run_until_parked();
    }

    fn split_view_handle(split_view: &MarkdownSplitView, cx: &App) -> Entity<MarkdownSplitView> {
        let workspace = split_view
            .editor()
            .read(cx)
            .workspace()
            .expect("the split view's editor knows its workspace");
        workspace
            .read(cx)
            .items_of_type::<MarkdownSplitView>(cx)
            .find(|handle| handle.read(cx).editor() == split_view.editor())
            .expect("the split view is in the workspace")
    }

    fn init_test(cx: &mut TestAppContext) -> Arc<AppState> {
        cx.update(|cx| {
            let state = AppState::test(cx);
            editor::init(cx);
            crate::init(cx);
            state
        })
    }

    impl super::persistence::MarkdownPreviewDb {
        db::query! {
            async fn insert_workspace(workspace_id: WorkspaceId) -> Result<()> {
                INSERT OR IGNORE INTO workspaces(workspace_id) VALUES (?)
            }
        }
    }

    async fn insert_split_view_workspace_row(workspace_id: WorkspaceId) {
        crate::markdown_split_view::insert_workspace_row_for_tests(workspace_id).await;
    }

    // In tests each static connection opens its own in-memory database, so the workspace row
    // that flushing writes through WorkspaceDb isn't visible to the preview table's foreign key.
    async fn insert_workspace_row(workspace_id: WorkspaceId) {
        super::persistence::MARKDOWN_PREVIEW_DB
            .insert_workspace(workspace_id)
            .await
            .unwrap();
    }

    async fn markdown_workspace(
        cx: &mut TestAppContext,
        files: serde_json::Value,
        lazy_language: bool,
    ) -> (
        Entity<Project>,
        Entity<Workspace>,
        WindowHandle<MultiWorkspace>,
    ) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(path!("/project"), files).await;
        let project = Project::test(fs, [path!("/project").as_ref()], cx).await;
        if lazy_language {
            register_lazy_markdown_language(&project, cx);
        } else {
            register_markdown_language(&project, cx);
        }
        let multi_workspace =
            cx.add_window(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
        let workspace = multi_workspace
            .update(cx, |multi_workspace, _, _| {
                multi_workspace.workspace().clone()
            })
            .unwrap();
        (project, workspace, multi_workspace)
    }

    async fn open_project_file(
        cx: &mut TestAppContext,
        project: &Entity<Project>,
        multi_workspace: &WindowHandle<MultiWorkspace>,
        path: &str,
        pane: Option<&Entity<Pane>>,
        focus_item: bool,
    ) -> Box<dyn ItemHandle> {
        let open = multi_workspace
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace.workspace().update(cx, |workspace, cx| {
                    workspace.open_path(
                        test_project_path(project, path, cx),
                        pane.map(Entity::downgrade),
                        focus_item,
                        window,
                        cx,
                    )
                })
            })
            .unwrap();
        let item = open.await.unwrap();
        cx.run_until_parked();
        item
    }

    fn assert_editor_replaced_by_preview(workspace: &Workspace, editor: &dyn ItemHandle, cx: &App) {
        let split_view = workspace
            .active_item_as::<MarkdownSplitView>(cx)
            .expect("the editor should be shown in a preview tab");
        assert_eq!(split_view.read(cx).layout(), MarkdownLayout::Preview);
        assert_eq!(
            split_view.read(cx).editor().entity_id(),
            editor.item_id(),
            "the preview tab should take over the editor"
        );
        assert!(
            workspace
                .panes()
                .iter()
                .all(|pane| pane.read(cx).index_for_item(editor).is_none())
        );
    }

    fn set_auto_preview_enabled(cx: &mut App, enabled: bool) {
        settings::SettingsStore::update_global(cx, |store, cx| {
            store.update_user_settings(cx, |settings| {
                settings
                    .markdown_preview
                    .get_or_insert_default()
                    .open_markdown_files_in_preview = Some(enabled);
            });
        });
    }

    fn test_project_path(project: &Entity<Project>, path: &str, cx: &App) -> ProjectPath {
        let worktree_id = project.read(cx).worktrees(cx).next().unwrap().read(cx).id();
        ProjectPath {
            worktree_id,
            path: rel_path(path).into(),
        }
    }

    fn markdown_language_config() -> language::LanguageConfig {
        language::LanguageConfig {
            name: "Markdown".into(),
            matcher: language::LanguageMatcher {
                path_suffixes: vec!["md".to_string(), "markdown".to_string()],
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn register_markdown_language(project: &Entity<Project>, cx: &mut TestAppContext) {
        let languages = project.read_with(cx, |project, _| project.languages().clone());
        languages.add(Arc::new(language::Language::new(
            markdown_language_config(),
            None,
        )));
    }

    fn register_lazy_markdown_language(project: &Entity<Project>, cx: &mut TestAppContext) {
        project
            .read_with(cx, |project, _| project.languages().clone())
            .register_test_language(markdown_language_config());
    }

    async fn wait_for_preview_serialization(cx: &mut TestAppContext) {
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(250));
        cx.run_until_parked();
    }

    fn saved_preview_path(
        _cx: &mut TestAppContext,
        item_id: ItemId,
        workspace_id: WorkspaceId,
    ) -> PathBuf {
        super::persistence::MARKDOWN_PREVIEW_DB
            .get_preview(item_id, workspace_id)
            .unwrap()
            .unwrap()
            .0
    }

    fn preview_source_path(
        cx: &mut TestAppContext,
        preview: &Entity<MarkdownPreviewView>,
    ) -> Arc<RelPath> {
        let editor = preview.read_with(cx, |preview, _| {
            preview.active_editor.as_ref().unwrap().editor.clone()
        });
        editor_source_path(cx, &editor)
    }

    fn split_view_source_path(
        cx: &mut TestAppContext,
        split_view: &Entity<MarkdownSplitView>,
    ) -> Arc<RelPath> {
        let editor = split_view.read_with(cx, |split_view, _| split_view.editor().clone());
        editor_source_path(cx, &editor)
    }

    fn active_split_view(
        cx: &mut TestAppContext,
        workspace: &Entity<Workspace>,
    ) -> Entity<MarkdownSplitView> {
        workspace.read_with(cx, |workspace, cx| {
            workspace
                .active_item_as::<MarkdownSplitView>(cx)
                .expect("a Markdown split view should be active")
        })
    }

    fn editor_source_path(cx: &mut TestAppContext, editor: &Entity<Editor>) -> Arc<RelPath> {
        editor.read_with(cx, |editor, cx| {
            let buffer = editor.buffer().read(cx).as_singleton().unwrap();
            buffer.read(cx).file().unwrap().path().clone()
        })
    }

    fn markdown_fixture_directory(tree: &TempTree) -> PathBuf {
        tree.path().join("docs")
    }

    #[track_caller]
    fn assert_resolved_preview_image_path(
        resolved: Option<ImageSource>,
        expected_path: &std::path::Path,
    ) {
        match resolved {
            Some(ImageSource::Resource(Resource::Path(path))) => {
                assert_eq!(path.as_ref(), expected_path);
            }
            _ => panic!("Expected preview image to resolve to a local path"),
        }
    }
}
