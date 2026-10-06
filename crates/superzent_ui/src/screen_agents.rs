use std::time::{Duration, Instant};

use gpui::{Context, Entity, Task, WeakEntity};
use superzent_agent::{
    AgentHookEvent, AgentHookEventType, ScreenAgent, ScreenAgentState, ScreenDetection,
    identify_screen_agent,
};
use terminal::{TaskStatus, Terminal};

use crate::WorkspaceAttentionController;

// Output arrives in bursts while an agent works, so the screen is read once it settles
// for a moment rather than on every chunk.
const SCREEN_CHECK_DELAY: Duration = Duration::from_millis(300);
// A screen that stops showing work without showing a prompt may be mid-redraw, so the
// turn only counts as over once it has stayed that way, with no output, for a while.
const IDLE_CONFIRMATION: Duration = Duration::from_millis(600);
// An agent that keeps redrawing an idle screen (a clock, an animation) would otherwise
// never be seen to stop.
const IDLE_CONFIRMATION_LIMIT: Duration = Duration::from_secs(3);
const IDLE_RECHECK_DELAY: Duration = Duration::from_millis(250);
// How often a task terminal's agent is checked for having exited.
const TASK_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// An agent CLI without lifecycle hooks, whose state is read from its screen and reported
/// as if it came from hooks.
pub(crate) struct ScreenAgentTracker {
    agent: ScreenAgent,
    state: ScreenAgentState,
    idle_since: Option<Instant>,
    last_output: Instant,
    check: Option<Task<()>>,
}

#[derive(Debug, PartialEq)]
pub(crate) enum ScreenStateStep {
    Keep,
    Commit(ScreenAgentState),
    ConfirmIdle { since: Instant },
}

pub(crate) fn next_screen_state(
    current: ScreenAgentState,
    idle_since: Option<Instant>,
    last_output: Instant,
    detection: ScreenDetection,
    now: Instant,
) -> ScreenStateStep {
    if detection.state == current {
        return ScreenStateStep::Keep;
    }
    if detection.state != ScreenAgentState::Idle || detection.visible {
        return ScreenStateStep::Commit(detection.state);
    }
    let since = idle_since.unwrap_or(now);
    let quiet_for = now.saturating_duration_since(since.max(last_output));
    if quiet_for >= IDLE_CONFIRMATION
        || now.saturating_duration_since(since) >= IDLE_CONFIRMATION_LIMIT
    {
        ScreenStateStep::Commit(ScreenAgentState::Idle)
    } else {
        ScreenStateStep::ConfirmIdle { since }
    }
}

fn hook_event_for_state(state: ScreenAgentState) -> AgentHookEventType {
    match state {
        ScreenAgentState::Working => AgentHookEventType::Start,
        ScreenAgentState::Blocked => AgentHookEventType::PermissionRequest,
        ScreenAgentState::Idle => AgentHookEventType::Stop,
    }
}

/// A task terminal runs its command as its own process, which the terminal doesn't sample;
/// an interactive one runs the agent as its foreground job.
fn terminal_screen_agent(terminal: &Terminal) -> Option<ScreenAgent> {
    if let Some(task) = terminal.task() {
        if task.status != TaskStatus::Running {
            return None;
        }
        let command = task.spawned_task.command.as_deref()?;
        let argv = std::iter::once(command.to_string())
            .chain(task.spawned_task.args.iter().cloned())
            .collect::<Vec<_>>();
        return identify_screen_agent(command, &argv).or_else(|| {
            // A command run through a shell (`sh -c "gemini …"`).
            let script = argv
                .iter()
                .skip_while(|argument| argument.as_str() != "-c")
                .nth(1)?;
            let argv = script
                .split_whitespace()
                .map(str::to_string)
                .collect::<Vec<_>>();
            identify_screen_agent(argv.first()?, &argv)
        });
    }
    let process = terminal.foreground_process_info()?;
    identify_screen_agent(&process.name, &process.argv)
}

impl WorkspaceAttentionController {
    /// Starts or stops reading the terminal's screen as its foreground process changes.
    pub(crate) fn refresh_screen_agent(
        &mut self,
        terminal_id: &str,
        terminal: &Entity<Terminal>,
        cx: &mut Context<Self>,
    ) {
        let agent = terminal_screen_agent(terminal.read(cx));
        let tracked_agent = self
            .screen_agents
            .get(terminal_id)
            .map(|tracker| tracker.agent);
        if tracked_agent == agent {
            return;
        }
        if let Some(tracker) = self.screen_agents.remove(terminal_id) {
            // A one-shot task ending mid-turn has finished its work; an interactive agent
            // the user quits mid-turn has not.
            if terminal.read(cx).task().is_some() && tracker.state != ScreenAgentState::Idle {
                self.report_screen_agent_event(terminal_id, AgentHookEventType::Stop, cx);
            }
            self.report_screen_agent_event(terminal_id, AgentHookEventType::SessionEnd, cx);
        }
        let Some(agent) = agent else {
            return;
        };
        self.screen_agents.insert(
            terminal_id.to_string(),
            ScreenAgentTracker {
                agent,
                state: ScreenAgentState::Idle,
                idle_since: None,
                last_output: Instant::now(),
                check: None,
            },
        );
        self.report_screen_agent_event(terminal_id, AgentHookEventType::SessionStart, cx);
        if let Some(session) = self.agent_sessions.get_mut(terminal_id) {
            // The terminal may have run a hooked agent before this one.
            session.kind = None;
            session.screen_agent = Some(agent);
        }
        self.sync_terminal_tab_agent(terminal_id, cx);
        self.schedule_screen_check(terminal_id, terminal.downgrade(), SCREEN_CHECK_DELAY, cx);
    }

    pub(crate) fn handle_terminal_output(
        &mut self,
        terminal_id: &str,
        terminal: &Entity<Terminal>,
        cx: &mut Context<Self>,
    ) {
        // A task terminal's process isn't sampled, so its output is what reveals the task
        // starting or finishing.
        if terminal.read(cx).task().is_some() {
            self.refresh_screen_agent(terminal_id, terminal, cx);
        }
        if let Some(tracker) = self.screen_agents.get_mut(terminal_id) {
            tracker.last_output = Instant::now();
            self.schedule_screen_check(terminal_id, terminal.downgrade(), SCREEN_CHECK_DELAY, cx);
        }
    }

    fn schedule_screen_check(
        &mut self,
        terminal_id: &str,
        terminal: WeakEntity<Terminal>,
        delay: Duration,
        cx: &mut Context<Self>,
    ) {
        let Some(tracker) = self.screen_agents.get_mut(terminal_id) else {
            return;
        };
        if tracker.check.is_some() {
            return;
        }
        let terminal_id = terminal_id.to_string();
        tracker.check = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let checked = this.update(cx, |this, cx| {
                this.check_screen_agent(&terminal_id, terminal, cx);
            });
            if checked.is_err() {
                log::debug!("dropped a screen check for terminal {terminal_id}");
            }
        }));
    }

    fn check_screen_agent(
        &mut self,
        terminal_id: &str,
        terminal: WeakEntity<Terminal>,
        cx: &mut Context<Self>,
    ) {
        let Some(tracker) = self.screen_agents.get_mut(terminal_id) else {
            return;
        };
        tracker.check = None;
        let Some(live_terminal) = terminal.upgrade() else {
            return;
        };
        let is_task = live_terminal.read(cx).task().is_some();
        if is_task {
            // A task can exit without printing anything, which leaves no event to react to.
            self.refresh_screen_agent(terminal_id, &live_terminal, cx);
        }
        let Some(tracker) = self.screen_agents.get_mut(terminal_id) else {
            return;
        };
        let detection = {
            let live_terminal = live_terminal.read(cx);
            tracker.agent.detect(
                &live_terminal.bottom_screen_text(),
                &live_terminal.breadcrumb_text,
            )
        };
        match next_screen_state(
            tracker.state,
            tracker.idle_since,
            tracker.last_output,
            detection,
            Instant::now(),
        ) {
            ScreenStateStep::Keep => tracker.idle_since = None,
            ScreenStateStep::ConfirmIdle { since } => {
                tracker.idle_since = Some(since);
                self.schedule_screen_check(terminal_id, terminal.clone(), IDLE_RECHECK_DELAY, cx);
            }
            ScreenStateStep::Commit(state) => {
                tracker.state = state;
                tracker.idle_since = None;
                self.report_screen_agent_event(terminal_id, hook_event_for_state(state), cx);
            }
        }
        if is_task {
            self.schedule_screen_check(terminal_id, terminal, TASK_POLL_INTERVAL, cx);
        }
    }

    fn report_screen_agent_event(
        &mut self,
        terminal_id: &str,
        event_type: AgentHookEventType,
        cx: &mut Context<Self>,
    ) {
        self.handle_hook_event(
            AgentHookEvent {
                event_type,
                terminal_id: terminal_id.to_string(),
                workspace_id: self.workspace_ids_by_terminal.get(terminal_id).cloned(),
                session_id: None,
                cwd: None,
                agent: None,
                prompt: None,
                launch_args: None,
            },
            cx,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detection(state: ScreenAgentState, visible: bool) -> ScreenDetection {
        ScreenDetection { state, visible }
    }

    #[test]
    fn work_and_approvals_are_reported_at_once() {
        let now = Instant::now();
        assert_eq!(
            next_screen_state(
                ScreenAgentState::Idle,
                None,
                now,
                detection(ScreenAgentState::Working, true),
                now
            ),
            ScreenStateStep::Commit(ScreenAgentState::Working)
        );
        assert_eq!(
            next_screen_state(
                ScreenAgentState::Working,
                None,
                now,
                detection(ScreenAgentState::Blocked, true),
                now
            ),
            ScreenStateStep::Commit(ScreenAgentState::Blocked)
        );
        assert_eq!(
            next_screen_state(
                ScreenAgentState::Working,
                Some(now),
                now,
                detection(ScreenAgentState::Working, true),
                now
            ),
            ScreenStateStep::Keep
        );
    }

    #[test]
    fn a_turn_ends_once_the_screen_stays_idle_and_quiet() {
        let start = Instant::now();
        let idle = detection(ScreenAgentState::Idle, false);
        assert_eq!(
            next_screen_state(ScreenAgentState::Working, None, start, idle, start),
            ScreenStateStep::ConfirmIdle { since: start }
        );
        assert_eq!(
            next_screen_state(
                ScreenAgentState::Working,
                Some(start),
                start,
                idle,
                start + Duration::from_millis(250)
            ),
            ScreenStateStep::ConfirmIdle { since: start }
        );
        assert_eq!(
            next_screen_state(
                ScreenAgentState::Working,
                Some(start),
                start,
                idle,
                start + IDLE_CONFIRMATION
            ),
            ScreenStateStep::Commit(ScreenAgentState::Idle)
        );
    }

    #[test]
    fn output_during_the_confirmation_defers_the_end_of_a_turn() {
        let start = Instant::now();
        let idle = detection(ScreenAgentState::Idle, false);
        let output = start + Duration::from_millis(500);
        assert_eq!(
            next_screen_state(
                ScreenAgentState::Working,
                Some(start),
                output,
                idle,
                start + IDLE_CONFIRMATION
            ),
            ScreenStateStep::ConfirmIdle { since: start }
        );
        assert_eq!(
            next_screen_state(
                ScreenAgentState::Working,
                Some(start),
                start + IDLE_CONFIRMATION_LIMIT,
                idle,
                start + IDLE_CONFIRMATION_LIMIT
            ),
            ScreenStateStep::Commit(ScreenAgentState::Idle)
        );
    }

    #[test]
    fn a_visible_prompt_ends_the_turn_at_once() {
        let now = Instant::now();
        assert_eq!(
            next_screen_state(
                ScreenAgentState::Working,
                None,
                now,
                detection(ScreenAgentState::Idle, true),
                now
            ),
            ScreenStateStep::Commit(ScreenAgentState::Idle)
        );
    }
}
