use anyhow::{Result, bail};
use editor::Editor;
use gpui::{AnyElement, Context, Entity, ScrollHandle, SharedString, Subscription, Window};
use std::collections::BTreeMap;
use superzent_model::{AgentPreset, AgentPresetDraft, SuperzentStore};
use ui::{Button, ButtonStyle, Color, Divider, IconButton, IconName, Label, Tooltip, prelude::*};

use crate::SettingsWindow;

pub(crate) fn render_superzent_agent_presets_page(
    _settings_window: &SettingsWindow,
    scroll_handle: &ScrollHandle,
    window: &mut Window,
    cx: &mut Context<SettingsWindow>,
) -> AnyElement {
    let page = window.use_keyed_state("superzent-agent-presets-page", cx, |_, cx| {
        AgentPresetsPage::new(cx)
    });

    v_flex()
        .id("superzent-agent-presets-page")
        .min_w_0()
        .min_h_0()
        .w_full()
        .h_full()
        .pt_2p5()
        .px_8()
        .overflow_y_scroll()
        .track_scroll(scroll_handle)
        .child(v_flex().w_full().pb_16().child(page))
        .into_any_element()
}

struct AgentPresetsPage {
    store: Entity<SuperzentStore>,
    last_error: Option<SharedString>,
    _subscription: Subscription,
}

impl AgentPresetsPage {
    fn new(cx: &mut Context<Self>) -> Self {
        let store = SuperzentStore::global(cx);
        let subscription = cx.observe(&store, |_, _, cx| cx.notify());

        Self {
            store,
            last_error: None,
            _subscription: subscription,
        }
    }

    fn clear_error(&mut self, cx: &mut Context<Self>) {
        if self.last_error.is_some() {
            self.last_error = None;
            cx.notify();
        }
    }

    fn set_error(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.last_error = Some(message.into());
        cx.notify();
    }

    fn add_preset(&mut self, cx: &mut Context<Self>) {
        let presets = self.store.read(cx).presets().to_vec();
        let mut draft = presets
            .first()
            .map(AgentPresetDraft::from)
            .unwrap_or_else(|| AgentPresetDraft {
                label: "New Preset".to_string(),
                command: "codex".to_string(),
                args: Vec::new(),
                env: BTreeMap::new(),
                attention_patterns: Vec::new(),
            });
        draft.label = next_new_preset_label(&presets);
        draft.attention_patterns.clear();

        match self
            .store
            .update(cx, |store, cx| store.create_preset(draft, cx))
        {
            Ok(_) => self.clear_error(cx),
            Err(error) => self.set_error(format!("Failed to add preset: {error}"), cx),
        }
    }

    fn move_preset(&mut self, preset_id: &str, offset: isize, cx: &mut Context<Self>) {
        let presets = self.store.read(cx).presets().to_vec();
        let Some(source_index) = presets.iter().position(|preset| preset.id == preset_id) else {
            self.set_error("Preset not found.", cx);
            return;
        };
        let Some(target_index) = source_index.checked_add_signed(offset) else {
            return;
        };
        if target_index >= presets.len() {
            return;
        }

        let target_preset_id = presets[target_index].id.clone();
        self.store.update(cx, |store, cx| {
            store.reorder_preset(preset_id, Some(&target_preset_id), cx);
        });
        self.clear_error(cx);
    }

    fn delete_preset(&mut self, preset_id: &str, cx: &mut Context<Self>) {
        match self
            .store
            .update(cx, |store, cx| store.delete_preset(preset_id, cx))
        {
            Ok(()) => self.clear_error(cx),
            Err(error) => self.set_error(format!("Failed to delete preset: {error}"), cx),
        }
    }

    fn save_preset(
        &mut self,
        preset_id: &str,
        label_editor: &Entity<Editor>,
        command_editor: &Entity<Editor>,
        args_editor: &Entity<Editor>,
        env_editor: &Entity<Editor>,
        attention_patterns: &[String],
        cx: &mut Context<Self>,
    ) {
        let draft = parse_preset_draft(
            label_editor.read(cx).text(cx),
            command_editor.read(cx).text(cx),
            args_editor.read(cx).text(cx),
            env_editor.read(cx).text(cx),
        )
        .map(|mut draft| {
            draft.attention_patterns = attention_patterns.to_vec();
            draft
        });

        match draft.and_then(|draft| {
            self.store
                .update(cx, |store, cx| store.update_preset(preset_id, draft, cx))
        }) {
            Ok(()) => self.clear_error(cx),
            Err(error) => self.set_error(format!("Failed to save preset: {error}"), cx),
        }
    }

    fn reset_editor_text(
        editor: &Entity<Editor>,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        editor.update(cx, |editor, cx| {
            editor.set_text(text, window, cx);
        });
    }

    fn render_preset_card(
        &self,
        preset: &AgentPreset,
        index: usize,
        total_presets: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let label_editor = preset_editor(
            format!("superzent-preset-label-{}", preset.id),
            &preset.label,
            false,
            "Preset name",
            window,
            cx,
        );
        let command_editor = preset_editor(
            format!("superzent-preset-command-{}", preset.id),
            &preset.command,
            false,
            "Executable to launch",
            window,
            cx,
        );
        let args_editor = preset_editor(
            format!("superzent-preset-args-{}", preset.id),
            &preset.args.join("\n"),
            true,
            "One argument per line",
            window,
            cx,
        );
        let env_editor = preset_editor(
            format!("superzent-preset-env-{}", preset.id),
            &render_env_lines(&preset.env),
            true,
            "KEY=VALUE per line",
            window,
            cx,
        );

        v_flex()
            .min_w_0()
            .w_full()
            .gap_3()
            .p_4()
            .border_1()
            .border_color(cx.theme().colors().border_variant)
            .rounded_lg()
            .bg(cx.theme().colors().editor_background)
            .child(
                h_flex()
                    .min_w_0()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        v_flex()
                            .flex_1()
                            .gap_0p5()
                            .min_w_0()
                            .child(Label::new(preset.label.clone()).size(LabelSize::Small))
                            .child(
                                Label::new(format!("ID: {}", preset.id))
                                    .size(LabelSize::XSmall)
                                    .color(Color::Muted),
                            ),
                    )
                    .child(
                        h_flex()
                            .flex_shrink_0()
                            .items_center()
                            .gap_1()
                            .child(
                                IconButton::new(
                                    format!("superzent-preset-up-{}", preset.id),
                                    IconName::ArrowUp,
                                )
                                .style(ButtonStyle::Subtle)
                                .disabled(index == 0)
                                .tooltip(|window, cx| Tooltip::text("Move up")(window, cx))
                                .on_click(cx.listener({
                                    let preset_id = preset.id.clone();
                                    move |this, _, _, cx| this.move_preset(&preset_id, -1, cx)
                                })),
                            )
                            .child(
                                IconButton::new(
                                    format!("superzent-preset-down-{}", preset.id),
                                    IconName::ArrowDown,
                                )
                                .style(ButtonStyle::Subtle)
                                .disabled(index + 1 == total_presets)
                                .tooltip(|window, cx| Tooltip::text("Move down")(window, cx))
                                .on_click(cx.listener({
                                    let preset_id = preset.id.clone();
                                    move |this, _, _, cx| this.move_preset(&preset_id, 1, cx)
                                })),
                            )
                            .child(
                                IconButton::new(
                                    format!("superzent-preset-delete-{}", preset.id),
                                    IconName::Trash,
                                )
                                .style(ButtonStyle::Subtle)
                                .disabled(total_presets <= 1)
                                .tooltip(|window, cx| Tooltip::text("Delete preset")(window, cx))
                                .on_click(cx.listener({
                                    let preset_id = preset.id.clone();
                                    move |this, _, _, cx| this.delete_preset(&preset_id, cx)
                                })),
                            ),
                    ),
            )
            .child(render_field("Label", None, label_editor.clone()))
            .child(render_field("Command", None, command_editor.clone()))
            .child(render_field(
                "Arguments",
                Some("One argument per line"),
                args_editor.clone(),
            ))
            .child(render_field(
                "Environment",
                Some("KEY=VALUE per line"),
                env_editor.clone(),
            ))
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new(format!("superzent-preset-reset-{}", preset.id), "Reset")
                            .style(ButtonStyle::Subtle)
                            .on_click(cx.listener({
                                let reset_label_editor = label_editor.clone();
                                let reset_command_editor = command_editor.clone();
                                let reset_args_editor = args_editor.clone();
                                let reset_env_editor = env_editor.clone();
                                let label = preset.label.clone();
                                let command = preset.command.clone();
                                let args = preset.args.join("\n");
                                let env = render_env_lines(&preset.env);
                                move |this, _, window, cx| {
                                    Self::reset_editor_text(
                                        &reset_label_editor,
                                        label.clone(),
                                        window,
                                        cx,
                                    );
                                    Self::reset_editor_text(
                                        &reset_command_editor,
                                        command.clone(),
                                        window,
                                        cx,
                                    );
                                    Self::reset_editor_text(
                                        &reset_args_editor,
                                        args.clone(),
                                        window,
                                        cx,
                                    );
                                    Self::reset_editor_text(
                                        &reset_env_editor,
                                        env.clone(),
                                        window,
                                        cx,
                                    );
                                    this.clear_error(cx);
                                }
                            })),
                    )
                    .child(
                        Button::new(format!("superzent-preset-save-{}", preset.id), "Save")
                            .style(ButtonStyle::Filled)
                            .on_click(cx.listener({
                                let preset_id = preset.id.clone();
                                let attention_patterns = preset.attention_patterns.clone();
                                move |this, _, _, cx| {
                                    this.save_preset(
                                        &preset_id,
                                        &label_editor,
                                        &command_editor,
                                        &args_editor,
                                        &env_editor,
                                        &attention_patterns,
                                        cx,
                                    );
                                }
                            })),
                    ),
            )
            .into_any_element()
    }
}

impl Render for AgentPresetsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let presets = self.store.read(cx).presets().to_vec();

        v_flex()
            .min_w_0()
            .w_full()
            .gap_4()
            .child(
                v_flex()
                    .min_w_0()
                    .w_full()
                    .gap_1()
                    .child(Label::new("Agent Presets").size(LabelSize::Large))
                    .child(
                        Label::new(
                            "Configure the preset buttons shown below terminal tabs and the default agent launch settings for managed workspaces.",
                        )
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                    ),
            )
            .when_some(self.last_error.clone(), |this, error| {
                this.child(
                    Label::new(error)
                        .size(LabelSize::Small)
                        .color(Color::Error),
                )
            })
            .child(
                h_flex()
                    .w_full()
                    .justify_end()
                    .child(
                        Button::new("superzent-add-preset", "Add Preset")
                            .style(ButtonStyle::Filled)
                            .icon(IconName::Plus)
                            .on_click(cx.listener(|this, _, _, cx| this.add_preset(cx))),
                    ),
            )
            .children(presets.iter().enumerate().flat_map(|(index, preset)| {
                let mut elements = vec![self.render_preset_card(
                    preset,
                    index,
                    presets.len(),
                    window,
                    cx,
                )];
                if index + 1 < presets.len() {
                    elements.push(Divider::horizontal().into_any_element());
                }
                elements
            }))
    }
}

fn render_field(
    title: &'static str,
    description: Option<&'static str>,
    editor: Entity<Editor>,
) -> AnyElement {
    v_flex()
        .min_w_0()
        .w_full()
        .gap_1()
        .child(Label::new(title).size(LabelSize::Small))
        .when_some(description, |this, description| {
            this.child(
                Label::new(description)
                    .size(LabelSize::XSmall)
                    .color(Color::Muted),
            )
        })
        .child(div().min_w_0().w_full().child(editor))
        .into_any_element()
}

fn preset_editor(
    id: impl Into<gpui::ElementId>,
    expected_text: &str,
    multi_line: bool,
    placeholder: &'static str,
    window: &mut Window,
    cx: &mut Context<AgentPresetsPage>,
) -> Entity<Editor> {
    let expected_text = expected_text.to_string();
    let editor = window.use_keyed_state(id, cx, {
        let expected_text = expected_text.clone();
        move |window, cx| {
            let mut editor = if multi_line {
                Editor::auto_height_unbounded(2, window, cx)
            } else {
                Editor::single_line(window, cx)
            };
            editor.set_placeholder_text(placeholder, window, cx);
            editor.set_text(expected_text, window, cx);
            editor
        }
    });

    sync_editor_text(&editor, &expected_text, window, cx);
    editor
}

fn sync_editor_text(
    editor: &Entity<Editor>,
    expected_text: &str,
    window: &mut Window,
    cx: &mut Context<AgentPresetsPage>,
) {
    let current_text = editor.read(cx).text(cx);
    if current_text != expected_text && !editor.read(cx).is_focused(window) {
        editor.update(cx, |editor, cx| {
            editor.set_text(expected_text.to_string(), window, cx);
        });
    }
}

fn next_new_preset_label(existing_presets: &[AgentPreset]) -> String {
    let mut index = 1usize;
    loop {
        let label = if index == 1 {
            "New Preset".to_string()
        } else {
            format!("New Preset {index}")
        };
        if existing_presets.iter().all(|preset| preset.label != label) {
            return label;
        }
        index += 1;
    }
}

fn render_env_lines(environment: &BTreeMap<String, String>) -> String {
    environment
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_preset_draft(
    label: String,
    command: String,
    arguments: String,
    environment: String,
) -> Result<AgentPresetDraft> {
    let args = arguments
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();

    let mut environment_map = BTreeMap::new();
    for line in environment
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let Some((key, value)) = line.split_once('=') else {
            bail!("Environment lines must use KEY=VALUE.");
        };
        let key = key.trim();
        if key.is_empty() {
            bail!("Environment keys cannot be empty.");
        }
        environment_map.insert(key.to_string(), value.trim().to_string());
    }

    Ok(AgentPresetDraft {
        label,
        command,
        args,
        env: environment_map,
        attention_patterns: Vec::new(),
    })
}
