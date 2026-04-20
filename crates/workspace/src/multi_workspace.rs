use anyhow::Result;
use gpui::{
    AnyView, App, Context, DragMoveEvent, Entity, EntityId, EventEmitter, FocusHandle, Focusable,
    ManagedView, MouseButton, Pixels, Render, Subscription, Task, Tiling, Window, WindowId,
    actions, deferred, px,
};
#[cfg(any(test, feature = "test-support"))]
use project::Project;
use std::future::Future;
use std::path::PathBuf;
use ui::prelude::*;
use util::ResultExt;

const SIDEBAR_RESIZE_HANDLE_SIZE: Pixels = px(6.0);

use crate::{
    CloseIntent, CloseWindow, DockPosition, Event as WorkspaceEvent, Item, ModalView, Panel,
    Workspace, WorkspaceId, client_side_decorations,
};

actions!(
    multi_workspace,
    [
        /// Creates a new workspace within the current window.
        NewWorkspaceInWindow,
        /// Switches to the next workspace within the current window.
        NextWorkspaceInWindow,
        /// Switches to the previous workspace within the current window.
        PreviousWorkspaceInWindow,
        /// Toggles the workspace switcher sidebar.
        ToggleWorkspaceSidebar,
        /// Moves focus to or from the workspace sidebar without closing it.
        FocusWorkspaceSidebar,
    ]
);

pub enum MultiWorkspaceEvent {
    ActiveWorkspaceChanged,
    WorkspaceAdded(Entity<Workspace>),
    WorkspaceRemoved(EntityId),
}

pub enum SidebarEvent {
    Open,
    Close,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WorkspaceCycleDirection {
    Forward,
    Backward,
}

#[derive(Clone, Debug)]
struct WorkspaceCycleState {
    ordered_workspace_ids: Vec<EntityId>,
    selected_index: usize,
}

pub trait Sidebar: EventEmitter<SidebarEvent> + Focusable + Render + Sized {
    fn width(&self, cx: &App) -> Pixels;
    fn set_width(&mut self, width: Option<Pixels>, cx: &mut Context<Self>);
    fn has_notifications(&self, cx: &App) -> bool;
    fn toggle_recent_projects_popover(&self, window: &mut Window, cx: &mut App);
    fn is_recent_projects_popover_deployed(&self) -> bool;
    /// Makes focus reset bac to the search editor upon toggling the sidebar from outside
    fn prepare_for_focus(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {}
}

pub trait SidebarHandle: 'static + Send + Sync {
    fn width(&self, cx: &App) -> Pixels;
    fn set_width(&self, width: Option<Pixels>, cx: &mut App);
    fn focus_handle(&self, cx: &App) -> FocusHandle;
    fn focus(&self, window: &mut Window, cx: &mut App);
    fn prepare_for_focus(&self, window: &mut Window, cx: &mut App);
    fn has_notifications(&self, cx: &App) -> bool;
    fn to_any(&self) -> AnyView;
    fn entity_id(&self) -> EntityId;
    fn toggle_recent_projects_popover(&self, window: &mut Window, cx: &mut App);
    fn is_recent_projects_popover_deployed(&self, cx: &App) -> bool;
}

#[derive(Clone)]
pub struct DraggedSidebar;

impl Render for DraggedSidebar {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

impl<T: Sidebar> SidebarHandle for Entity<T> {
    fn width(&self, cx: &App) -> Pixels {
        self.read(cx).width(cx)
    }

    fn set_width(&self, width: Option<Pixels>, cx: &mut App) {
        self.update(cx, |this, cx| this.set_width(width, cx))
    }

    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.read(cx).focus_handle(cx)
    }

    fn focus(&self, window: &mut Window, cx: &mut App) {
        let handle = self.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
    }

    fn prepare_for_focus(&self, window: &mut Window, cx: &mut App) {
        self.update(cx, |this, cx| this.prepare_for_focus(window, cx));
    }

    fn has_notifications(&self, cx: &App) -> bool {
        self.read(cx).has_notifications(cx)
    }

    fn to_any(&self) -> AnyView {
        self.clone().into()
    }

    fn entity_id(&self) -> EntityId {
        Entity::entity_id(self)
    }

    fn toggle_recent_projects_popover(&self, window: &mut Window, cx: &mut App) {
        self.update(cx, |this, cx| {
            this.toggle_recent_projects_popover(window, cx);
        });
    }

    fn is_recent_projects_popover_deployed(&self, cx: &App) -> bool {
        self.read(cx).is_recent_projects_popover_deployed()
    }
}

pub struct MultiWorkspace {
    window_id: WindowId,
    workspaces: Vec<Entity<Workspace>>,
    workspace_usage_order: Vec<EntityId>,
    workspace_cycle_state: Option<WorkspaceCycleState>,
    active_workspace_index: usize,
    sidebar: Option<Box<dyn SidebarHandle>>,
    sidebar_open: bool,
    pending_removal_tasks: Vec<Task<()>>,
    _serialize_task: Option<Task<()>>,
    _sidebar_subscription: Option<Subscription>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<MultiWorkspaceEvent> for MultiWorkspace {}

impl MultiWorkspace {
    pub fn new(workspace: Entity<Workspace>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let workspace_id = workspace.entity_id();
        let release_subscription = cx.on_release(|this: &mut MultiWorkspace, _cx| {
            if let Some(task) = this._serialize_task.take() {
                task.detach();
            }
            for task in std::mem::take(&mut this.pending_removal_tasks) {
                task.detach();
            }
        });
        let quit_subscription = cx.on_app_quit(Self::app_will_quit);
        let settings_subscription =
            cx.observe_global_in::<settings::SettingsStore>(window, |_this, _window, _cx| {});
        Self::subscribe_to_workspace(&workspace, cx);
        Self {
            window_id: window.window_handle().window_id(),
            workspaces: vec![workspace],
            workspace_usage_order: vec![workspace_id],
            workspace_cycle_state: None,
            active_workspace_index: 0,
            sidebar: None,
            sidebar_open: true,
            pending_removal_tasks: Vec::new(),
            _serialize_task: None,
            _sidebar_subscription: None,
            _subscriptions: vec![
                release_subscription,
                quit_subscription,
                settings_subscription,
            ],
        }
    }

    pub fn register_sidebar<T: Sidebar>(
        &mut self,
        sidebar: Entity<T>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let subscription =
            cx.subscribe_in(&sidebar, window, |this, _, event, window, cx| match event {
                SidebarEvent::Open => this.toggle_sidebar(window, cx),
                SidebarEvent::Close => this.close_sidebar(window, cx),
            });
        self.sidebar = Some(Box::new(sidebar));
        self._sidebar_subscription = Some(subscription);
    }

    pub fn sidebar(&self) -> Option<&dyn SidebarHandle> {
        self.sidebar.as_deref()
    }

    pub fn sidebar_open(&self) -> bool {
        self.sidebar_open && self.sidebar.is_some()
    }

    pub fn sidebar_has_notifications(&self, cx: &App) -> bool {
        self.sidebar
            .as_ref()
            .map_or(false, |s| s.has_notifications(cx))
    }

    pub fn toggle_recent_projects_popover(&self, window: &mut Window, cx: &mut App) {
        if let Some(sidebar) = &self.sidebar {
            sidebar.toggle_recent_projects_popover(window, cx);
        }
    }

    pub fn is_recent_projects_popover_deployed(&self, cx: &App) -> bool {
        self.sidebar
            .as_ref()
            .map_or(false, |s| s.is_recent_projects_popover_deployed(cx))
    }

    pub fn multi_workspace_enabled(&self, cx: &App) -> bool {
        let _ = cx;
        true
    }

    pub fn toggle_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.multi_workspace_enabled(cx) {
            return;
        }

        if self.sidebar_open {
            self.close_sidebar(window, cx);
        } else {
            self.open_sidebar(cx);
            if let Some(sidebar) = &self.sidebar {
                sidebar.prepare_for_focus(window, cx);
                sidebar.focus(window, cx);
            }
        }
    }

    pub fn focus_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.multi_workspace_enabled(cx) {
            return;
        }

        if self.sidebar_open {
            let sidebar_is_focused = self
                .sidebar
                .as_ref()
                .is_some_and(|s| s.focus_handle(cx).contains_focused(window, cx));

            if sidebar_is_focused {
                let pane = self.workspace().read(cx).active_pane().clone();
                let pane_focus = pane.read(cx).focus_handle(cx);
                window.focus(&pane_focus, cx);
            } else if let Some(sidebar) = &self.sidebar {
                sidebar.prepare_for_focus(window, cx);
                sidebar.focus(window, cx);
            }
        } else {
            self.open_sidebar(cx);
            if let Some(sidebar) = &self.sidebar {
                sidebar.prepare_for_focus(window, cx);
                sidebar.focus(window, cx);
            }
        }
    }

    pub fn open_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar_open = true;
        self.serialize(cx);
        cx.notify();
    }

    fn close_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sidebar_open = false;
        let pane = self.workspace().read(cx).active_pane().clone();
        let pane_focus = pane.read(cx).focus_handle(cx);
        window.focus(&pane_focus, cx);
        self.serialize(cx);
        cx.notify();
    }

    pub fn close_window(&mut self, _: &CloseWindow, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            let workspaces = this.update(cx, |multi_workspace, _cx| {
                multi_workspace.workspaces().to_vec()
            })?;

            for workspace in workspaces {
                let should_continue = workspace
                    .update_in(cx, |workspace, window, cx| {
                        workspace.prepare_to_close(CloseIntent::CloseWindow, window, cx)
                    })?
                    .await?;
                if !should_continue {
                    return anyhow::Ok(());
                }
            }

            cx.update(|window, _cx| {
                window.remove_window();
            })?;

            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    }

    pub fn close_workspace_at_index(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        let Some(workspace) = self.workspaces.get(index).cloned() else {
            return Task::ready(Ok(()));
        };
        let workspace_id = workspace.entity_id();
        let app_state = workspace.read(cx).app_state().clone();
        let Some(window_handle) = window.window_handle().downcast::<MultiWorkspace>() else {
            return Task::ready(Ok(()));
        };

        cx.spawn_in(window, async move |this, cx| {
            let should_continue = workspace
                .update_in(cx, |workspace, window, cx| {
                    workspace.prepare_to_close(CloseIntent::CloseWorkspace, window, cx)
                })?
                .await?;
            if !should_continue {
                return anyhow::Ok(());
            }

            let needs_replacement = this.update(cx, |multi_workspace, _cx| {
                multi_workspace.workspaces.len() == 1
            })?;
            if needs_replacement {
                let _ = cx
                    .update(|_window, cx| {
                        Workspace::new_local(
                            vec![],
                            app_state.clone(),
                            Some(window_handle),
                            None,
                            None,
                            true,
                            cx,
                        )
                    })?
                    .await?;
            }

            this.update_in(cx, |multi_workspace, window, cx| {
                if let Some(index) = multi_workspace
                    .workspaces
                    .iter()
                    .position(|candidate| candidate.entity_id() == workspace_id)
                {
                    multi_workspace.remove_workspace(index, window, cx);
                }
            })?;

            anyhow::Ok(())
        })
    }

    fn subscribe_to_workspace(workspace: &Entity<Workspace>, cx: &mut Context<Self>) {
        cx.subscribe(workspace, |this, workspace, event, cx| {
            if let WorkspaceEvent::Activate = event {
                this.activate(workspace, cx);
            }
        })
        .detach();
    }

    pub fn workspace(&self) -> &Entity<Workspace> {
        &self.workspaces[self.active_workspace_index]
    }

    pub fn workspaces(&self) -> &[Entity<Workspace>] {
        &self.workspaces
    }

    pub fn active_workspace_index(&self) -> usize {
        self.active_workspace_index
    }

    pub fn workspace_entries_excluding_active(
        &self,
        cx: &App,
    ) -> Vec<(usize, std::path::PathBuf, Entity<Workspace>)> {
        self.workspaces
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != self.active_workspace_index)
            .map(|(index, workspace)| {
                let worktree_path = workspace
                    .read(cx)
                    .project()
                    .read(cx)
                    .worktrees(cx)
                    .next()
                    .map(|worktree| worktree.read(cx).abs_path().to_path_buf())
                    .unwrap_or_default();
                (index, worktree_path, workspace.clone())
            })
            .collect()
    }

    pub fn workspaces_by_recent_use(&self) -> Vec<Entity<Workspace>> {
        let mut ordered_workspaces = Vec::with_capacity(self.workspaces.len());

        for workspace_id in &self.workspace_usage_order {
            if let Some(workspace) = self
                .workspaces
                .iter()
                .find(|workspace| workspace.entity_id() == *workspace_id)
            {
                ordered_workspaces.push(workspace.clone());
            }
        }

        for workspace in &self.workspaces {
            if ordered_workspaces
                .iter()
                .any(|ordered_workspace| ordered_workspace == workspace)
            {
                continue;
            }

            ordered_workspaces.push(workspace.clone());
        }

        ordered_workspaces
    }

    pub fn activate(&mut self, workspace: Entity<Workspace>, cx: &mut Context<Self>) {
        if !self.multi_workspace_enabled(cx) {
            self.workspaces[0] = workspace;
            self.active_workspace_index = 0;
            self.record_workspace_usage(self.workspaces[0].entity_id());
            self.reset_workspace_cycle_state();
            cx.emit(MultiWorkspaceEvent::ActiveWorkspaceChanged);
            cx.notify();
            return;
        }

        let old_index = self.active_workspace_index;
        let new_index = self.set_active_workspace(workspace, cx);
        if old_index != new_index {
            self.serialize(cx);
        }
    }

    fn set_active_workspace(
        &mut self,
        workspace: Entity<Workspace>,
        cx: &mut Context<Self>,
    ) -> usize {
        self.reset_workspace_cycle_state();
        let index = self.add_workspace(workspace, cx);
        let changed = self.active_workspace_index != index;
        self.active_workspace_index = index;
        self.record_workspace_usage(self.workspaces[index].entity_id());
        if changed {
            cx.emit(MultiWorkspaceEvent::ActiveWorkspaceChanged);
        }
        cx.notify();
        index
    }

    /// Adds a workspace to this window without changing which workspace is active.
    /// Returns the index of the workspace (existing or newly inserted).
    pub fn add_workspace(&mut self, workspace: Entity<Workspace>, cx: &mut Context<Self>) -> usize {
        self.reset_workspace_cycle_state();
        if let Some(index) = self.workspaces.iter().position(|w| *w == workspace) {
            self.record_workspace_usage(self.workspaces[index].entity_id());
            index
        } else {
            Self::subscribe_to_workspace(&workspace, cx);
            self.workspaces.push(workspace.clone());
            self.record_workspace_usage(workspace.entity_id());
            cx.emit(MultiWorkspaceEvent::WorkspaceAdded(workspace));
            cx.notify();
            self.workspaces.len() - 1
        }
    }

    pub fn replace_workspace(
        &mut self,
        workspace_to_replace: Entity<Workspace>,
        new_workspace: Entity<Workspace>,
        cx: &mut Context<Self>,
    ) -> bool {
        self.reset_workspace_cycle_state();
        let Some(index) = self
            .workspaces
            .iter()
            .position(|workspace| *workspace == workspace_to_replace)
        else {
            return false;
        };

        Self::subscribe_to_workspace(&new_workspace, cx);
        let removed_workspace =
            std::mem::replace(&mut self.workspaces[index], new_workspace.clone());
        self.active_workspace_index = index;
        self.remove_workspace_from_usage_order(removed_workspace.entity_id());
        self.record_workspace_usage(new_workspace.entity_id());
        self.sync_workspace_usage_order();
        self.serialize(cx);
        cx.emit(MultiWorkspaceEvent::WorkspaceRemoved(
            removed_workspace.entity_id(),
        ));
        cx.emit(MultiWorkspaceEvent::WorkspaceAdded(new_workspace));
        cx.emit(MultiWorkspaceEvent::ActiveWorkspaceChanged);
        cx.notify();
        true
    }

    pub fn activate_index(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.activate_index_internal(index, window, cx, false);
    }

    pub fn activate_next_recent_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cycle_recent_workspace(WorkspaceCycleDirection::Forward, window, cx);
    }

    pub fn activate_previous_recent_workspace(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cycle_recent_workspace(WorkspaceCycleDirection::Backward, window, cx);
    }

    fn activate_index_internal(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
        preserve_cycle_state: bool,
    ) {
        debug_assert!(
            index < self.workspaces.len(),
            "workspace index out of bounds"
        );
        if !preserve_cycle_state {
            self.reset_workspace_cycle_state();
        }
        let changed = self.active_workspace_index != index;
        self.active_workspace_index = index;
        self.record_workspace_usage(self.workspaces[index].entity_id());
        self.serialize(cx);
        self.focus_active_workspace(window, cx);
        if changed {
            cx.emit(MultiWorkspaceEvent::ActiveWorkspaceChanged);
        }
        cx.notify();
    }

    fn cycle_recent_workspace(
        &mut self,
        direction: WorkspaceCycleDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.workspaces.len() <= 1 {
            return;
        }

        let active_workspace_id = self.workspaces[self.active_workspace_index].entity_id();
        let ordered_workspace_ids = self
            .workspace_cycle_state
            .take()
            .filter(|state| self.can_resume_workspace_cycle(state, active_workspace_id))
            .map(|state| {
                let target_index = match direction {
                    WorkspaceCycleDirection::Forward => {
                        (state.selected_index + 1) % state.ordered_workspace_ids.len()
                    }
                    WorkspaceCycleDirection::Backward => {
                        if state.selected_index == 0 {
                            state.ordered_workspace_ids.len() - 1
                        } else {
                            state.selected_index - 1
                        }
                    }
                };

                (state.ordered_workspace_ids, target_index)
            })
            .or_else(|| {
                let ordered_workspace_ids = self
                    .workspaces_by_recent_use()
                    .into_iter()
                    .map(|workspace| workspace.entity_id())
                    .collect::<Vec<_>>();
                let selected_index = ordered_workspace_ids
                    .iter()
                    .position(|workspace_id| *workspace_id == active_workspace_id)?;
                let target_index = match direction {
                    WorkspaceCycleDirection::Forward => {
                        (selected_index + 1) % ordered_workspace_ids.len()
                    }
                    WorkspaceCycleDirection::Backward => {
                        if selected_index == 0 {
                            ordered_workspace_ids.len() - 1
                        } else {
                            selected_index - 1
                        }
                    }
                };

                Some((ordered_workspace_ids, target_index))
            });

        let Some((ordered_workspace_ids, target_index)) = ordered_workspace_ids else {
            return;
        };
        let Some(target_workspace_id) = ordered_workspace_ids.get(target_index).copied() else {
            self.reset_workspace_cycle_state();
            return;
        };
        let Some(target_workspace_index) = self
            .workspaces
            .iter()
            .position(|workspace| workspace.entity_id() == target_workspace_id)
        else {
            self.reset_workspace_cycle_state();
            return;
        };

        self.workspace_cycle_state = Some(WorkspaceCycleState {
            ordered_workspace_ids,
            selected_index: target_index,
        });
        self.activate_index_internal(target_workspace_index, window, cx, true);
    }

    fn serialize(&mut self, cx: &mut App) {
        let window_id = self.window_id;
        let state = crate::persistence::model::MultiWorkspaceState {
            active_workspace_id: self.workspace().read(cx).database_id(),
            sidebar_open: self.sidebar_open,
        };
        self._serialize_task = Some(cx.background_spawn(async move {
            crate::persistence::write_multi_workspace_state(window_id, state).await;
        }));
    }

    /// Returns the in-flight serialization task (if any) so the caller can
    /// await it. Used by the quit handler to ensure pending DB writes
    /// complete before the process exits.
    pub fn flush_serialization(&mut self) -> Task<()> {
        self._serialize_task.take().unwrap_or(Task::ready(()))
    }

    fn app_will_quit(&mut self, _cx: &mut Context<Self>) -> impl Future<Output = ()> + use<> {
        let mut tasks: Vec<Task<()>> = Vec::new();
        if let Some(task) = self._serialize_task.take() {
            tasks.push(task);
        }
        tasks.extend(std::mem::take(&mut self.pending_removal_tasks));

        async move {
            futures::future::join_all(tasks).await;
        }
    }

    pub fn focus_active_workspace(&self, window: &mut Window, cx: &mut App) {
        // If a dock panel is zoomed, focus it instead of the center pane.
        // Otherwise, focusing the center pane triggers dismiss_zoomed_items_to_reveal
        // which closes the zoomed dock.
        let focus_handle = {
            let workspace = self.workspace().read(cx);
            let mut target = None;
            for dock in workspace.all_docks() {
                let dock = dock.read(cx);
                if dock.is_open() {
                    if let Some(panel) = dock.active_panel() {
                        if panel.is_zoomed(window, cx) {
                            target = Some(panel.panel_focus_handle(cx));
                            break;
                        }
                    }
                }
            }
            target.unwrap_or_else(|| {
                let pane = workspace.active_pane().clone();
                pane.read(cx).focus_handle(cx)
            })
        };
        window.focus(&focus_handle, cx);
    }

    pub fn panel<T: Panel>(&self, cx: &App) -> Option<Entity<T>> {
        self.workspace().read(cx).panel::<T>(cx)
    }

    pub fn active_modal<V: ManagedView + 'static>(&self, cx: &App) -> Option<Entity<V>> {
        self.workspace().read(cx).active_modal::<V>(cx)
    }

    pub fn add_panel<T: Panel>(
        &mut self,
        panel: Entity<T>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.workspace().update(cx, |workspace, cx| {
            workspace.add_panel(panel, window, cx);
        });
    }

    pub fn focus_panel<T: Panel>(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<T>> {
        self.workspace()
            .update(cx, |workspace, cx| workspace.focus_panel::<T>(window, cx))
    }

    // used in a test
    pub fn toggle_modal<V: ModalView, B>(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        build: B,
    ) where
        B: FnOnce(&mut Window, &mut gpui::Context<V>) -> V,
    {
        self.workspace().update(cx, |workspace, cx| {
            workspace.toggle_modal(window, cx, build);
        });
    }

    pub fn toggle_dock(
        &mut self,
        dock_side: DockPosition,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.workspace().update(cx, |workspace, cx| {
            workspace.toggle_dock(dock_side, window, cx);
        });
    }

    pub fn active_item_as<I: 'static>(&self, cx: &App) -> Option<Entity<I>> {
        self.workspace().read(cx).active_item_as::<I>(cx)
    }

    pub fn items_of_type<'a, T: Item>(
        &'a self,
        cx: &'a App,
    ) -> impl 'a + Iterator<Item = Entity<T>> {
        self.workspace().read(cx).items_of_type::<T>(cx)
    }

    pub fn database_id(&self, cx: &App) -> Option<WorkspaceId> {
        self.workspace().read(cx).database_id()
    }

    pub fn take_pending_removal_tasks(&mut self) -> Vec<Task<()>> {
        let tasks: Vec<Task<()>> = std::mem::take(&mut self.pending_removal_tasks)
            .into_iter()
            .filter(|task| !task.is_ready())
            .collect();
        tasks
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn set_random_database_id(&mut self, cx: &mut Context<Self>) {
        self.workspace().update(cx, |workspace, _cx| {
            workspace.set_random_database_id();
        });
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn test_new(project: Entity<Project>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let workspace = cx.new(|cx| Workspace::test_new(project, window, cx));
        Self::new(workspace, window, cx)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn test_add_workspace(
        &mut self,
        project: Entity<Project>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<Workspace> {
        let workspace = cx.new(|cx| Workspace::test_new(project, window, cx));
        self.activate(workspace.clone(), cx);
        workspace
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn create_test_workspace(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        let app_state = self.workspace().read(cx).app_state().clone();
        let project = Project::local(
            app_state.client.clone(),
            app_state.node_runtime.clone(),
            app_state.user_store.clone(),
            app_state.languages.clone(),
            app_state.fs.clone(),
            None,
            project::LocalProjectFlags::default(),
            cx,
        );
        let new_workspace = cx.new(|cx| Workspace::new(None, project, app_state, window, cx));
        self.set_active_workspace(new_workspace.clone(), cx);
        self.focus_active_workspace(window, cx);

        let weak_workspace = new_workspace.downgrade();
        cx.spawn_in(window, async move |this, cx| {
            let workspace_id = crate::persistence::DB.next_id().await.unwrap();
            let workspace = weak_workspace.upgrade().unwrap();
            let task: Task<()> = this
                .update_in(cx, |this, window, cx| {
                    let session_id = workspace.read(cx).session_id();
                    let window_id = window.window_handle().window_id().as_u64();
                    workspace.update(cx, |workspace, _cx| {
                        workspace.set_database_id(workspace_id);
                    });
                    this.serialize(cx);
                    cx.background_spawn(async move {
                        crate::persistence::DB
                            .set_session_binding(workspace_id, session_id, Some(window_id))
                            .await
                            .log_err();
                    })
                })
                .unwrap();
            task.await
        })
    }

    pub fn remove_workspace(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.workspaces.len() <= 1 || index >= self.workspaces.len() {
            return;
        }

        self.reset_workspace_cycle_state();
        let removed_workspace = self.workspaces.remove(index);
        self.remove_workspace_from_usage_order(removed_workspace.entity_id());

        if self.active_workspace_index >= self.workspaces.len() {
            self.active_workspace_index = self.workspaces.len() - 1;
        } else if self.active_workspace_index > index {
            self.active_workspace_index -= 1;
        }

        if let Some(active_workspace) = self.workspaces.get(self.active_workspace_index) {
            self.record_workspace_usage(active_workspace.entity_id());
        }

        self.sync_workspace_usage_order();

        if let Some(workspace_id) = removed_workspace.read(cx).database_id() {
            self.pending_removal_tasks.retain(|task| !task.is_ready());
            self.pending_removal_tasks
                .push(cx.background_spawn(async move {
                    // Clear the session binding instead of deleting the row so
                    // the workspace still appears in the recent-projects list.
                    crate::persistence::DB
                        .set_session_binding(workspace_id, None, None)
                        .await
                        .log_err();
                }));
        }

        self.serialize(cx);
        self.focus_active_workspace(window, cx);
        cx.emit(MultiWorkspaceEvent::WorkspaceRemoved(
            removed_workspace.entity_id(),
        ));
        cx.emit(MultiWorkspaceEvent::ActiveWorkspaceChanged);
        cx.notify();
    }

    pub fn open_project(
        &mut self,
        paths: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        let workspace = self.workspace().clone();

        if self.multi_workspace_enabled(cx) {
            workspace.update(cx, |workspace, cx| {
                workspace.open_workspace_for_paths(true, paths, window, cx)
            })
        } else {
            cx.spawn_in(window, async move |_this, cx| {
                let should_continue = workspace
                    .update_in(cx, |workspace, window, cx| {
                        workspace.prepare_to_close(crate::CloseIntent::ReplaceWindow, window, cx)
                    })?
                    .await?;
                if should_continue {
                    workspace
                        .update_in(cx, |workspace, window, cx| {
                            workspace.open_workspace_for_paths(true, paths, window, cx)
                        })?
                        .await
                } else {
                    Ok(())
                }
            })
        }
    }

    fn record_workspace_usage(&mut self, workspace_id: EntityId) {
        self.workspace_usage_order
            .retain(|existing_id| *existing_id != workspace_id);
        self.workspace_usage_order.insert(0, workspace_id);
    }

    fn can_resume_workspace_cycle(
        &self,
        state: &WorkspaceCycleState,
        active_workspace_id: EntityId,
    ) -> bool {
        state.selected_index < state.ordered_workspace_ids.len()
            && state.ordered_workspace_ids[state.selected_index] == active_workspace_id
            && state.ordered_workspace_ids.len() == self.workspaces.len()
            && state.ordered_workspace_ids.iter().all(|workspace_id| {
                self.workspaces
                    .iter()
                    .any(|workspace| workspace.entity_id() == *workspace_id)
            })
    }

    fn reset_workspace_cycle_state(&mut self) {
        self.workspace_cycle_state = None;
    }

    fn remove_workspace_from_usage_order(&mut self, workspace_id: EntityId) {
        self.workspace_usage_order
            .retain(|existing_id| *existing_id != workspace_id);
    }

    fn sync_workspace_usage_order(&mut self) {
        self.workspace_usage_order.retain(|workspace_id| {
            self.workspaces
                .iter()
                .any(|workspace| workspace.entity_id() == *workspace_id)
        });

        for workspace in &self.workspaces {
            if self
                .workspace_usage_order
                .iter()
                .any(|workspace_id| *workspace_id == workspace.entity_id())
            {
                continue;
            }

            self.workspace_usage_order.push(workspace.entity_id());
        }
    }
}

impl Render for MultiWorkspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let multi_workspace_enabled = self.multi_workspace_enabled(cx);

        let sidebar: Option<AnyElement> = if multi_workspace_enabled && self.sidebar_open() {
            self.sidebar.as_ref().map(|sidebar_handle| {
                let weak = cx.weak_entity();

                let sidebar_width = sidebar_handle.width(cx);
                let resize_handle = deferred(
                    div()
                        .id("sidebar-resize-handle")
                        .absolute()
                        .right(-SIDEBAR_RESIZE_HANDLE_SIZE / 2.)
                        .top(px(0.))
                        .h_full()
                        .w(SIDEBAR_RESIZE_HANDLE_SIZE)
                        .cursor_col_resize()
                        .on_drag(DraggedSidebar, |dragged, _, _, cx| {
                            cx.stop_propagation();
                            cx.new(|_| dragged.clone())
                        })
                        .on_mouse_down(MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .on_mouse_up(MouseButton::Left, move |event, _, cx| {
                            if event.click_count == 2 {
                                weak.update(cx, |this, cx| {
                                    if let Some(sidebar) = this.sidebar.as_mut() {
                                        sidebar.set_width(None, cx);
                                    }
                                })
                                .ok();
                                cx.stop_propagation();
                            }
                        })
                        .occlude(),
                );

                div()
                    .id("sidebar-container")
                    .relative()
                    .h_full()
                    .w(sidebar_width)
                    .flex_shrink_0()
                    .child(sidebar_handle.to_any())
                    .child(resize_handle)
                    .into_any_element()
            })
        } else {
            None
        };

        let ui_font = theme::setup_ui_font(window, cx);
        let text_color = cx.theme().colors().text;

        let workspace = self.workspace().clone();
        let workspace_key_context = workspace.update(cx, |workspace, cx| workspace.key_context(cx));
        let titlebar_item = workspace.read(cx).titlebar_item();
        let root = workspace.update(cx, |workspace, cx| workspace.actions(v_flex(), window, cx));

        client_side_decorations(
            root.key_context(workspace_key_context)
                .relative()
                .size_full()
                .font(ui_font)
                .text_color(text_color)
                .on_action(cx.listener(Self::close_window))
                .when(self.multi_workspace_enabled(cx), |this| {
                    this.on_action(cx.listener(
                        |this: &mut Self, _: &ToggleWorkspaceSidebar, window, cx| {
                            this.toggle_sidebar(window, cx);
                        },
                    ))
                    .on_action(cx.listener(
                        |this: &mut Self, _: &FocusWorkspaceSidebar, window, cx| {
                            this.focus_sidebar(window, cx);
                        },
                    ))
                })
                .children(titlebar_item)
                .child(
                    div()
                        .flex_1()
                        .w_full()
                        .flex()
                        .flex_row()
                        .overflow_hidden()
                        .when(
                            self.sidebar_open() && self.multi_workspace_enabled(cx),
                            |this| {
                                this.on_drag_move(cx.listener(
                                    |this: &mut Self,
                                     e: &DragMoveEvent<DraggedSidebar>,
                                     _window,
                                     cx| {
                                        if let Some(sidebar) = &this.sidebar {
                                            let new_width = e.event.position.x;
                                            sidebar.set_width(Some(new_width), cx);
                                        }
                                    },
                                ))
                                .children(sidebar)
                            },
                        )
                        .child(self.workspace().clone()),
                )
                .child(self.workspace().read(cx).modal_layer.clone()),
            window,
            cx,
            Tiling {
                left: multi_workspace_enabled && self.sidebar_open(),
                ..Tiling::default()
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use feature_flags::FeatureFlagAppExt as _;
    use fs::FakeFs;
    use gpui::TestAppContext;
    use project::DisableAiSettings;
    use settings::Settings as _;
    use settings::SettingsStore;

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
            theme::init(theme::LoadThemes::JustBase, cx);
            DisableAiSettings::register(cx);
            cx.update_flags(false, vec!["agent-v2".into()]);
        });
    }

    fn recent_workspace_ids(
        multi_workspace: &Entity<MultiWorkspace>,
        cx: &TestAppContext,
    ) -> Vec<EntityId> {
        multi_workspace.read_with(cx, |multi_workspace, _cx| {
            multi_workspace
                .workspaces_by_recent_use()
                .into_iter()
                .map(|workspace| workspace.entity_id())
                .collect::<Vec<_>>()
        })
    }

    fn active_workspace_id(
        multi_workspace: &Entity<MultiWorkspace>,
        cx: &TestAppContext,
    ) -> EntityId {
        multi_workspace.read_with(cx, |multi_workspace, _cx| {
            multi_workspace.workspace().entity_id()
        })
    }

    #[gpui::test]
    async fn test_sidebar_disabled_when_disable_ai_is_enabled(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, [], cx).await;

        let (multi_workspace, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project, window, cx));

        multi_workspace.read_with(cx, |mw, cx| {
            assert!(mw.multi_workspace_enabled(cx));
        });

        multi_workspace.update_in(cx, |mw, _window, cx| {
            mw.open_sidebar(cx);
            assert!(mw.sidebar_open());
        });

        cx.update(|_window, cx| {
            DisableAiSettings::override_global(DisableAiSettings { disable_ai: true }, cx);
        });
        cx.run_until_parked();

        multi_workspace.read_with(cx, |mw, cx| {
            assert!(
                !mw.sidebar_open(),
                "Sidebar should be closed when disable_ai is true"
            );
            assert!(
                !mw.multi_workspace_enabled(cx),
                "Multi-workspace should be disabled when disable_ai is true"
            );
        });

        multi_workspace.update_in(cx, |mw, window, cx| {
            mw.toggle_sidebar(window, cx);
        });
        multi_workspace.read_with(cx, |mw, _cx| {
            assert!(
                !mw.sidebar_open(),
                "Sidebar should remain closed when toggled with disable_ai true"
            );
        });

        cx.update(|_window, cx| {
            DisableAiSettings::override_global(DisableAiSettings { disable_ai: false }, cx);
        });
        cx.run_until_parked();

        multi_workspace.read_with(cx, |mw, cx| {
            assert!(
                mw.multi_workspace_enabled(cx),
                "Multi-workspace should be enabled after re-enabling AI"
            );
            assert!(
                !mw.sidebar_open(),
                "Sidebar should still be closed after re-enabling AI (not auto-opened)"
            );
        });

        multi_workspace.update_in(cx, |mw, window, cx| {
            mw.toggle_sidebar(window, cx);
        });
        multi_workspace.read_with(cx, |mw, _cx| {
            assert!(
                mw.sidebar_open(),
                "Sidebar should open when toggled after re-enabling AI"
            );
        });
    }

    #[gpui::test]
    async fn test_workspaces_by_recent_use_tracks_activation_order(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        let project_a = Project::test(fs.clone(), [], cx).await;
        let project_b = Project::test(fs.clone(), [], cx).await;
        let project_c = Project::test(fs, [], cx).await;

        let (multi_workspace, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project_a, window, cx));

        let workspace_a = multi_workspace.read_with(cx, |multi_workspace, _cx| {
            multi_workspace.workspace().clone()
        });
        let workspace_b = multi_workspace.update_in(cx, |multi_workspace, window, cx| {
            multi_workspace.test_add_workspace(project_b, window, cx)
        });
        let workspace_c = multi_workspace.update_in(cx, |multi_workspace, window, cx| {
            multi_workspace.test_add_workspace(project_c, window, cx)
        });

        assert_eq!(
            recent_workspace_ids(&multi_workspace, cx),
            vec![
                workspace_c.entity_id(),
                workspace_b.entity_id(),
                workspace_a.entity_id()
            ]
        );

        multi_workspace.update_in(cx, |multi_workspace, _window, cx| {
            multi_workspace.activate(workspace_a.clone(), cx);
        });
        assert_eq!(
            recent_workspace_ids(&multi_workspace, cx),
            vec![
                workspace_a.entity_id(),
                workspace_c.entity_id(),
                workspace_b.entity_id()
            ]
        );

        multi_workspace.update_in(cx, |multi_workspace, _window, cx| {
            multi_workspace.activate(workspace_b.clone(), cx);
        });
        assert_eq!(
            recent_workspace_ids(&multi_workspace, cx),
            vec![
                workspace_b.entity_id(),
                workspace_a.entity_id(),
                workspace_c.entity_id()
            ]
        );
    }

    #[gpui::test]
    async fn test_workspaces_by_recent_use_removes_closed_workspaces(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        let project_a = Project::test(fs.clone(), [], cx).await;
        let project_b = Project::test(fs.clone(), [], cx).await;
        let project_c = Project::test(fs, [], cx).await;

        let (multi_workspace, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project_a, window, cx));

        let workspace_a = multi_workspace.read_with(cx, |multi_workspace, _cx| {
            multi_workspace.workspace().clone()
        });
        let workspace_b = multi_workspace.update_in(cx, |multi_workspace, window, cx| {
            multi_workspace.test_add_workspace(project_b, window, cx)
        });
        let workspace_c = multi_workspace.update_in(cx, |multi_workspace, window, cx| {
            multi_workspace.test_add_workspace(project_c, window, cx)
        });

        multi_workspace.update_in(cx, |multi_workspace, window, cx| {
            multi_workspace.remove_workspace(1, window, cx);
        });
        assert_eq!(
            recent_workspace_ids(&multi_workspace, cx),
            vec![workspace_c.entity_id(), workspace_a.entity_id()]
        );
        assert!(!recent_workspace_ids(&multi_workspace, cx).contains(&workspace_b.entity_id()));
    }

    #[gpui::test]
    async fn test_activate_next_recent_workspace_cycles_through_mru_snapshot(
        cx: &mut TestAppContext,
    ) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        let project_a = Project::test(fs.clone(), [], cx).await;
        let project_b = Project::test(fs.clone(), [], cx).await;
        let project_c = Project::test(fs.clone(), [], cx).await;
        let project_d = Project::test(fs, [], cx).await;

        let (multi_workspace, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project_a, window, cx));

        let workspace_a = multi_workspace.read_with(cx, |multi_workspace, _cx| {
            multi_workspace.workspace().clone()
        });
        let workspace_b = multi_workspace.update_in(cx, |multi_workspace, window, cx| {
            multi_workspace.test_add_workspace(project_b, window, cx)
        });
        let workspace_c = multi_workspace.update_in(cx, |multi_workspace, window, cx| {
            multi_workspace.test_add_workspace(project_c, window, cx)
        });
        let workspace_d = multi_workspace.update_in(cx, |multi_workspace, window, cx| {
            multi_workspace.test_add_workspace(project_d, window, cx)
        });

        assert_eq!(
            recent_workspace_ids(&multi_workspace, cx),
            vec![
                workspace_d.entity_id(),
                workspace_c.entity_id(),
                workspace_b.entity_id(),
                workspace_a.entity_id()
            ]
        );

        multi_workspace.update_in(cx, |multi_workspace, window, cx| {
            multi_workspace.activate_next_recent_workspace(window, cx);
        });
        assert_eq!(
            active_workspace_id(&multi_workspace, cx),
            workspace_c.entity_id()
        );

        multi_workspace.update_in(cx, |multi_workspace, window, cx| {
            multi_workspace.activate_next_recent_workspace(window, cx);
        });
        assert_eq!(
            active_workspace_id(&multi_workspace, cx),
            workspace_b.entity_id()
        );

        multi_workspace.update_in(cx, |multi_workspace, window, cx| {
            multi_workspace.activate_next_recent_workspace(window, cx);
        });
        assert_eq!(
            active_workspace_id(&multi_workspace, cx),
            workspace_a.entity_id()
        );

        multi_workspace.update_in(cx, |multi_workspace, window, cx| {
            multi_workspace.activate_next_recent_workspace(window, cx);
        });
        assert_eq!(
            active_workspace_id(&multi_workspace, cx),
            workspace_d.entity_id()
        );
    }
}
