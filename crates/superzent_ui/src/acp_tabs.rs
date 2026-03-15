use agent_ui::{AgentPanel, ExternalAgent, NewExternalAgentThread, OpenHistory};
use gpui::{Action, App, Context, Window, actions};
use project::agent_server_store::{CLAUDE_AGENT_NAME, CODEX_NAME, GEMINI_NAME};
use schemars::JsonSchema;
use serde::Deserialize;
use settings::update_settings_file;
use workspace::Workspace;
use zed_actions::assistant::ToggleFocus;

actions!(superzent, [FocusAcpTab]);

#[derive(Clone, Default, PartialEq, Deserialize, JsonSchema, Action)]
#[action(namespace = superzent)]
#[serde(deny_unknown_fields)]
pub struct NewAcpTab {
    #[serde(default)]
    pub agent_name: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
}

#[derive(Clone, Default, PartialEq, Deserialize, JsonSchema, Action)]
#[action(namespace = superzent)]
#[serde(deny_unknown_fields)]
pub struct OpenAcpHistory {
    #[serde(default)]
    pub agent_name: Option<String>,
}

pub(crate) fn init(cx: &mut App) {
    cx.observe_new(
        |workspace: &mut Workspace, _window, _cx: &mut Context<Workspace>| {
            workspace
                .register_action(run_new_acp_tab)
                .register_action(run_open_acp_history)
                .register_action(run_focus_acp_tab)
                .register_action(|workspace, action: &NewExternalAgentThread, window, cx| {
                    if workspace.panel::<AgentPanel>(cx).is_some() {
                        return;
                    }

                    let agent_name = match action.agent() {
                        Some(ExternalAgent::Custom { name }) => Some(name.to_string()),
                        Some(ExternalAgent::NativeAgent) => {
                            show_native_agent_toast(workspace, cx);
                            return;
                        }
                        None => None,
                    };

                    run_new_acp_tab(
                        workspace,
                        &NewAcpTab {
                            agent_name,
                            prompt: None,
                        },
                        window,
                        cx,
                    );
                })
                .register_action(|workspace, _: &OpenHistory, window, cx| {
                    if workspace.panel::<AgentPanel>(cx).is_some() {
                        return;
                    }

                    run_open_acp_history(
                        workspace,
                        &OpenAcpHistory { agent_name: None },
                        window,
                        cx,
                    );
                })
                .register_action(|workspace, _: &ToggleFocus, window, cx| {
                    if workspace.panel::<AgentPanel>(cx).is_some() {
                        return;
                    }

                    run_focus_acp_tab(workspace, &FocusAcpTab, window, cx);
                });
        },
    )
    .detach();
}

pub(crate) fn ensure_promoted_agent_enabled(agent_name: &str, cx: &mut App) {
    if matches!(agent_name, CLAUDE_AGENT_NAME | CODEX_NAME | GEMINI_NAME) {
        enable_promoted_agent(agent_name, cx);
    }
}

pub(crate) fn run_new_acp_tab(
    workspace: &mut Workspace,
    action: &NewAcpTab,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    agent_ui::open_external_acp_tab(
        workspace,
        action.agent_name.clone(),
        action.prompt.clone(),
        window,
        cx,
    );
}

pub(crate) fn run_open_acp_history(
    workspace: &mut Workspace,
    action: &OpenAcpHistory,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let agent_name = action
        .agent_name
        .clone()
        .or_else(|| agent_ui::active_external_acp_agent_name(workspace, cx));
    agent_ui::show_external_acp_history(workspace, agent_name, window, cx);
}

pub(crate) fn run_focus_acp_tab(
    workspace: &mut Workspace,
    _: &FocusAcpTab,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if agent_ui::focus_external_acp_tab(workspace, window, cx) {
        return;
    }

    run_new_acp_tab(
        workspace,
        &NewAcpTab {
            agent_name: None,
            prompt: None,
        },
        window,
        cx,
    );
}

fn enable_promoted_agent(agent_name: &str, cx: &mut App) {
    let agent_name = agent_name.to_string();
    update_settings_file(<dyn fs::Fs>::global(cx), cx, move |settings, _| {
        let agent_servers = settings.agent_servers.get_or_insert_default();
        agent_servers
            .entry(agent_name.clone())
            .and_modify(|entry| {
                if should_restore_built_in_registry_entry(&agent_name, entry) {
                    *entry = built_in_registry_settings(Some(entry));
                }
            })
            .or_insert_with(|| built_in_registry_settings(None));
    });
}

fn should_restore_built_in_registry_entry(
    agent_name: &str,
    entry: &settings::CustomAgentServerSettings,
) -> bool {
    match entry {
        settings::CustomAgentServerSettings::Registry { .. } => false,
        settings::CustomAgentServerSettings::Custom { path, args, .. } => {
            args.is_empty()
                && built_in_terminal_command(agent_name)
                    .is_some_and(|command| path.file_name().is_some_and(|name| name == command))
        }
        settings::CustomAgentServerSettings::Extension { .. } => false,
    }
}

fn built_in_terminal_command(agent_name: &str) -> Option<&'static str> {
    match agent_name {
        CODEX_NAME => Some("codex"),
        CLAUDE_AGENT_NAME => Some("claude"),
        GEMINI_NAME => Some("gemini"),
        _ => None,
    }
}

fn built_in_registry_settings(
    existing: Option<&settings::CustomAgentServerSettings>,
) -> settings::CustomAgentServerSettings {
    match existing {
        Some(settings::CustomAgentServerSettings::Custom {
            env,
            default_mode,
            default_model,
            favorite_models,
            default_config_options,
            favorite_config_option_values,
            ..
        })
        | Some(settings::CustomAgentServerSettings::Extension {
            env,
            default_mode,
            default_model,
            favorite_models,
            default_config_options,
            favorite_config_option_values,
        })
        | Some(settings::CustomAgentServerSettings::Registry {
            env,
            default_mode,
            default_model,
            favorite_models,
            default_config_options,
            favorite_config_option_values,
        }) => settings::CustomAgentServerSettings::Registry {
            env: env.clone(),
            default_mode: default_mode.clone(),
            default_model: default_model.clone(),
            favorite_models: favorite_models.clone(),
            default_config_options: default_config_options.clone(),
            favorite_config_option_values: favorite_config_option_values.clone(),
        },
        None => settings::CustomAgentServerSettings::Registry {
            default_mode: None,
            default_model: None,
            env: Default::default(),
            favorite_models: Vec::new(),
            default_config_options: Default::default(),
            favorite_config_option_values: Default::default(),
        },
    }
}

fn show_native_agent_toast(workspace: &mut Workspace, cx: &mut Context<Workspace>) {
    struct NativeAgentToast;

    workspace.show_toast(
        workspace::Toast::new(
            workspace::notifications::NotificationId::unique::<NativeAgentToast>(),
            "Native Zed agent threads are not part of the superzent ACP tab flow.",
        )
        .autohide(),
        cx,
    );
}
