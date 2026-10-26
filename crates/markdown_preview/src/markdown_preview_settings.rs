use gpui::{Pixels, px};
use settings::{RegisterSetting, Settings};

use crate::markdown_split_view::MarkdownLayout;

/// The settings for the markdown preview.
#[derive(Clone, Copy, Debug, Default, RegisterSetting)]
pub struct MarkdownPreviewSettings {
    /// Whether to automatically open Markdown files in the preview.
    pub open_markdown_files_in_preview: bool,
    /// Whether to automatically open Markdown files with the editor and the preview side by side.
    pub open_markdown_files_side_by_side: bool,
    /// The maximum width of the rendered markdown content, or `None` to render
    /// content edge to edge.
    pub max_width: Option<Pixels>,
}

impl MarkdownPreviewSettings {
    /// The layout newly opened Markdown files switch to, if any.
    pub fn layout_for_opened_files(&self) -> Option<MarkdownLayout> {
        if self.open_markdown_files_side_by_side {
            Some(MarkdownLayout::SideBySide)
        } else if self.open_markdown_files_in_preview {
            Some(MarkdownLayout::Preview)
        } else {
            None
        }
    }
}

impl Settings for MarkdownPreviewSettings {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        let content = content.markdown_preview.clone().unwrap_or_default();
        let max_width = if content.limit_content_width.unwrap_or(true) {
            content.max_width.map(px)
        } else {
            None
        };
        Self {
            open_markdown_files_in_preview: content.open_markdown_files_in_preview.unwrap_or(false),
            open_markdown_files_side_by_side: content
                .open_markdown_files_side_by_side
                .unwrap_or(false),
            max_width,
        }
    }
}
