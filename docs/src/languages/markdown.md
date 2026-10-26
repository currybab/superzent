---
title: Markdown
description: "Configure Markdown language support in Zed, including language servers, formatting, and debugging."
---

# Markdown

Markdown support is available natively in Zed.

- Tree-sitter: [tree-sitter-markdown](https://github.com/tree-sitter-grammars/tree-sitter-markdown)
- Language Server: N/A

## Syntax Highlighting Code Blocks

Zed supports language-specific syntax highlighting of markdown code blocks by leveraging [tree-sitter language grammars](../extensions/languages.md#grammar). All [Zed supported languages](../languages.md), including those provided by official or community extensions, are available for use in markdown code blocks. All you need to do is provide a language name after the opening <kbd>```</kbd> code fence like so:

````python
```python
import functools as ft

@ft.lru_cache(maxsize=500)
def fib(n):
    return n if n < 2 else fib(n - 1) + fib(n - 2)
```
````

## Preview

Markdown tabs have three layouts, which the buttons at the right of the tab's toolbar switch between:

- **Editor only**: a plain editor tab ({#action markdown::SwitchToEditor}).
- **Editor and preview**: the editor and the rendered preview side by side in one tab ({#action markdown::SwitchToSideBySide}). Drag the divider to resize them, or double-click it to reset.
- **Preview only**: the rendered preview, with the editor kept in the same tab ({#action markdown::SwitchToPreview}).

The tab keeps its unsaved edits when you switch layouts, and saving, closing, and restoring the tab work like an editor tab.

To open Markdown files in one of the preview layouts automatically, add to your settings file:

```json [settings]
  "markdown_preview": {
    // Open Markdown files with the editor and the preview side by side.
    "open_markdown_files_side_by_side": true,
    // Or open them in the preview only.
    "open_markdown_files_in_preview": false
  },
```

## Configuration

### Format

Zed supports using Prettier to automatically re-format Markdown documents. You can trigger this manually via the {#action editor::Format} action or via the {#kb editor::Format} keyboard shortcut. Alternately, you can enable format on save.

Configure formatting in Settings ({#kb zed::OpenSettings}) under Languages > Markdown, or add to your settings file:

```json [settings]
  "languages": {
    "Markdown": {
      "format_on_save": "on"
    }
  },
```

### List Continuation

Zed automatically continues lists when you press Enter at the end of a list item. Supported list types:

- Unordered lists (`-`, `*`, or `+` markers)
- Ordered lists (numbers are auto-incremented)
- Task lists (`- [ ]` and `- [x]`)

Pressing Enter on an empty list item removes the marker and exits the list.

To disable this behavior, configure in Settings ({#kb zed::OpenSettings}) under Languages > Markdown, or add to your settings file:

```json [settings]
  "languages": {
    "Markdown": {
      "extend_list_on_newline": false
    }
  },
```

### List Indentation

Zed indents list items when you press Tab while the cursor is on a line containing only a list marker. This allows you to quickly create nested lists.

To disable this behavior, configure in Settings ({#kb zed::OpenSettings}) under Languages > Markdown, or add to your settings file:

```json [settings]
  "languages": {
    "Markdown": {
      "indent_list_on_tab": false
    }
  },
```

### Trailing Whitespace

By default Zed will remove trailing whitespace on save. If you rely on invisible trailing whitespace being converted to `<br />` in Markdown files you can disable this behavior.

Configure in Settings ({#kb zed::OpenSettings}) under Languages > Markdown, or add to your settings file:

```json [settings]
  "languages": {
    "Markdown": {
      "remove_trailing_whitespace_on_save": false
    }
  },
```
