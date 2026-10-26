use gpui::{Action, Context, EventEmitter, IntoElement, Render, Subscription, WeakEntity, Window};
use ui::{IconButton, IconName, IconSize, Tooltip, prelude::*};
use workspace::item::ItemHandle;
use workspace::{ToolbarItemEvent, ToolbarItemLocation, ToolbarItemView, Workspace};

use crate::markdown_split_view::{self, MarkdownLayout, MarkdownSplitView};
use crate::{SwitchToEditor, SwitchToPreview, SwitchToSideBySide};

/// Toolbar buttons that switch a Markdown tab between the editor, the editor and the preview side
/// by side, and the preview.
pub struct MarkdownLayoutToolbar {
    workspace: WeakEntity<Workspace>,
    active_item: Option<Box<dyn ItemHandle>>,
    layout: Option<MarkdownLayout>,
    _subscriptions: Vec<Subscription>,
}

impl MarkdownLayoutToolbar {
    pub fn new(workspace: WeakEntity<Workspace>) -> Self {
        Self {
            workspace,
            active_item: None,
            layout: None,
            _subscriptions: Vec::new(),
        }
    }

    fn location(&self) -> ToolbarItemLocation {
        match self.layout {
            Some(_) => ToolbarItemLocation::PrimaryRight,
            None => ToolbarItemLocation::Hidden,
        }
    }

    fn refresh_layout(&mut self, cx: &mut Context<Self>) {
        let layout = self.current_layout(cx);
        if layout == self.layout {
            return;
        }
        let was_visible = self.layout.is_some();
        self.layout = layout;
        if was_visible != layout.is_some() {
            cx.emit(ToolbarItemEvent::ChangeLocation(self.location()));
        }
        cx.notify();
    }

    fn current_layout(&self, cx: &App) -> Option<MarkdownLayout> {
        MarkdownLayout::of_item(self.active_item.as_deref()?, cx)
    }

    fn render_button(
        &self,
        layout: MarkdownLayout,
        current_layout: MarkdownLayout,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (id, icon, tooltip, action): (_, _, _, Box<dyn Action>) = match layout {
            MarkdownLayout::Editor => (
                "markdown-layout-editor",
                IconName::FileCode,
                "Editor Only",
                Box::new(SwitchToEditor),
            ),
            MarkdownLayout::SideBySide => (
                "markdown-layout-side-by-side",
                IconName::SplitAlt,
                "Editor and Preview",
                Box::new(SwitchToSideBySide),
            ),
            MarkdownLayout::Preview => (
                "markdown-layout-preview",
                IconName::Eye,
                "Preview Only",
                Box::new(SwitchToPreview),
            ),
        };
        IconButton::new(id, icon)
            .icon_size(IconSize::Small)
            .style(ButtonStyle::Subtle)
            .toggle_state(layout == current_layout)
            .tooltip(move |_window, cx| Tooltip::for_action(tooltip, action.as_ref(), cx))
            .on_click(
                cx.listener(move |this, _, window, cx| this.switch_active_item(layout, window, cx)),
            )
    }

    fn switch_active_item(
        &mut self,
        layout: MarkdownLayout,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(item) = self.active_item.as_ref().map(|item| item.boxed_clone()) else {
            return;
        };
        let workspace = self.workspace.clone();
        // Switching layouts updates the pane's toolbar, this item included.
        window.defer(cx, move |window, cx| {
            workspace
                .update(cx, |workspace, cx| {
                    markdown_split_view::switch_layout(
                        workspace,
                        item.as_ref(),
                        layout,
                        window,
                        cx,
                    );
                })
                .ok();
        });
    }
}

impl EventEmitter<ToolbarItemEvent> for MarkdownLayoutToolbar {}

impl Render for MarkdownLayoutToolbar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(current_layout) = self.layout else {
            return h_flex().id("markdown-layout-toolbar");
        };
        h_flex()
            .id("markdown-layout-toolbar")
            .gap_0p5()
            .child(self.render_button(MarkdownLayout::Editor, current_layout, cx))
            .child(self.render_button(MarkdownLayout::SideBySide, current_layout, cx))
            .child(self.render_button(MarkdownLayout::Preview, current_layout, cx))
    }
}

impl ToolbarItemView for MarkdownLayoutToolbar {
    fn set_active_pane_item(
        &mut self,
        active_pane_item: Option<&dyn ItemHandle>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> ToolbarItemLocation {
        self.active_item = active_pane_item.map(ItemHandle::boxed_clone);
        self._subscriptions.clear();
        if let Some(item) = active_pane_item {
            // An editor counts as Markdown only once its language has loaded.
            let this = cx.entity().downgrade();
            self._subscriptions.push(item.subscribe_to_item_events(
                window,
                cx,
                Box::new(move |_, _, cx| {
                    this.update(cx, |this, cx| this.refresh_layout(cx)).ok();
                }),
            ));
            if let Some(split_view) = item.downcast::<MarkdownSplitView>() {
                self._subscriptions
                    .push(cx.observe(&split_view, |this, _, cx| this.refresh_layout(cx)));
            }
        }
        self.layout = self.current_layout(cx);
        cx.notify();
        self.location()
    }
}

#[cfg(test)]
mod tests {
    use editor::Editor;
    use fs::FakeFs;
    use gpui::{AppContext as _, TestAppContext};
    use project::Project;
    use serde_json::json;
    use std::sync::Arc;
    use util::{path, rel_path::rel_path};
    use workspace::{AppState, MultiWorkspace};

    use super::MarkdownLayoutToolbar;
    use crate::markdown_split_view::{MarkdownLayout, MarkdownSplitView};

    #[gpui::test]
    async fn buttons_switch_the_layout_of_their_panes_item(cx: &mut TestAppContext) {
        cx.update(|cx| {
            AppState::test(cx);
            editor::init(cx);
            crate::init(cx);
        });
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(path!("/project"), json!({ "note.md": "# Note\n" }))
            .await;
        let project = Project::test(fs, [path!("/project").as_ref()], cx).await;
        project
            .read_with(cx, |project, _| project.languages().clone())
            .add(Arc::new(language::Language::new(
                language::LanguageConfig {
                    name: "Markdown".into(),
                    matcher: language::LanguageMatcher {
                        path_suffixes: vec!["md".to_string()],
                        ..Default::default()
                    },
                    ..Default::default()
                },
                None,
            )));
        let multi_workspace =
            cx.add_window(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
        let workspace = multi_workspace
            .update(cx, |multi_workspace, _, _| {
                multi_workspace.workspace().clone()
            })
            .unwrap();
        let worktree_id = project.read_with(cx, |project, cx| {
            project.worktrees(cx).next().unwrap().read(cx).id()
        });
        let open = multi_workspace
            .update(cx, |_, window, cx| {
                workspace.update(cx, |workspace, cx| {
                    workspace.open_path((worktree_id, rel_path("note.md")), None, true, window, cx)
                })
            })
            .unwrap();
        let editor = open.await.unwrap().downcast::<Editor>().unwrap();
        cx.run_until_parked();

        let toolbar_item = multi_workspace
            .update(cx, |_, window, cx| {
                let toolbar_item = cx.new(|_| MarkdownLayoutToolbar::new(workspace.downgrade()));
                let pane = workspace.read(cx).active_pane().clone();
                pane.update(cx, |pane, cx| {
                    pane.toolbar().update(cx, |toolbar, cx| {
                        toolbar.add_item(toolbar_item.clone(), window, cx)
                    });
                });
                toolbar_item
            })
            .unwrap();

        for layout in [
            MarkdownLayout::SideBySide,
            MarkdownLayout::Preview,
            MarkdownLayout::Editor,
        ] {
            multi_workspace
                .update(cx, |_, window, cx| {
                    toolbar_item.update(cx, |toolbar_item, cx| {
                        toolbar_item.switch_active_item(layout, window, cx)
                    });
                })
                .unwrap();
            cx.run_until_parked();
            workspace.read_with(cx, |workspace, cx| {
                let item = workspace.active_item(cx).unwrap();
                assert_eq!(MarkdownLayout::of_item(item.as_ref(), cx), Some(layout));
                assert_eq!(toolbar_item.read(cx).current_layout(cx), Some(layout));
            });
        }
        workspace.read_with(cx, |workspace, cx| {
            assert_eq!(workspace.active_item_as::<Editor>(cx), Some(editor));
            assert_eq!(workspace.items_of_type::<MarkdownSplitView>(cx).count(), 0);
        });
    }
}
