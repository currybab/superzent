use gpui::{App, actions};
use workspace::Workspace;

pub mod markdown_layout_toolbar;
pub mod markdown_preview_settings;
pub mod markdown_preview_view;
pub mod markdown_split_view;

pub use zed_actions::preview::markdown::{OpenPreview, OpenPreviewToTheSide};

use crate::markdown_preview_view::MarkdownPreviewView;
use crate::markdown_split_view::{MarkdownLayout, MarkdownSplitView};

actions!(
    markdown,
    [
        /// Scrolls up by one page in the markdown preview.
        #[action(deprecated_aliases = ["markdown::MovePageUp"])]
        ScrollPageUp,
        /// Scrolls down by one page in the markdown preview.
        #[action(deprecated_aliases = ["markdown::MovePageDown"])]
        ScrollPageDown,
        /// Scrolls up by approximately one visual line.
        ScrollUp,
        /// Scrolls down by approximately one visual line.
        ScrollDown,
        /// Scrolls up by one markdown element in the markdown preview
        ScrollUpByItem,
        /// Scrolls down by one markdown element in the markdown preview
        ScrollDownByItem,
        /// Scrolls to the top of the markdown preview.
        ScrollToTop,
        /// Scrolls to the bottom of the markdown preview.
        ScrollToBottom,
        /// Opens a following markdown preview that syncs with the editor.
        OpenFollowingPreview,
        /// Closes the markdown preview and returns focus to the source editor.
        CloseAndReturnToEditor,
        /// Shows the active Markdown file in a plain editor tab.
        SwitchToEditor,
        /// Shows the active Markdown file's editor and preview side by side in one tab.
        SwitchToSideBySide,
        /// Shows only the preview of the active Markdown file in its tab.
        SwitchToPreview
    ]
);

pub fn init(cx: &mut App) {
    workspace::register_serializable_item::<MarkdownPreviewView>(cx);
    workspace::register_serializable_item::<MarkdownSplitView>(cx);

    cx.observe_new(|workspace: &mut Workspace, window, cx| {
        let Some(window) = window else {
            return;
        };
        markdown_preview_view::MarkdownPreviewView::register(workspace, window, cx);
        register_layout_actions(workspace);
    })
    .detach();
}

fn register_layout_actions(workspace: &mut Workspace) {
    fn switch_active_item(
        workspace: &mut Workspace,
        layout: MarkdownLayout,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Workspace>,
    ) {
        let Some(item) = workspace.active_pane().read(cx).active_item() else {
            return;
        };
        if MarkdownLayout::of_item(item.as_ref(), cx).is_some() {
            markdown_split_view::switch_layout(workspace, item.as_ref(), layout, window, cx);
        }
    }

    workspace.register_action(|workspace, _: &SwitchToEditor, window, cx| {
        switch_active_item(workspace, MarkdownLayout::Editor, window, cx)
    });
    workspace.register_action(|workspace, _: &SwitchToSideBySide, window, cx| {
        switch_active_item(workspace, MarkdownLayout::SideBySide, window, cx)
    });
    workspace.register_action(|workspace, _: &SwitchToPreview, window, cx| {
        switch_active_item(workspace, MarkdownLayout::Preview, window, cx)
    });
}
