use gpui::{ClickEvent, DismissEvent, EventEmitter, FocusHandle, Focusable, Render, WeakEntity};
use project::project_settings::ProjectSettings;
use remote::RemoteConnectionOptions;
use settings::Settings;
use ui::{ElevationIndex, Modal, ModalFooter, ModalHeader, Section, prelude::*};
use workspace::{
    ModalView, MultiWorkspace, OpenOptions, Workspace, notifications::DetachAndPromptErr,
};

use crate::open_remote_project;

enum Host {
    CollabGuestProject,
    RemoteServerProject(RemoteConnectionOptions, bool),
}

pub struct DisconnectedOverlay {
    workspace: WeakEntity<Workspace>,
    host: Host,
    focus_handle: FocusHandle,
    finished: bool,
}

impl EventEmitter<DismissEvent> for DisconnectedOverlay {}
impl Focusable for DisconnectedOverlay {
    fn focus_handle(&self, _cx: &gpui::App) -> gpui::FocusHandle {
        self.focus_handle.clone()
    }
}
impl ModalView for DisconnectedOverlay {
    fn on_before_dismiss(
        &mut self,
        _window: &mut Window,
        _: &mut Context<Self>,
    ) -> workspace::DismissDecision {
        workspace::DismissDecision::Dismiss(self.finished)
    }
    fn fade_out_background(&self) -> bool {
        true
    }
}

impl DisconnectedOverlay {
    pub fn register(
        workspace: &mut Workspace,
        window: Option<&mut Window>,
        cx: &mut Context<Workspace>,
    ) {
        let Some(window) = window else {
            return;
        };
        cx.subscribe_in(
            workspace.project(),
            window,
            |workspace, project, event, window, cx| {
                if !matches!(
                    event,
                    project::Event::DisconnectedFromHost
                        | project::Event::DisconnectedFromRemote { .. }
                ) {
                    return;
                }
                let handle = cx.entity().downgrade();

                let remote_connection_options = project.read(cx).remote_connection_options(cx);
                let host = if let Some(remote_connection_options) = remote_connection_options {
                    Host::RemoteServerProject(
                        remote_connection_options,
                        matches!(
                            event,
                            project::Event::DisconnectedFromRemote {
                                server_not_running: true
                            }
                        ),
                    )
                } else {
                    Host::CollabGuestProject
                };

                workspace.toggle_modal(window, cx, |_, cx| DisconnectedOverlay {
                    finished: false,
                    workspace: handle,
                    host,
                    focus_handle: cx.focus_handle(),
                });
            },
        )
        .detach();
    }

    fn handle_reconnect(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.finished = true;
        cx.emit(DismissEvent);

        if let Host::RemoteServerProject(remote_connection_options, _) = &self.host {
            self.reconnect_to_remote_project(remote_connection_options.clone(), window, cx);
        }
    }

    fn reconnect_to_remote_project(
        &self,
        connection_options: RemoteConnectionOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };

        let Some(window_handle) = window.window_handle().downcast::<MultiWorkspace>() else {
            return;
        };

        let app_state = workspace.read(cx).app_state().clone();
        let paths = workspace
            .read(cx)
            .root_paths(cx)
            .iter()
            .map(|path| path.to_path_buf())
            .collect();

        cx.spawn_in(window, async move |_, cx| {
            open_remote_project(
                connection_options,
                paths,
                app_state,
                OpenOptions {
                    replace_window: Some(window_handle),
                    ..Default::default()
                },
                cx,
            )
            .await?;
            Ok(())
        })
        .detach_and_prompt_err("Failed to reconnect", window, cx, |_, _, _| None);
    }

    // Superzent keeps a single window, so closing it from here would quit the app. Close only
    // the disconnected workspace; MultiWorkspace swaps in an empty one if it was the last.
    fn close_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let Some(multi_workspace) = window.root::<MultiWorkspace>().flatten() else {
            log::error!("disconnected overlay is not hosted in a MultiWorkspace window");
            return;
        };

        multi_workspace.update(cx, |multi_workspace, cx| {
            let Some(index) = multi_workspace
                .workspaces()
                .iter()
                .position(|candidate| *candidate == workspace)
            else {
                return;
            };
            multi_workspace
                .close_workspace_at_index(index, window, cx)
                .detach_and_log_err(cx);
        });
    }

    fn cancel(&mut self, _: &menu::Cancel, _: &mut Window, cx: &mut Context<Self>) {
        self.finished = true;
        cx.emit(DismissEvent)
    }
}

impl Render for DisconnectedOverlay {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let can_reconnect = matches!(self.host, Host::RemoteServerProject(..));

        let message = match &self.host {
            Host::CollabGuestProject => {
                "Your connection to the remote project has been lost.".to_string()
            }
            Host::RemoteServerProject(options, server_not_running) => {
                let autosave = if ProjectSettings::get_global(cx)
                    .session
                    .restore_unsaved_buffers
                {
                    "\nUnsaved changes are stored locally."
                } else {
                    ""
                };
                let reason = if *server_not_running {
                    "process exiting unexpectedly"
                } else {
                    "not responding"
                };
                format!(
                    "Your connection to {} has been lost due to the server {reason}.{autosave}",
                    options.display_name(),
                )
            }
        };

        div()
            .track_focus(&self.focus_handle(cx))
            .elevation_3(cx)
            .on_action(cx.listener(Self::cancel))
            .occlude()
            .w(rems(24.))
            .max_h(rems(40.))
            .child(
                Modal::new("disconnected", None)
                    .header(
                        ModalHeader::new()
                            .show_dismiss_button(true)
                            .child(Headline::new("Disconnected").size(HeadlineSize::Small)),
                    )
                    .section(Section::new().child(Label::new(message)))
                    .footer(
                        ModalFooter::new().end_slot(
                            h_flex()
                                .gap_2()
                                .child(
                                    Button::new("close-workspace", "Close Workspace")
                                        .style(ButtonStyle::Filled)
                                        .layer(ElevationIndex::ModalSurface)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.close_workspace(window, cx)
                                        })),
                                )
                                .when(can_reconnect, |el| {
                                    el.child(
                                        Button::new("reconnect", "Reconnect")
                                            .style(ButtonStyle::Filled)
                                            .layer(ElevationIndex::ModalSurface)
                                            .start_icon(Icon::new(IconName::ArrowCircle))
                                            .on_click(cx.listener(Self::handle_reconnect)),
                                    )
                                }),
                        ),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gpui::{Entity, TestAppContext, VisualTestContext};
    use project::Project;
    use workspace::AppState;

    use super::*;

    fn init_test(cx: &mut TestAppContext) -> Arc<AppState> {
        cx.update(|cx| {
            let state = AppState::test(cx);
            crate::init(cx);
            state
        })
    }

    fn open_overlay(
        workspace: &Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) -> Entity<DisconnectedOverlay> {
        workspace.update_in(cx, |workspace, window, cx| {
            let handle = cx.entity().downgrade();
            workspace.toggle_modal(window, cx, |_, cx| DisconnectedOverlay {
                finished: false,
                workspace: handle,
                host: Host::CollabGuestProject,
                focus_handle: cx.focus_handle(),
            });
            workspace
                .active_modal::<DisconnectedOverlay>(cx)
                .expect("disconnected overlay should be open")
        })
    }

    #[gpui::test]
    async fn test_close_closes_only_disconnected_workspace(cx: &mut TestAppContext) {
        let app_state = init_test(cx);
        let first_project = Project::test(app_state.fs.clone(), [], cx).await;
        let second_project = Project::test(app_state.fs.clone(), [], cx).await;
        let window =
            cx.add_window(|window, cx| MultiWorkspace::test_new(first_project.clone(), window, cx));
        let cx = &mut VisualTestContext::from_window(window.into(), cx);

        let disconnected_workspace = window
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace.test_add_workspace(second_project, window, cx)
            })
            .expect("window should exist");
        let overlay = open_overlay(&disconnected_workspace, cx);

        overlay.update_in(cx, |overlay, window, cx| {
            overlay.close_workspace(window, cx)
        });
        cx.run_until_parked();

        let remaining_projects = window
            .read_with(cx, |multi_workspace, cx| {
                multi_workspace
                    .workspaces()
                    .iter()
                    .map(|workspace| workspace.read(cx).project().clone())
                    .collect::<Vec<_>>()
            })
            .expect("window should stay open");
        assert_eq!(remaining_projects, vec![first_project]);
    }

    #[gpui::test]
    async fn test_close_last_workspace_keeps_window_open(cx: &mut TestAppContext) {
        let app_state = init_test(cx);
        let project = Project::test(app_state.fs.clone(), [], cx).await;
        let window =
            cx.add_window(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
        let cx = &mut VisualTestContext::from_window(window.into(), cx);

        let disconnected_workspace = window
            .read_with(cx, |multi_workspace, _| multi_workspace.workspace().clone())
            .expect("window should exist");
        let overlay = open_overlay(&disconnected_workspace, cx);

        overlay.update_in(cx, |overlay, window, cx| {
            overlay.close_workspace(window, cx)
        });
        cx.run_until_parked();

        let workspaces = window
            .read_with(cx, |multi_workspace, _| {
                multi_workspace.workspaces().to_vec()
            })
            .expect("window should stay open");
        assert_eq!(workspaces.len(), 1);
        assert_ne!(workspaces[0], disconnected_workspace);
    }
}
