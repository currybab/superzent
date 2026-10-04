use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

use gpui::{AnyElement, ClickEvent, EntityId, SharedString};
use superzent_agent::{AgentHookEventType, AgentKind, ScreenAgent};
use terminal_view::{TerminalTabAttention, render_attention_dot};
use ui::{Icon, Indicator, ListItem, prelude::*};
use workspace::status_bar_height;

use crate::{GlobalAttentionController, SuperzentSidebar, workspace_notification_title};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum AgentListGroup {
    NeedsApproval,
    NeedsReview,
    Working,
    Idle,
}

impl AgentListGroup {
    pub(crate) fn for_attention(attention: Option<TerminalTabAttention>) -> Self {
        match attention {
            Some(TerminalTabAttention::NeedsApproval) => Self::NeedsApproval,
            Some(TerminalTabAttention::NeedsReview) => Self::NeedsReview,
            Some(TerminalTabAttention::Working) => Self::Working,
            None => Self::Idle,
        }
    }

    fn attention(self) -> Option<TerminalTabAttention> {
        match self {
            Self::NeedsApproval => Some(TerminalTabAttention::NeedsApproval),
            Self::NeedsReview => Some(TerminalTabAttention::NeedsReview),
            Self::Working => Some(TerminalTabAttention::Working),
            Self::Idle => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::NeedsApproval => "Needs approval",
            Self::NeedsReview => "Needs review",
            Self::Working => "Working",
            Self::Idle => "Idle",
        }
    }
}

/// Where an agent runs: an agent CLI in a terminal, or an ACP conversation in a tab.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum AgentListTarget {
    Terminal(String),
    #[cfg_attr(not(feature = "acp_tabs"), allow(dead_code))]
    AcpThread(EntityId),
}

impl AgentListTarget {
    pub(crate) fn key(&self) -> String {
        match self {
            Self::Terminal(terminal_id) => format!("terminal-{terminal_id}"),
            Self::AcpThread(thread_id) => format!("acp-{}", thread_id.as_u64()),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum AgentListIcon {
    Named(IconName),
    #[cfg_attr(not(feature = "acp_tabs"), allow(dead_code))]
    External(SharedString),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AgentListEntry {
    pub(crate) target: AgentListTarget,
    pub(crate) group: AgentListGroup,
    pub(crate) title: Option<String>,
    /// The tab's name, shown when there is neither a task title nor a workspace.
    pub(crate) name: String,
    pub(crate) icon: AgentListIcon,
    pub(crate) workspace_id: Option<String>,
    pub(crate) working_since: Option<Instant>,
    pub(crate) sequence: u64,
    pub(crate) focused: bool,
}

/// An agent is listed while it runs, or after it exits while its finished work is still
/// worth a look. The foreground job catches an agent killed without reporting its exit.
pub(crate) fn agent_is_listed(
    group: AgentListGroup,
    session_running: bool,
    has_foreground_job: bool,
) -> bool {
    match group {
        AgentListGroup::NeedsReview => true,
        AgentListGroup::NeedsApproval | AgentListGroup::Working | AgentListGroup::Idle => {
            session_running && has_foreground_job
        }
    }
}

/// A task terminal (a preset launched with a prompt) runs the agent as its own process,
/// so it lives as long as the task runs. An interactive shell runs it as a foreground job.
pub(crate) fn agent_process_alive(task_running: Option<bool>, has_foreground_job: bool) -> bool {
    task_running.unwrap_or(has_foreground_job)
}

/// Codex reports activity from a log watcher and its own notify command, which can land
/// after the wrapper reported the exit; that activity must not revive the agent.
pub(crate) fn agent_hook_event_applies(
    session_running: Option<bool>,
    event_type: &AgentHookEventType,
) -> bool {
    match event_type {
        AgentHookEventType::Start | AgentHookEventType::PermissionRequest => {
            session_running != Some(false)
        }
        AgentHookEventType::SessionStart
        | AgentHookEventType::SessionEnd
        | AgentHookEventType::Stop => true,
    }
}

/// Groups rows by urgency, keeping each row at its launch position within its group.
/// While the list is hovered, `frozen_groups` pins rows to the group they were in when
/// the pointer arrived, so a row can't move out from under a click.
pub(crate) fn group_agent_entries<'a>(
    entries: &'a [AgentListEntry],
    frozen_groups: &BTreeMap<String, AgentListGroup>,
) -> Vec<(AgentListGroup, Vec<&'a AgentListEntry>)> {
    let mut groups = BTreeMap::<AgentListGroup, Vec<&AgentListEntry>>::new();
    for entry in entries {
        let group = frozen_groups
            .get(&entry.target.key())
            .copied()
            .unwrap_or(entry.group);
        groups.entry(group).or_default().push(entry);
    }
    groups
        .into_iter()
        .map(|(group, mut entries)| {
            entries.sort_by_key(|entry| entry.sequence);
            (group, entries)
        })
        .collect()
}

/// Agents such as Claude Code prefix the terminal title with an animated status glyph.
pub(crate) fn clean_terminal_title(terminal_title: &str) -> String {
    terminal_title
        .trim_start_matches(|character: char| !character.is_alphanumeric())
        .trim()
        .to_string()
}

/// Claude Code puts a summary of the task in the terminal title. Other agents leave
/// whatever the shell set there, and once an agent exits the shell owns it again.
pub(crate) fn terminal_title_is_a_summary(kind: Option<AgentKind>, session_running: bool) -> bool {
    session_running && kind == Some(AgentKind::Claude)
}

/// Like ACP thread titles: the agent's own summary once it sets one (Claude Code puts it
/// in the terminal title), otherwise the first prompt of the session. An agent that has
/// neither has no task to name yet.
pub(crate) fn agent_task_title(
    terminal_title: Option<&str>,
    first_prompt: Option<&str>,
    tab_title: &str,
) -> Option<String> {
    let tab_title = tab_title.trim();
    let summary = terminal_title.map(clean_terminal_title).filter(|title| {
        !title.is_empty()
            && !title.eq_ignore_ascii_case(tab_title)
            && !AGENT_NAMES
                .iter()
                .any(|agent_name| title.eq_ignore_ascii_case(agent_name))
    });
    summary.or_else(|| {
        first_prompt
            .map(str::trim)
            .filter(|prompt| !prompt.is_empty())
            .map(str::to_string)
    })
}

// Titles agents show before they have anything to summarize.
const AGENT_NAMES: [&str; 3] = ["Claude Code", "Claude", "Codex"];

pub(crate) fn format_agent_elapsed(elapsed: Duration) -> String {
    let minutes = elapsed.as_secs() / 60;
    match (minutes / 60, minutes % 60) {
        (0, 0) => "<1m".to_string(),
        (0, minutes) => format!("{minutes}m"),
        (hours, 0) => format!("{hours}h"),
        (hours, minutes) => format!("{hours}h {minutes}m"),
    }
}

fn agent_location_label(project_name: Option<&str>, workspace_title: &str) -> String {
    match project_name {
        Some(project_name) if project_name != workspace_title => {
            format!("{project_name} · {workspace_title}")
        }
        _ => workspace_title.to_string(),
    }
}

pub(crate) fn agent_icon(kind: Option<AgentKind>, screen_agent: Option<ScreenAgent>) -> IconName {
    match (kind, screen_agent.map(ScreenAgent::id)) {
        (Some(AgentKind::Claude), _) => IconName::AiClaude,
        (Some(AgentKind::Codex), _) => IconName::AiOpenAi,
        (None, Some("gemini")) => IconName::AiGemini,
        (None, Some("copilot")) => IconName::Copilot,
        (None, Some(_)) => IconName::Sparkle,
        (None, None) => IconName::Terminal,
    }
}

fn open_agent(target: AgentListTarget, cx: &mut App) {
    let Some(controller) = cx
        .try_global::<GlobalAttentionController>()
        .map(|controller| controller.0.clone())
    else {
        return;
    };
    // Opening the tab updates the window this click is dispatched in, which is leased
    // until dispatch returns.
    cx.defer(move |cx| {
        controller.update(cx, |controller, cx| match &target {
            AgentListTarget::Terminal(terminal_id) => {
                controller.open_agent_terminal(terminal_id, cx)
            }
            #[cfg(feature = "acp_tabs")]
            AgentListTarget::AcpThread(thread_id) => controller.open_acp_thread(*thread_id, cx),
            #[cfg(not(feature = "acp_tabs"))]
            AgentListTarget::AcpThread(_) => {}
        });
    });
}

impl SuperzentSidebar {
    pub(crate) fn render_agents_section(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.agent_list.is_empty() {
            return None;
        }
        let collapsed = self.store.read(cx).agents_collapsed();
        let empty_frozen_groups = BTreeMap::new();
        let groups = group_agent_entries(
            &self.agent_list,
            self.frozen_agent_groups
                .as_ref()
                .unwrap_or(&empty_frozen_groups),
        );

        Some(
            v_flex()
                .flex_none()
                .max_h(relative(0.4))
                .border_t_1()
                .border_color(cx.theme().colors().border)
                .child(self.render_agents_header(collapsed, cx))
                .when(!collapsed, |this| {
                    this.child(
                        v_flex()
                            .id("agent-list")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .px_2()
                            .pb_1()
                            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                                this.set_agent_list_hovered(*hovered, cx);
                            }))
                            .children(groups.into_iter().map(|(group, entries)| {
                                v_flex()
                                    .child(
                                        div().px_1().pt_1().child(
                                            Label::new(group.label())
                                                .size(LabelSize::XSmall)
                                                .color(Color::Muted),
                                        ),
                                    )
                                    .children(
                                        entries
                                            .into_iter()
                                            .map(|entry| self.render_agent_row(entry, cx)),
                                    )
                            })),
                    )
                })
                .into_any_element(),
        )
    }

    fn render_agents_header(&self, collapsed: bool, cx: &mut Context<Self>) -> AnyElement {
        let count_in = |group| {
            self.agent_list
                .iter()
                .filter(|entry| entry.group == group)
                .count()
        };
        let count_badge = |count: usize, color: Color| {
            (count > 0).then(|| {
                h_flex()
                    .gap_0p5()
                    .items_center()
                    .child(Indicator::dot().color(color))
                    .child(Label::new(count.to_string()).size(LabelSize::XSmall))
            })
        };

        h_flex()
            .id("agent-list-header")
            .px_2()
            // Lines the collapsed section up with the status bar beside it.
            .h(status_bar_height(cx))
            .gap_1()
            .items_center()
            .cursor_pointer()
            .child(
                Icon::new(if collapsed {
                    IconName::ChevronRight
                } else {
                    IconName::ChevronDown
                })
                .size(IconSize::Small)
                .color(Color::Muted),
            )
            .child(Label::new("Agents").size(LabelSize::Small))
            .child(div().flex_1())
            .children(count_badge(
                count_in(AgentListGroup::NeedsApproval),
                Color::Error,
            ))
            .children(count_badge(
                count_in(AgentListGroup::NeedsReview),
                Color::Success,
            ))
            .child(
                Label::new(self.agent_list.len().to_string())
                    .size(LabelSize::XSmall)
                    .color(Color::Muted),
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.store.update(cx, |store, cx| {
                    store.set_agents_collapsed(!collapsed, cx);
                });
            }))
            .into_any_element()
    }

    fn render_agent_row(&self, entry: &AgentListEntry, cx: &mut Context<Self>) -> AnyElement {
        let store = self.store.read(cx);
        let location = entry
            .workspace_id
            .as_deref()
            .and_then(|workspace_id| store.workspace(workspace_id))
            .map(|workspace| {
                agent_location_label(
                    store
                        .project(&workspace.project_id)
                        .map(|project| project.name.as_str()),
                    &workspace_notification_title(workspace),
                )
            });
        let elapsed = entry
            .working_since
            .filter(|_| entry.group == AgentListGroup::Working)
            .map(|working_since| format_agent_elapsed(working_since.elapsed()));
        let key = entry.target.key();
        let dot_id = format!("agent-row-{key}");
        let dot = match entry.group.attention() {
            Some(attention) => render_attention_dot(dot_id, attention),
            None => div()
                .child(Indicator::dot().color(Color::Muted))
                .into_any_element(),
        };

        let kind_icon_size = IconSize::XSmall;
        let icon_gap = DynamicSpacing::Base06.rems(cx);
        let first_line = |text: String| {
            h_flex()
                .w_full()
                .min_w_0()
                .gap(icon_gap)
                .child(
                    match &entry.icon {
                        AgentListIcon::Named(icon) => Icon::new(*icon),
                        AgentListIcon::External(path) => Icon::from_external_svg(path.clone()),
                    }
                    .size(kind_icon_size)
                    .color(Color::Muted),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(Label::new(text).size(LabelSize::Small).truncate()),
                )
        };
        let content = match entry.title.clone() {
            // The task gets the whole first line, with the rest on a muted line below it.
            Some(title) => {
                let details = [location, elapsed]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" · ");
                v_flex()
                    .w_full()
                    .min_w_0()
                    .child(first_line(title))
                    .when(!details.is_empty(), |this| {
                        this.child(
                            div().pl(kind_icon_size.rems() + icon_gap).child(
                                Label::new(details)
                                    .size(LabelSize::XSmall)
                                    .color(Color::Muted)
                                    .truncate(),
                            ),
                        )
                    })
                    .into_any_element()
            }
            None => h_flex()
                .w_full()
                .min_w_0()
                .gap_1()
                .child(first_line(location.unwrap_or_else(|| entry.name.clone())))
                .when_some(elapsed, |this, elapsed| {
                    this.child(
                        Label::new(elapsed)
                            .size(LabelSize::XSmall)
                            .color(Color::Muted),
                    )
                })
                .into_any_element(),
        };

        ListItem::new(SharedString::from(format!("agent-{key}")))
            .spacing(ui::ListItemSpacing::Dense)
            .rounded()
            .toggle_state(entry.focused)
            .start_slot(dot)
            .child(content)
            .on_click({
                let target = entry.target.clone();
                move |_: &ClickEvent, _, cx| open_agent(target.clone(), cx)
            })
            .into_any_element()
    }

    fn set_agent_list_hovered(&mut self, hovered: bool, cx: &mut Context<Self>) {
        self.frozen_agent_groups = hovered.then(|| {
            self.agent_list
                .iter()
                .map(|entry| (entry.target.key(), entry.group))
                .collect()
        });
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(terminal_id: &str, group: AgentListGroup, sequence: u64) -> AgentListEntry {
        AgentListEntry {
            target: AgentListTarget::Terminal(terminal_id.to_string()),
            group,
            title: None,
            name: terminal_id.to_string(),
            icon: AgentListIcon::Named(IconName::Terminal),
            workspace_id: None,
            working_since: None,
            sequence,
            focused: false,
        }
    }

    fn grouped_ids(
        entries: &[AgentListEntry],
        frozen_groups: &BTreeMap<String, AgentListGroup>,
    ) -> Vec<(AgentListGroup, Vec<String>)> {
        group_agent_entries(entries, frozen_groups)
            .into_iter()
            .map(|(group, entries)| {
                (
                    group,
                    entries
                        .into_iter()
                        .map(|entry| entry.name.clone())
                        .collect(),
                )
            })
            .collect()
    }

    #[test]
    fn groups_follow_the_tab_attention() {
        assert_eq!(
            AgentListGroup::for_attention(Some(TerminalTabAttention::NeedsApproval)),
            AgentListGroup::NeedsApproval
        );
        assert_eq!(
            AgentListGroup::for_attention(Some(TerminalTabAttention::NeedsReview)),
            AgentListGroup::NeedsReview
        );
        assert_eq!(
            AgentListGroup::for_attention(Some(TerminalTabAttention::Working)),
            AgentListGroup::Working
        );
        assert_eq!(AgentListGroup::for_attention(None), AgentListGroup::Idle);
    }

    #[test]
    fn agents_are_listed_only_while_still_running_unless_owed_a_review() {
        for group in [
            AgentListGroup::NeedsApproval,
            AgentListGroup::Working,
            AgentListGroup::Idle,
        ] {
            assert!(agent_is_listed(group, true, true), "{group:?}");
            assert!(!agent_is_listed(group, false, true), "{group:?}");
            // Killed without reporting its exit.
            assert!(!agent_is_listed(group, true, false), "{group:?}");
        }
        // An agent that exited still owes a review of what it finished.
        assert!(agent_is_listed(AgentListGroup::NeedsReview, false, false));
    }

    #[test]
    fn task_terminals_run_the_agent_without_a_shell_in_front() {
        // A preset launched with a prompt runs the agent as the terminal's own process,
        // so no job ever takes the foreground from a shell.
        assert!(agent_process_alive(Some(true), false));
        assert!(!agent_process_alive(Some(false), false));
        assert!(!agent_process_alive(Some(false), true));
        // An interactive shell terminal runs the agent as a foreground job.
        assert!(agent_process_alive(None, true));
        assert!(!agent_process_alive(None, false));
    }

    #[test]
    fn activity_reported_after_an_agent_exits_is_ignored() {
        use AgentHookEventType::*;
        for event_type in [SessionStart, SessionEnd, Start, PermissionRequest, Stop] {
            assert!(
                agent_hook_event_applies(None, &event_type),
                "{event_type:?}"
            );
            assert!(
                agent_hook_event_applies(Some(true), &event_type),
                "{event_type:?}"
            );
        }
        assert!(!agent_hook_event_applies(Some(false), &Start));
        assert!(!agent_hook_event_applies(Some(false), &PermissionRequest));
        // A late completion still leaves finished work to review, and a new agent can
        // start in the same terminal.
        assert!(agent_hook_event_applies(Some(false), &Stop));
        assert!(agent_hook_event_applies(Some(false), &SessionStart));
        assert!(agent_hook_event_applies(Some(false), &SessionEnd));
    }

    #[test]
    fn groups_are_ordered_by_urgency_and_rows_by_launch_order() {
        let entries = vec![
            entry("idle", AgentListGroup::Idle, 1),
            entry("working-new", AgentListGroup::Working, 5),
            entry("review", AgentListGroup::NeedsReview, 2),
            entry("working-old", AgentListGroup::Working, 3),
            entry("approval", AgentListGroup::NeedsApproval, 4),
        ];

        assert_eq!(
            grouped_ids(&entries, &BTreeMap::new()),
            vec![
                (AgentListGroup::NeedsApproval, vec!["approval".to_string()]),
                (AgentListGroup::NeedsReview, vec!["review".to_string()]),
                (
                    AgentListGroup::Working,
                    vec!["working-old".to_string(), "working-new".to_string()]
                ),
                (AgentListGroup::Idle, vec!["idle".to_string()]),
            ]
        );
    }

    #[test]
    fn frozen_groups_keep_rows_in_place_while_hovered() {
        let entries = vec![
            entry("finished", AgentListGroup::NeedsReview, 1),
            entry("still-working", AgentListGroup::Working, 2),
            entry("new", AgentListGroup::Working, 3),
        ];
        let frozen_groups = BTreeMap::from([
            ("terminal-finished".to_string(), AgentListGroup::Working),
            (
                "terminal-still-working".to_string(),
                AgentListGroup::Working,
            ),
        ]);

        assert_eq!(
            grouped_ids(&entries, &frozen_groups),
            vec![(
                AgentListGroup::Working,
                vec![
                    "finished".to_string(),
                    "still-working".to_string(),
                    "new".to_string()
                ]
            )]
        );
    }

    #[test]
    fn task_title_prefers_the_agent_summary_then_the_first_prompt() {
        assert_eq!(
            agent_task_title(
                Some("✳ Refactor the store"),
                Some("refactor store"),
                "claude"
            )
            .as_deref(),
            Some("Refactor the store")
        );
        assert_eq!(
            agent_task_title(Some("⠐ 리뷰 정리"), None, "claude").as_deref(),
            Some("리뷰 정리")
        );
        // A title naming only the agent is no summary.
        for generic in ["✳ Claude Code", "codex", "Claude"] {
            assert_eq!(
                agent_task_title(Some(generic), Some("Fix the flaky test"), "claude").as_deref(),
                Some("Fix the flaky test"),
                "{generic}"
            );
        }
        assert_eq!(
            agent_task_title(Some("✳ Codex Preset"), Some("Ship it"), "Codex Preset").as_deref(),
            Some("Ship it")
        );
    }

    #[test]
    fn only_agents_known_to_summarize_get_their_terminal_title_read() {
        assert!(terminal_title_is_a_summary(Some(AgentKind::Claude), true));
        // Codex leaves whatever the shell put in the title, such as `user@host: ~/dir`.
        assert!(!terminal_title_is_a_summary(Some(AgentKind::Codex), true));
        assert!(!terminal_title_is_a_summary(None, true));
        // Once the agent exits, the shell owns the title again.
        assert!(!terminal_title_is_a_summary(Some(AgentKind::Claude), false));
    }

    #[test]
    fn agents_without_a_summary_or_prompt_have_no_task_title() {
        assert_eq!(agent_task_title(None, Some("  "), "codex"), None);
        assert_eq!(
            agent_task_title(Some("✳ Claude Code"), None, "claude"),
            None
        );
        assert_eq!(agent_task_title(Some("✳"), None, ""), None);
    }

    #[test]
    fn terminal_titles_drop_leading_status_glyphs() {
        assert_eq!(clean_terminal_title("✳ Refactor"), "Refactor");
        assert_eq!(clean_terminal_title("  ⠐  "), "");
    }

    #[test]
    fn location_names_the_project_only_when_it_adds_something() {
        assert_eq!(
            agent_location_label(Some("superzent"), "feat/sidebar"),
            "superzent · feat/sidebar"
        );
        assert_eq!(
            agent_location_label(Some("superzent"), "superzent"),
            "superzent"
        );
        assert_eq!(agent_location_label(None, "main"), "main");
    }

    #[test]
    fn elapsed_time_is_rounded_down_to_minutes() {
        assert_eq!(format_agent_elapsed(Duration::from_secs(0)), "<1m");
        assert_eq!(format_agent_elapsed(Duration::from_secs(59)), "<1m");
        assert_eq!(format_agent_elapsed(Duration::from_secs(60)), "1m");
        assert_eq!(
            format_agent_elapsed(Duration::from_secs(59 * 60 + 59)),
            "59m"
        );
        assert_eq!(format_agent_elapsed(Duration::from_secs(60 * 60)), "1h");
        assert_eq!(
            format_agent_elapsed(Duration::from_secs(2 * 60 * 60 + 5 * 60)),
            "2h 5m"
        );
    }
}
