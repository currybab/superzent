use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

use gpui::{AnyElement, ClickEvent, SharedString};
use superzent_agent::{AgentHookEventType, AgentKind};
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

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AgentListEntry {
    pub(crate) terminal_id: String,
    pub(crate) group: AgentListGroup,
    pub(crate) title: String,
    pub(crate) kind: Option<AgentKind>,
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
            .get(&entry.terminal_id)
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

/// Like ACP thread titles: the agent's own summary once it sets one (Claude Code puts it
/// in the terminal title), otherwise the first prompt of the session.
pub(crate) fn agent_display_title(
    terminal_title: Option<&str>,
    first_prompt: Option<&str>,
    tab_title: &str,
) -> String {
    let tab_title = tab_title.trim();
    let summary = terminal_title.map(clean_terminal_title).filter(|title| {
        !title.is_empty()
            && !title.eq_ignore_ascii_case(tab_title)
            && !AGENT_NAMES
                .iter()
                .any(|agent_name| title.eq_ignore_ascii_case(agent_name))
    });
    summary
        .or_else(|| {
            first_prompt
                .map(str::trim)
                .filter(|prompt| !prompt.is_empty())
                .map(str::to_string)
        })
        .or_else(|| (!tab_title.is_empty()).then(|| tab_title.to_string()))
        .unwrap_or_else(|| "Agent".to_string())
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

fn agent_kind_icon(kind: Option<AgentKind>) -> IconName {
    match kind {
        Some(AgentKind::Claude) => IconName::AiClaude,
        Some(AgentKind::Codex) => IconName::AiOpenAi,
        None => IconName::Terminal,
    }
}

fn open_agent_terminal(terminal_id: String, cx: &mut App) {
    let Some(controller) = cx
        .try_global::<GlobalAttentionController>()
        .map(|controller| controller.0.clone())
    else {
        return;
    };
    // Opening the tab updates the window this click is dispatched in, which is leased
    // until dispatch returns.
    cx.defer(move |cx| {
        controller.update(cx, |controller, cx| {
            controller.open_agent_terminal(&terminal_id, cx);
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
        let dot_id = format!("agent-row-{}", entry.terminal_id);
        let dot = match entry.group.attention() {
            Some(attention) => render_attention_dot(dot_id, attention),
            None => div()
                .child(Indicator::dot().color(Color::Muted))
                .into_any_element(),
        };

        ListItem::new(SharedString::from(format!("agent-{}", entry.terminal_id)))
            .spacing(ui::ListItemSpacing::Dense)
            .rounded()
            .toggle_state(entry.focused)
            .start_slot(dot)
            .child(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .py_0p5()
                    .child(
                        h_flex()
                            .w_full()
                            .gap_1p5()
                            .items_center()
                            .child(
                                Icon::new(agent_kind_icon(entry.kind))
                                    .size(IconSize::XSmall)
                                    .color(Color::Muted),
                            )
                            .child(
                                div().flex_1().min_w_0().child(
                                    Label::new(entry.title.clone())
                                        .size(LabelSize::Small)
                                        .truncate(),
                                ),
                            )
                            .when_some(elapsed, |this, elapsed| {
                                this.child(
                                    Label::new(elapsed)
                                        .size(LabelSize::XSmall)
                                        .color(Color::Muted),
                                )
                            }),
                    )
                    .when_some(location, |this, location| {
                        this.child(
                            Label::new(location)
                                .size(LabelSize::XSmall)
                                .color(Color::Muted)
                                .truncate(),
                        )
                    }),
            )
            .on_click({
                let terminal_id = entry.terminal_id.clone();
                move |_: &ClickEvent, _, cx| open_agent_terminal(terminal_id.clone(), cx)
            })
            .into_any_element()
    }

    fn set_agent_list_hovered(&mut self, hovered: bool, cx: &mut Context<Self>) {
        self.frozen_agent_groups = hovered.then(|| {
            self.agent_list
                .iter()
                .map(|entry| (entry.terminal_id.clone(), entry.group))
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
            terminal_id: terminal_id.to_string(),
            group,
            title: terminal_id.to_string(),
            kind: None,
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
                        .map(|entry| entry.terminal_id.clone())
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
            ("finished".to_string(), AgentListGroup::Working),
            ("still-working".to_string(), AgentListGroup::Working),
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
    fn display_title_prefers_the_agent_summary_then_the_first_prompt() {
        assert_eq!(
            agent_display_title(
                Some("✳ Refactor the store"),
                Some("refactor store"),
                "claude"
            ),
            "Refactor the store"
        );
        assert_eq!(
            agent_display_title(Some("⠐ 리뷰 정리"), None, "claude"),
            "리뷰 정리"
        );
        // A title naming only the agent is no summary.
        for generic in ["✳ Claude Code", "codex", "Claude"] {
            assert_eq!(
                agent_display_title(Some(generic), Some("Fix the flaky test"), "claude"),
                "Fix the flaky test",
                "{generic}"
            );
        }
        assert_eq!(
            agent_display_title(Some("✳ Codex Preset"), Some("Ship it"), "Codex Preset"),
            "Ship it"
        );
        assert_eq!(agent_display_title(None, Some("  "), " codex "), "codex");
        assert_eq!(agent_display_title(Some("✳"), None, ""), "Agent");
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
