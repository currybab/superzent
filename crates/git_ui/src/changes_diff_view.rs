use anyhow::{Context as _, Result};
use buffer_diff::{BufferDiff, BufferDiffEvent};
use editor::{Editor, EditorEvent, EditorSettings, SplittableEditor};
use git::repository::RepoPath;
use gpui::{
    AnyElement, App, AppContext as _, AsyncApp, Context, Entity, EventEmitter, FocusHandle,
    Focusable, IntoElement, Render, Subscription, Task, WeakEntity, Window,
};
use language::{
    Buffer, BufferEvent, Capability, DiskState, File, LanguageRegistry, LineEnding, Point,
    ReplicaId, Rope, TextBuffer,
};
use multi_buffer::{MultiBuffer, PathKey};
use project::{Project, ProjectPath, WorktreeId, git_store::Repository};
use settings::Settings as _;
use std::{
    any::{Any, TypeId},
    path::PathBuf,
    sync::Arc,
};
use ui::prelude::*;
use util::{ResultExt as _, paths::PathStyle, rel_path::RelPath};
use workspace::{
    Item, ItemHandle as _, ItemNavHistory, Workspace,
    item::{ItemEvent, SaveOptions, TabContentParams},
    searchable::SearchableItemHandle,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChangesDiffKind {
    /// The index on the left, the working tree on the right.
    WorkingTree,
    /// HEAD on the left, the index on the right.
    Index,
}

impl ChangesDiffKind {
    fn label(self) -> &'static str {
        match self {
            ChangesDiffKind::WorkingTree => "Working Tree",
            ChangesDiffKind::Index => "Index",
        }
    }
}

/// Attached to the error when the file can't be loaded into a text buffer (e.g. it
/// is binary), so callers can fall back to opening the file itself.
#[derive(Debug)]
pub(crate) struct NotATextFile;

impl std::fmt::Display for NotATextFile {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("the file can't be opened as text")
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct OpenChangesDiff {
    pub preview: bool,
    pub focus: bool,
}

enum DiffSource {
    WorkingTree {
        buffer: Entity<Buffer>,
        unstaged_diff: Entity<BufferDiff>,
    },
    Index {
        index_buffer: Entity<Buffer>,
        staged_diff: Entity<BufferDiff>,
        unstaged_diff: Entity<BufferDiff>,
        uncommitted_diff: Entity<BufferDiff>,
        index_text: Option<String>,
        head_text: Option<String>,
    },
}

impl DiffSource {
    fn displayed_buffer(&self) -> &Entity<Buffer> {
        match self {
            DiffSource::WorkingTree { buffer, .. } => buffer,
            DiffSource::Index { index_buffer, .. } => index_buffer,
        }
    }

    fn displayed_diff(&self) -> &Entity<BufferDiff> {
        match self {
            DiffSource::WorkingTree { unstaged_diff, .. } => unstaged_diff,
            DiffSource::Index { staged_diff, .. } => staged_diff,
        }
    }
}

pub(crate) struct ChangesDiffView {
    kind: ChangesDiffKind,
    repository: WeakEntity<Repository>,
    repo_path: RepoPath,
    source: DiffSource,
    editor: Entity<SplittableEditor>,
    language_registry: Arc<LanguageRegistry>,
    refresh_index_task: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl ChangesDiffView {
    pub(crate) fn open(
        workspace: WeakEntity<Workspace>,
        repository: Entity<Repository>,
        repo_path: RepoPath,
        kind: ChangesDiffKind,
        options: OpenChangesDiff,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<()>> {
        let existing = workspace
            .update(cx, |workspace, cx| {
                workspace
                    .items_of_type::<Self>(cx)
                    .find(|view| view.read(cx).is_showing(&repository, &repo_path, kind))
            })
            .ok()
            .flatten();
        if let Some(existing) = existing {
            return match workspace.update(cx, |workspace, cx| {
                workspace.activate_item(&existing, true, options.focus, window, cx);
                if !options.preview {
                    let item_id = existing.item_id();
                    workspace.active_pane().update(cx, |pane, _| {
                        pane.unpreview_item_if_preview(item_id);
                    });
                }
            }) {
                Ok(()) => Task::ready(Ok(())),
                Err(error) => Task::ready(Err(error)),
            };
        }

        window.spawn(cx, async move |cx| {
            let project = workspace.update(cx, |workspace, _| workspace.project().clone())?;
            let project_path = repository
                .read_with(cx, |repository, cx| {
                    repository.repo_path_to_project_path(&repo_path, cx)
                })
                .with_context(|| {
                    format!("{} is outside of the project", repo_path.as_unix_str())
                })?;
            let source = load_diff_source(&project, &project_path, kind, cx).await?;
            let language_registry = project.read_with(cx, |project, _| project.languages().clone());

            workspace.update_in(cx, |workspace, window, cx| {
                let workspace_entity = cx.entity();
                let view = cx.new(|cx| {
                    Self::new(
                        kind,
                        repository.downgrade(),
                        repo_path,
                        source,
                        language_registry,
                        project,
                        workspace_entity,
                        window,
                        cx,
                    )
                });

                let pane = workspace.active_pane().clone();
                let destination_index = if options.preview {
                    pane.update(cx, |pane, cx| {
                        pane.replace_preview_item_id(view.item_id(), window, cx)
                    })
                } else {
                    None
                };
                workspace.add_item(
                    pane,
                    Box::new(view),
                    destination_index,
                    true,
                    options.focus,
                    window,
                    cx,
                );
            })
        })
    }

    fn new(
        kind: ChangesDiffKind,
        repository: WeakEntity<Repository>,
        repo_path: RepoPath,
        source: DiffSource,
        language_registry: Arc<LanguageRegistry>,
        project: Entity<Project>,
        workspace: Entity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let multibuffer = cx.new(|cx| {
            let mut multibuffer = MultiBuffer::new(Capability::ReadWrite);
            multibuffer.set_all_diff_hunks_expanded(cx);
            multibuffer
        });
        let editor = cx.new(|cx| {
            let editor = SplittableEditor::new(
                EditorSettings::get_global(cx).diff_view_style,
                multibuffer,
                project,
                workspace,
                window,
                cx,
            );
            // Hunk staging needs a diff with a secondary (index) diff attached; neither side
            // of these views has one, so the stage/restore controls would silently do nothing.
            editor.set_render_diff_hunk_controls(
                Arc::new(|_, _, _, _, _, _, _, _| gpui::Empty.into_any_element()),
                cx,
            );
            editor.rhs_editor().update(cx, |editor, cx| {
                editor.set_should_serialize(false, cx);
                editor.disable_diagnostics(cx);
                if kind == ChangesDiffKind::Index {
                    editor.set_read_only(true);
                }
            });
            editor
        });

        let mut subscriptions = vec![cx.subscribe(&editor, |_, _, event: &EditorEvent, cx| {
            cx.emit(event.clone())
        })];
        subscriptions.push(cx.subscribe(
            source.displayed_buffer(),
            |this, _, event: &BufferEvent, cx| {
                if matches!(event, BufferEvent::Edited { .. }) {
                    this.sync_excerpts(cx);
                }
            },
        ));
        subscriptions.push(cx.subscribe(
            source.displayed_diff(),
            |this, _, event: &BufferDiffEvent, cx| {
                if matches!(event, BufferDiffEvent::DiffChanged(_)) {
                    this.sync_excerpts(cx);
                }
            },
        ));
        if let DiffSource::Index {
            unstaged_diff,
            uncommitted_diff,
            ..
        } = &source
        {
            for diff in [unstaged_diff, uncommitted_diff] {
                subscriptions.push(cx.subscribe(diff, |this, _, event: &BufferDiffEvent, cx| {
                    if matches!(event, BufferDiffEvent::DiffChanged(_)) {
                        this.refresh_index_if_changed(cx);
                    }
                }));
            }
        }

        let mut this = Self {
            kind,
            repository,
            repo_path,
            source,
            editor,
            language_registry,
            refresh_index_task: Task::ready(()),
            _subscriptions: subscriptions,
        };
        this.sync_excerpts(cx);
        this
    }

    fn is_showing(
        &self,
        repository: &Entity<Repository>,
        repo_path: &RepoPath,
        kind: ChangesDiffKind,
    ) -> bool {
        self.kind == kind
            && &self.repo_path == repo_path
            && self.repository.entity_id() == repository.entity_id()
    }

    /// Keeps the whole file in a single excerpt, VS Code style, instead of
    /// folding unchanged regions the way the project diff does.
    fn sync_excerpts(&mut self, cx: &mut Context<Self>) {
        let buffer = self.source.displayed_buffer().clone();
        let diff = self.source.displayed_diff().clone();
        let max_point = buffer.read(cx).max_point();
        let path_key = PathKey::with_sort_prefix(0, self.repo_path.as_ref().clone());
        self.editor.update(cx, |editor, cx| {
            editor.set_excerpts_for_path(path_key, buffer, [Point::zero()..max_point], 0, diff, cx);
        });
    }

    fn refresh_index_if_changed(&mut self, cx: &mut Context<Self>) {
        let DiffSource::Index {
            index_buffer,
            staged_diff,
            unstaged_diff,
            uncommitted_diff,
            index_text,
            head_text,
        } = &mut self.source
        else {
            return;
        };
        let new_index_text = unstaged_diff.read(cx).base_text_string(cx);
        let new_head_text = uncommitted_diff.read(cx).base_text_string(cx);
        if &new_index_text == index_text && &new_head_text == head_text {
            return;
        }
        *index_text = new_index_text.clone();
        *head_text = new_head_text.clone();

        let mut normalized_index_text = new_index_text.unwrap_or_default();
        LineEnding::normalize(&mut normalized_index_text);
        index_buffer.update(cx, |buffer, cx| buffer.set_text(normalized_index_text, cx));

        let index_buffer = index_buffer.clone();
        let staged_diff = staged_diff.clone();
        let language_registry = self.language_registry.clone();
        self.refresh_index_task = cx.spawn(async move |_, cx| {
            update_diff_against(
                &staged_diff,
                &index_buffer,
                new_head_text,
                &language_registry,
                cx,
            )
            .await;
        });
    }
}

async fn load_diff_source(
    project: &Entity<Project>,
    project_path: &ProjectPath,
    kind: ChangesDiffKind,
    cx: &mut AsyncApp,
) -> Result<DiffSource> {
    let buffer = project
        .update(cx, |project, cx| {
            project.open_buffer(project_path.clone(), cx)
        })
        .await
        .context(NotATextFile)?;
    let unstaged_diff = project
        .update(cx, |project, cx| {
            project.open_unstaged_diff(buffer.clone(), cx)
        })
        .await?;

    match kind {
        ChangesDiffKind::WorkingTree => Ok(DiffSource::WorkingTree {
            buffer,
            unstaged_diff,
        }),
        ChangesDiffKind::Index => {
            let uncommitted_diff = project
                .update(cx, |project, cx| {
                    project.open_uncommitted_diff(buffer.clone(), cx)
                })
                .await?;
            let index_text = unstaged_diff.read_with(cx, |diff, cx| diff.base_text_string(cx));
            let head_text = uncommitted_diff.read_with(cx, |diff, cx| diff.base_text_string(cx));
            let language_registry = project.read_with(cx, |project, _| project.languages().clone());
            let display_name = project_path
                .path
                .file_name()
                .unwrap_or_default()
                .to_string();
            let blob: Arc<dyn File> = Arc::new(IndexBlob {
                path: project_path.path.clone(),
                worktree_id: project_path.worktree_id,
                is_deleted: index_text.is_none(),
                display_name,
            });
            let index_buffer = build_index_buffer(
                index_text.clone().unwrap_or_default(),
                blob,
                &language_registry,
                cx,
            )
            .await;
            let staged_diff = cx.new(|cx| {
                let snapshot = index_buffer.read(cx).text_snapshot();
                BufferDiff::new(&snapshot, cx)
            });
            update_diff_against(
                &staged_diff,
                &index_buffer,
                head_text.clone(),
                &language_registry,
                cx,
            )
            .await;
            Ok(DiffSource::Index {
                index_buffer,
                staged_diff,
                unstaged_diff,
                uncommitted_diff,
                index_text,
                head_text,
            })
        }
    }
}

async fn build_index_buffer(
    mut text: String,
    blob: Arc<dyn File>,
    language_registry: &Arc<LanguageRegistry>,
    cx: &mut AsyncApp,
) -> Entity<Buffer> {
    let line_ending = LineEnding::detect(&text);
    LineEnding::normalize(&mut text);
    let text = Rope::from(text);
    let language = cx.update(|cx| language_registry.language_for_file(&blob, Some(&text), cx));
    let language = match language {
        Some(language) => language_registry
            .load_language(&language)
            .await
            .ok()
            .and_then(|language| language.log_err()),
        None => None,
    };
    cx.new(|cx| {
        let text_buffer = TextBuffer::new_normalized(
            ReplicaId::LOCAL,
            cx.entity_id().as_non_zero_u64().into(),
            line_ending,
            text,
        );
        let mut buffer = Buffer::build(text_buffer, Some(blob), Capability::ReadWrite);
        buffer.set_language_async(language, cx);
        buffer
    })
}

async fn update_diff_against(
    diff: &Entity<BufferDiff>,
    buffer: &Entity<Buffer>,
    mut base_text: Option<String>,
    language_registry: &Arc<LanguageRegistry>,
    cx: &mut AsyncApp,
) {
    if let Some(base_text) = &mut base_text {
        LineEnding::normalize(base_text);
    }
    let (snapshot, language) = buffer.read_with(cx, |buffer, _| {
        (buffer.text_snapshot(), buffer.language().cloned())
    });
    let update = diff
        .update(cx, |diff, cx| {
            diff.update_diff(
                snapshot.clone(),
                base_text.map(|text| Arc::from(text.as_str())),
                Some(true),
                language.clone(),
                cx,
            )
        })
        .await;
    diff.update(cx, |diff, cx| {
        diff.language_changed(language, Some(language_registry.clone()), cx);
        diff.set_snapshot(update, &snapshot, cx)
    })
    .await;
}

/// The staged version of a file, which has no backing file on disk.
struct IndexBlob {
    path: Arc<RelPath>,
    worktree_id: WorktreeId,
    is_deleted: bool,
    display_name: String,
}

impl File for IndexBlob {
    fn as_local(&self) -> Option<&dyn language::LocalFile> {
        None
    }

    fn disk_state(&self) -> DiskState {
        DiskState::Historic {
            was_deleted: self.is_deleted,
        }
    }

    fn path_style(&self, _: &App) -> PathStyle {
        PathStyle::local()
    }

    fn path(&self) -> &Arc<RelPath> {
        &self.path
    }

    fn full_path(&self, _: &App) -> PathBuf {
        self.path.as_std_path().to_path_buf()
    }

    fn file_name<'a>(&'a self, _: &'a App) -> &'a str {
        &self.display_name
    }

    fn worktree_id(&self, _: &App) -> WorktreeId {
        self.worktree_id
    }

    fn to_proto(&self, _: &App) -> language::proto::File {
        unimplemented!()
    }

    fn is_private(&self) -> bool {
        false
    }

    fn can_open(&self) -> bool {
        true
    }
}

impl EventEmitter<EditorEvent> for ChangesDiffView {}

impl Focusable for ChangesDiffView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.focus_handle(cx)
    }
}

impl Render for ChangesDiffView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        self.editor.clone()
    }
}

impl Item for ChangesDiffView {
    type Event = EditorEvent;

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<Icon> {
        Some(Icon::new(IconName::Diff).color(Color::Muted))
    }

    fn tab_content(&self, params: TabContentParams, _window: &Window, cx: &App) -> AnyElement {
        Label::new(self.tab_content_text(0, cx))
            .color(if params.selected {
                Color::Default
            } else {
                Color::Muted
            })
            .when(params.preview, |label| label.italic())
            .into_any_element()
    }

    fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
        let file_name = self
            .repo_path
            .file_name()
            .unwrap_or_else(|| self.repo_path.as_unix_str());
        format!("{file_name} ({})", self.kind.label()).into()
    }

    fn tab_tooltip_text(&self, _cx: &App) -> Option<SharedString> {
        Some(format!("{} ({})", self.repo_path.as_unix_str(), self.kind.label()).into())
    }

    fn telemetry_event_text(&self) -> Option<&'static str> {
        Some("Changes Diff Opened")
    }

    fn to_item_events(event: &EditorEvent, f: &mut dyn FnMut(ItemEvent)) {
        match event {
            // The excerpt is re-synced whenever the diff changes; that is not a user
            // edit and must not turn a preview tab into a permanent one.
            EditorEvent::ExcerptsAdded { .. } | EditorEvent::ExcerptsRemoved { .. } => {}
            _ => Editor::to_item_events(event, f),
        }
    }

    fn preserve_preview(&self, cx: &App) -> bool {
        // The index snapshot is rewritten by us, never by the user.
        self.kind == ChangesDiffKind::Index
            || self.source.displayed_buffer().read(cx).preserve_preview()
    }

    fn deactivated(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |editor, cx| {
            editor
                .rhs_editor()
                .update(cx, |editor, cx| editor.deactivated(window, cx))
        });
    }

    fn navigate(
        &mut self,
        data: Arc<dyn Any + Send>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.editor.update(cx, |editor, cx| {
            editor
                .rhs_editor()
                .update(cx, |editor, cx| editor.navigate(data, window, cx))
        })
    }

    fn set_nav_history(
        &mut self,
        nav_history: ItemNavHistory,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.editor.update(cx, |editor, cx| {
            editor
                .rhs_editor()
                .update(cx, |editor, _| editor.set_nav_history(Some(nav_history)))
        });
    }

    fn as_searchable(&self, _: &Entity<Self>, _cx: &App) -> Option<Box<dyn SearchableItemHandle>> {
        Some(Box::new(self.editor.clone()))
    }

    fn for_each_project_item(
        &self,
        cx: &App,
        f: &mut dyn FnMut(gpui::EntityId, &dyn project::ProjectItem),
    ) {
        if self.kind == ChangesDiffKind::WorkingTree {
            self.editor
                .read(cx)
                .rhs_editor()
                .read(cx)
                .for_each_project_item(cx, f)
        }
    }

    fn is_dirty(&self, cx: &App) -> bool {
        self.kind == ChangesDiffKind::WorkingTree
            && self.source.displayed_buffer().read(cx).is_dirty()
    }

    fn has_conflict(&self, cx: &App) -> bool {
        self.kind == ChangesDiffKind::WorkingTree
            && self.source.displayed_buffer().read(cx).has_conflict()
    }

    fn can_save(&self, _cx: &App) -> bool {
        self.kind == ChangesDiffKind::WorkingTree
    }

    fn save(
        &mut self,
        options: SaveOptions,
        project: Entity<Project>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        self.editor.update(cx, |editor, cx| {
            editor
                .rhs_editor()
                .update(cx, |editor, cx| editor.save(options, project, window, cx))
        })
    }

    fn reload(
        &mut self,
        project: Entity<Project>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        self.editor.update(cx, |editor, cx| {
            editor
                .rhs_editor()
                .update(cx, |editor, cx| editor.reload(project, window, cx))
        })
    }

    fn act_as_type<'a>(
        &'a self,
        type_id: TypeId,
        self_handle: &'a Entity<Self>,
        cx: &'a App,
    ) -> Option<gpui::AnyEntity> {
        if type_id == TypeId::of::<Self>() {
            Some(self_handle.clone().into())
        } else if type_id == TypeId::of::<Editor>() {
            Some(self.editor.read(cx).rhs_editor().clone().into())
        } else if type_id == TypeId::of::<SplittableEditor>() {
            Some(self.editor.clone().into())
        } else {
            None
        }
    }

    fn added_to_workspace(
        &mut self,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.editor.update(cx, |editor, cx| {
            editor.added_to_workspace(workspace, window, cx)
        });
    }
}
