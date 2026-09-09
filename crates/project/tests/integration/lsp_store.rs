use std::path::Path;

use fs::FakeFs;
use futures::StreamExt;
use gpui::TestAppContext;
use language::{CodeLabel, FakeLspAdapter, HighlightId, rust_lang};
use lsp::Uri;
use project::{Project, lsp_store::*};
use serde_json::json;
use unindent::Unindent;
use util::path;

use crate::init_test;

#[gpui::test]
async fn test_removing_invisible_worktree_cleans_reused_lsp_bookkeeping(cx: &mut TestAppContext) {
    init_test(cx);
    cx.executor().allow_parking();

    let fs = FakeFs::new(cx.executor());
    fs.insert_tree(path!("/the-root"), json!({ "main.rs": "fn main() {}" }))
        .await;
    fs.insert_tree(
        path!("/the-registry"),
        json!({ "dep": { "src": { "dep.rs": "pub fn dep() {}" } } }),
    )
    .await;

    let project = Project::test(fs, [path!("/the-root").as_ref()], cx).await;
    let language_registry = project.read_with(cx, |project, _| project.languages().clone());
    language_registry.add(rust_lang());
    let mut fake_servers = language_registry.register_fake_lsp("Rust", FakeLspAdapter::default());

    let (_visible_buffer, _visible_handle) = project
        .update(cx, |project, cx| {
            project.open_local_buffer_with_lsp(path!("/the-root/main.rs"), cx)
        })
        .await
        .unwrap();
    fake_servers.next().await.unwrap();
    cx.run_until_parked();

    let server_id = project.read_with(cx, |project, cx| {
        project
            .lsp_store()
            .read(cx)
            .language_server_statuses()
            .next()
            .unwrap()
            .0
    });
    let external_buffer = project
        .update(cx, |project, cx| {
            project.open_local_buffer_via_lsp(
                Uri::from_file_path(path!("/the-registry/dep/src/dep.rs")).unwrap(),
                server_id,
                cx,
            )
        })
        .await
        .unwrap();
    cx.run_until_parked();

    let invisible_worktree_id =
        external_buffer.read_with(cx, |buffer, cx| buffer.file().unwrap().worktree_id(cx));
    project.read_with(cx, |project, cx| {
        let worktree = project.worktree_for_id(invisible_worktree_id, cx).unwrap();
        assert!(!worktree.read(cx).is_visible());
        assert!(
            project
                .lsp_store()
                .read(cx)
                .has_language_server_seed_for_worktree(invisible_worktree_id)
        );
    });

    project.update(cx, |project, cx| {
        project.remove_worktree(invisible_worktree_id, cx);
    });
    cx.run_until_parked();

    project.read_with(cx, |project, cx| {
        let lsp_store = project.lsp_store();
        let lsp_store = lsp_store.read(cx);
        assert!(
            lsp_store
                .language_server_statuses()
                .any(|(status_server_id, _)| status_server_id == server_id)
        );
        assert!(!lsp_store.has_language_server_seed_for_worktree(invisible_worktree_id));
    });
}

#[test]
fn test_glob_literal_prefix() {
    assert_eq!(glob_literal_prefix(Path::new("**/*.js")), Path::new(""));
    assert_eq!(
        glob_literal_prefix(Path::new("node_modules/**/*.js")),
        Path::new("node_modules")
    );
    assert_eq!(
        glob_literal_prefix(Path::new("foo/{bar,baz}.js")),
        Path::new("foo")
    );
    assert_eq!(
        glob_literal_prefix(Path::new("foo/bar/baz.js")),
        Path::new("foo/bar/baz.js")
    );

    #[cfg(target_os = "windows")]
    {
        assert_eq!(glob_literal_prefix(Path::new("**\\*.js")), Path::new(""));
        assert_eq!(
            glob_literal_prefix(Path::new("node_modules\\**/*.js")),
            Path::new("node_modules")
        );
        assert_eq!(
            glob_literal_prefix(Path::new("foo/{bar,baz}.js")),
            Path::new("foo")
        );
        assert_eq!(
            glob_literal_prefix(Path::new("foo\\bar\\baz.js")),
            Path::new("foo/bar/baz.js")
        );
    }
}

#[test]
fn test_multi_len_chars_normalization() {
    let mut label = CodeLabel::new(
        "myElˇ (parameter) myElˇ: {\n    foo: string;\n}".to_string(),
        0..6,
        vec![(0..6, HighlightId(1))],
    );
    ensure_uniform_list_compatible_label(&mut label);
    assert_eq!(
        label,
        CodeLabel::new(
            "myElˇ (parameter) myElˇ: { foo: string; }".to_string(),
            0..6,
            vec![(0..6, HighlightId(1))],
        )
    );
}

#[test]
fn test_completion_label_snippet_normalization() {
    for line_ending in ["\n", "\r\n", "\r"] {
        let text = "
            #[cfg(test)]
            mod tests {
                use super::*;

                #[test]
                fn test_name() {

                }
            }"
        .unindent()
        .replace('\n', line_ending);
        let name_start = text.find("test_name").expect("snippet has a test name");
        let text_len = text.len();
        let mut label = CodeLabel::new(
            text,
            0..text_len,
            vec![
                (0..12, HighlightId(1)),
                (name_start..name_start + 9, HighlightId::TABSTOP_REPLACE_ID),
            ],
        );

        ensure_uniform_list_compatible_label(&mut label);

        assert_eq!(
            label,
            CodeLabel::new(
                "#[cfg(test)] mod tests { use super::*; #[test] fn test_name() { } }".to_string(),
                0..67,
                vec![
                    (0..12, HighlightId(1)),
                    (50..59, HighlightId::TABSTOP_REPLACE_ID),
                ],
            ),
            "line ending: {line_ending:?}",
        );
    }
}

#[test]
fn test_completion_label_unicode_normalization() {
    for line_ending in ["\n", "\r\n", "\r"] {
        let text = "
            héllo {
                🦀value: 世界,
            }"
        .unindent()
        .replace('\n', line_ending);
        let value_start = text.find("🦀value").expect("label has a value");
        let type_start = text.find("世界").expect("label has a type");
        let text_len = text.len();
        let mut label = CodeLabel::new(
            text,
            value_start..value_start + 9,
            vec![
                (0..text_len, HighlightId(0)),
                (0..6, HighlightId(1)),
                (
                    value_start..value_start + 9,
                    HighlightId::TABSTOP_REPLACE_ID,
                ),
                (type_start..type_start + 6, HighlightId(2)),
            ],
        );

        ensure_uniform_list_compatible_label(&mut label);

        assert_eq!(
            label,
            CodeLabel::new(
                "héllo { 🦀value: 世界, }".to_string(),
                9..18,
                vec![
                    (0..29, HighlightId(0)),
                    (0..6, HighlightId(1)),
                    (9..18, HighlightId::TABSTOP_REPLACE_ID),
                    (20..26, HighlightId(2)),
                ],
            ),
            "line ending: {line_ending:?}",
        );
    }
}

#[test]
fn test_completion_label_whitespace_normalization() {
    for line_ending in ["\n", "\r\n", "\r", "\n\r", "\r\r\n"] {
        for before in ["", " ", " \t "] {
            for after in ["", " ", " \t "] {
                let mut label = CodeLabel::plain(format!("{before}{line_ending}{after}"), None);
                ensure_uniform_list_compatible_label(&mut label);
                assert_eq!(label, CodeLabel::plain(" ".to_string(), None));
            }
        }
    }

    for text in ["", " ", " \t ", "héllo  世界", "héllo\t世界"] {
        let mut label = CodeLabel::plain(text.to_string(), None);
        let expected = label.clone();
        ensure_uniform_list_compatible_label(&mut label);
        assert_eq!(label, expected);
    }
}

#[test]
fn test_trailing_newline_in_completion_documentation() {
    let doc =
        lsp::Documentation::String("Inappropriate argument value (of correct type).\n".to_string());
    let completion_doc: CompletionDocumentation = doc.into();
    assert!(
        matches!(completion_doc, CompletionDocumentation::SingleLine(s) if s == "Inappropriate argument value (of correct type).")
    );

    let doc = lsp::Documentation::String("  some value  \n".to_string());
    let completion_doc: CompletionDocumentation = doc.into();
    assert!(matches!(
        completion_doc,
        CompletionDocumentation::SingleLine(s) if s == "some value"
    ));
}
