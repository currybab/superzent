use crate::title_bar_settings::TitleBarSettings;
use gpui::{
    AnyWindowHandle, App, Context, Corner, DismissEvent, Entity, EventEmitter, FocusHandle,
    Focusable, IntoElement, ParentElement, Render, StatefulInteractiveElement, Styled,
    Subscription, Task, WeakEntity, Window,
};
use project::WorktreeSettings;
use settings::{Settings, SettingsLocation};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use sysinfo::{
    CpuRefreshKind, MemoryRefreshKind, Pid, ProcessRefreshKind, ProcessesToUpdate, RefreshKind,
    System,
};
use terminal_view::{TerminalView, terminal_panel::TerminalPanel};
use ui::{
    Button, ButtonCommon, ButtonSize, ButtonStyle, Color, IconButton, IconName, IconSize, Label,
    LabelSize, PopoverMenu, PopoverMenuHandle, SelectableButton, TintColor, Tooltip, div, h_flex,
    prelude::*, rems, v_flex, vh,
};
use util::ResultExt;
use util::rel_path::RelPath;
use workspace::{MultiWorkspace, Workspace};

const OPEN_REFRESH_INTERVAL: Duration = Duration::from_secs(2);
const CLOSED_REFRESH_INTERVAL: Duration = Duration::from_secs(15);
const POLL_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct UsageValues {
    cpu: f32,
    memory: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct HostMetrics {
    total_memory: u64,
    free_memory: u64,
    used_memory: u64,
    cpu_core_count: usize,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct TerminalMetrics {
    label: String,
    root_process_id: u32,
    usage: UsageValues,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct WorkspaceMetrics {
    label: String,
    usage: UsageValues,
    terminals: Vec<TerminalMetrics>,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct ResourceSnapshot {
    app: UsageValues,
    host: HostMetrics,
    workspaces: Vec<WorkspaceMetrics>,
    total: UsageValues,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ProcessSnapshotEntry {
    parent_process_id: Option<u32>,
    cpu_milli_percent: u32,
    memory: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct TerminalSnapshotRequest {
    terminal_id: u64,
    root_process_id: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct TerminalPresentation {
    request: TerminalSnapshotRequest,
    label: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct WorkspaceSnapshotRequest {
    label: String,
    terminals: Vec<TerminalPresentation>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ResourceSnapshotRequest {
    workspaces: Vec<WorkspaceSnapshotRequest>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum UsageSeverity {
    #[default]
    Normal,
    Elevated,
    High,
}

struct ProcessSampler {
    system: System,
}

pub struct ResourceMonitor {
    workspace: WeakEntity<Workspace>,
    window_handle: AnyWindowHandle,
    current_process_id: Option<u32>,
    process_sampler: Arc<Mutex<ProcessSampler>>,
    snapshot: ResourceSnapshot,
    popover_menu_handle: PopoverMenuHandle<ResourceMonitorPopover>,
    poll_task: Task<()>,
    refresh_task: Task<()>,
    refresh_in_flight: bool,
    last_refresh_completed_at: Option<Instant>,
}

pub struct ResourceMonitorPopover {
    resource_monitor: WeakEntity<ResourceMonitor>,
    focus_handle: FocusHandle,
    _subscription: Subscription,
}

impl ProcessSampler {
    fn new() -> Self {
        let refresh_kind = RefreshKind::nothing()
            .with_cpu(CpuRefreshKind::everything())
            .with_memory(MemoryRefreshKind::everything())
            .with_processes(
                ProcessRefreshKind::nothing()
                    .without_tasks()
                    .with_memory()
                    .with_cpu(),
            );

        Self {
            system: System::new_with_specifics(refresh_kind),
        }
    }

    fn sample(&mut self) -> (HashMap<u32, ProcessSnapshotEntry>, HostMetrics) {
        self.system
            .refresh_memory_specifics(MemoryRefreshKind::everything());
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing()
                .without_tasks()
                .with_memory()
                .with_cpu(),
        );

        if self.system.cpus().is_empty() {
            self.system
                .refresh_cpu_specifics(CpuRefreshKind::everything());
        }

        let processes = self
            .system
            .processes()
            .iter()
            .map(|(process_id, process)| {
                (
                    process_id.as_u32(),
                    ProcessSnapshotEntry {
                        parent_process_id: process.parent().map(Pid::as_u32),
                        cpu_milli_percent: (process.cpu_usage().max(0.0) * 1000.0).round() as u32,
                        memory: process.memory(),
                    },
                )
            })
            .collect();

        let host = HostMetrics {
            total_memory: self.system.total_memory(),
            free_memory: self.system.free_memory(),
            used_memory: self.system.used_memory(),
            cpu_core_count: self.system.cpus().len().max(1),
        };

        (processes, host)
    }
}

impl ResourceMonitor {
    pub fn new(workspace: WeakEntity<Workspace>, window: &Window, cx: &mut Context<Self>) -> Self {
        let current_process_id = sysinfo::get_current_pid().ok().map(Pid::as_u32);
        let process_sampler = Arc::new(Mutex::new(ProcessSampler::new()));

        let mut resource_monitor = Self {
            workspace,
            window_handle: window.window_handle(),
            current_process_id,
            process_sampler,
            snapshot: ResourceSnapshot::default(),
            popover_menu_handle: PopoverMenuHandle::default(),
            poll_task: Task::ready(()),
            refresh_task: Task::ready(()),
            refresh_in_flight: false,
            last_refresh_completed_at: None,
        };

        resource_monitor.start_polling(cx);
        let resource_monitor_handle = cx.entity().downgrade();
        cx.defer(move |cx| {
            resource_monitor_handle
                .update(cx, |resource_monitor, cx| {
                    resource_monitor.refresh_now(None, cx);
                })
                .log_err();
        });
        resource_monitor
    }

    fn start_polling(&mut self, cx: &mut Context<Self>) {
        self.poll_task = cx.spawn(async move |resource_monitor, cx| {
            loop {
                cx.background_executor().timer(POLL_INTERVAL).await;
                let Ok(()) = resource_monitor.update(cx, |resource_monitor, cx| {
                    if resource_monitor.should_refresh(cx) {
                        resource_monitor.refresh_now(None, cx);
                    }
                }) else {
                    break;
                };
            }
        });
    }

    fn should_refresh(&self, cx: &App) -> bool {
        if !TitleBarSettings::get_global(cx).show_resource_monitor || self.refresh_in_flight {
            return false;
        }

        let refresh_interval = if self.popover_menu_handle.is_deployed() {
            OPEN_REFRESH_INTERVAL
        } else {
            CLOSED_REFRESH_INTERVAL
        };

        self.last_refresh_completed_at
            .is_none_or(|last_refresh_completed_at| {
                last_refresh_completed_at.elapsed() >= refresh_interval
            })
    }

    fn refresh_now(&mut self, window: Option<&Window>, cx: &mut Context<Self>) {
        if self.refresh_in_flight {
            return;
        }

        let snapshot_request = self.build_snapshot_request(window, cx);
        let process_sampler = self.process_sampler.clone();
        let current_process_id = self.current_process_id;

        self.refresh_in_flight = true;

        let background_task = cx.background_spawn(async move {
            collect_resource_snapshot(snapshot_request, current_process_id, process_sampler)
        });

        self.refresh_task = cx.spawn(async move |resource_monitor, cx| {
            let snapshot = background_task.await;
            resource_monitor
                .update(cx, |resource_monitor, cx| {
                    resource_monitor.snapshot = snapshot;
                    resource_monitor.refresh_in_flight = false;
                    resource_monitor.last_refresh_completed_at = Some(Instant::now());
                    cx.notify();
                })
                .log_err();
        });
    }

    fn build_snapshot_request(&self, window: Option<&Window>, cx: &App) -> ResourceSnapshotRequest {
        let workspaces = self.window_workspaces(window, cx);
        let workspaces = if workspaces.is_empty() {
            self.workspace.upgrade().into_iter().collect()
        } else {
            workspaces
        };

        let workspace_requests = workspaces
            .into_iter()
            .filter_map(|workspace| {
                let label = workspace_label(&workspace, cx);
                let terminals = terminal_presentations(&workspace, cx);
                if terminals.is_empty() {
                    None
                } else {
                    Some(WorkspaceSnapshotRequest { label, terminals })
                }
            })
            .collect();

        ResourceSnapshotRequest {
            workspaces: workspace_requests,
        }
    }

    fn window_handle_workspaces(&self, cx: &App) -> Vec<Entity<Workspace>> {
        self.window_handle
            .downcast::<MultiWorkspace>()
            .and_then(|window_handle| {
                window_handle
                    .read_with(cx, |multi_workspace, _| {
                        multi_workspace.workspaces().to_vec()
                    })
                    .ok()
            })
            .unwrap_or_default()
    }

    fn window_workspaces(&self, window: Option<&Window>, cx: &App) -> Vec<Entity<Workspace>> {
        if let Some(window) = window
            && let Some(Some(multi_workspace)) = window.root::<MultiWorkspace>()
        {
            return multi_workspace.read(cx).workspaces().to_vec();
        }

        self.window_handle_workspaces(cx)
    }

    fn chip_style(&self) -> ButtonStyle {
        match usage_severity(self.snapshot.total, self.snapshot.total, false) {
            UsageSeverity::Normal => ButtonStyle::Subtle,
            UsageSeverity::Elevated => ButtonStyle::Tinted(TintColor::Warning),
            UsageSeverity::High => ButtonStyle::Tinted(TintColor::Error),
        }
    }

    fn trigger_label(&self) -> String {
        if self.snapshot.total.memory == 0 {
            "0 KB".to_string()
        } else {
            format_memory(self.snapshot.total.memory)
        }
    }

    fn trigger_tooltip(&self) -> String {
        format!(
            "Resource usage: {} CPU, {} memory",
            format_cpu(self.snapshot.total.cpu),
            format_memory(self.snapshot.total.memory)
        )
    }
}

impl Render for ResourceMonitor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let button_style = self.chip_style();
        let trigger_label = self.trigger_label();
        let tooltip = self.trigger_tooltip();
        let resource_monitor = cx.entity().downgrade();
        let resource_monitor_for_open = resource_monitor.clone();
        let resource_monitor_for_menu = resource_monitor.clone();

        PopoverMenu::new("resource-monitor-popover")
            .anchor(Corner::BottomLeft)
            .with_handle(self.popover_menu_handle.clone())
            .on_open(Rc::new(move |window, cx| {
                resource_monitor_for_open
                    .update(cx, |resource_monitor, cx| {
                        resource_monitor.refresh_now(Some(window), cx);
                    })
                    .log_err();
            }))
            .menu(move |_window, cx| {
                resource_monitor_for_menu.upgrade().map(|resource_monitor| {
                    cx.new(|cx| ResourceMonitorPopover::new(resource_monitor, cx))
                })
            })
            .trigger_with_tooltip(
                Button::new("resource-monitor-trigger", trigger_label)
                    .icon(IconName::BoltOutlined)
                    .icon_size(IconSize::XSmall)
                    .label_size(LabelSize::Small)
                    .style(button_style)
                    .selected_style(button_style)
                    .when(button_style == ButtonStyle::Subtle, |button| {
                        button.color(Color::Muted).icon_color(Color::Muted)
                    })
                    .size(ButtonSize::Compact),
                Tooltip::text(tooltip),
            )
    }
}

impl ResourceMonitorPopover {
    fn new(resource_monitor: Entity<ResourceMonitor>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&resource_monitor, |_, _, cx| cx.notify());

        Self {
            resource_monitor: resource_monitor.downgrade(),
            focus_handle: cx.focus_handle(),
            _subscription: subscription,
        }
    }

    fn render_metric_badge(
        &self,
        label: &'static str,
        value: String,
        severity: UsageSeverity,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let border_color = severity_border_color(severity, cx);
        let metric_color = severity_color(severity);

        v_flex()
            .gap_0p5()
            .px_2()
            .py_1p5()
            .flex_1()
            .rounded_md()
            .border_1()
            .border_color(border_color)
            .child(Label::new(label).size(LabelSize::Small).color(Color::Muted))
            .child(Label::new(value).size(LabelSize::Small).color(metric_color))
    }

    fn render_usage_row(
        &self,
        label: &str,
        usage: UsageValues,
        total: UsageValues,
        inset: bool,
        _cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let severity = usage_severity(usage, total, true);
        let metric_color = severity_color(severity);

        h_flex()
            .justify_between()
            .items_center()
            .gap_2()
            .w_full()
            .when(inset, |row| row.pl_4())
            .child(
                Label::new(label.to_string())
                    .size(LabelSize::Small)
                    .color(if inset { Color::Muted } else { Color::Default })
                    .truncate(),
            )
            .child(
                h_flex()
                    .gap_3()
                    .flex_none()
                    .child(
                        Label::new(format_cpu(usage.cpu))
                            .size(LabelSize::Small)
                            .color(metric_color),
                    )
                    .child(
                        Label::new(format_memory(usage.memory))
                            .size(LabelSize::Small)
                            .color(metric_color),
                    ),
            )
    }
}

impl Render for ResourceMonitorPopover {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(snapshot) = self
            .resource_monitor
            .read_with(cx, |resource_monitor, _| resource_monitor.snapshot.clone())
            .ok()
        else {
            return div().into_any_element();
        };

        let total_memory_share =
            host_memory_share_percent(snapshot.total.memory, snapshot.host.total_memory);
        let host_share_severity = host_memory_share_severity(total_memory_share);
        let is_refreshing = self
            .resource_monitor
            .read_with(cx, |resource_monitor, _| resource_monitor.refresh_in_flight)
            .unwrap_or(false);
        let resource_monitor = self.resource_monitor.clone();
        let mut workspace_sections = Vec::with_capacity(snapshot.workspaces.len());

        for workspace in &snapshot.workspaces {
            let terminal_rows = workspace
                .terminals
                .iter()
                .map(|terminal| {
                    self.render_usage_row(&terminal.label, terminal.usage, snapshot.total, true, cx)
                        .into_any_element()
                })
                .collect::<Vec<_>>();

            workspace_sections.push(
                v_flex()
                    .gap_1()
                    .child(div().border_b_1().border_color(cx.theme().colors().border))
                    .child(
                        Label::new(workspace.label.clone())
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .child(self.render_usage_row(
                        &workspace.label,
                        workspace.usage,
                        snapshot.total,
                        false,
                        cx,
                    ))
                    .children(terminal_rows)
                    .into_any_element(),
            );
        }

        v_flex()
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(DismissEvent)))
            .w(rems(30.))
            .max_h(vh(0.6, window))
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().colors().border)
            .bg(cx.theme().colors().elevated_surface_background)
            .shadow_lg()
            .child(
                v_flex()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(cx.theme().colors().border)
                    .child(
                        h_flex()
                            .justify_between()
                            .items_center()
                            .child(
                                Label::new("Resource Usage")
                                    .size(LabelSize::Small)
                                    .color(Color::Muted),
                            )
                            .child(
                                IconButton::new(
                                    "resource-monitor-refresh",
                                    if is_refreshing {
                                        IconName::LoadCircle
                                    } else {
                                        IconName::RotateCw
                                    },
                                )
                                .shape(ui::IconButtonShape::Square)
                                .icon_size(IconSize::XSmall)
                                .tooltip(Tooltip::text("Refresh resource metrics"))
                                .on_click(move |_, window, cx| {
                                    resource_monitor
                                        .update(cx, |resource_monitor, cx| {
                                            resource_monitor.refresh_now(Some(window), cx);
                                        })
                                        .log_err();
                                }),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(self.render_metric_badge(
                                "CPU",
                                format_cpu(snapshot.total.cpu),
                                usage_severity(snapshot.total, snapshot.total, false),
                                cx,
                            ))
                            .child(self.render_metric_badge(
                                "Memory",
                                format_memory(snapshot.total.memory),
                                usage_severity(snapshot.total, snapshot.total, false),
                                cx,
                            ))
                            .child(self.render_metric_badge(
                                "RAM Share",
                                format_percent(total_memory_share),
                                host_share_severity,
                                cx,
                            )),
                    ),
            )
            .child(
                v_flex()
                    .id("resource-monitor-scroll")
                    .overflow_y_scroll()
                    .child(
                        v_flex()
                            .gap_2()
                            .px_3()
                            .py_2()
                            .child(
                                v_flex()
                                    .gap_1()
                                    .child(
                                        Label::new("Application")
                                            .size(LabelSize::Small)
                                            .color(Color::Muted),
                                    )
                                    .child(self.render_usage_row(
                                        "Superzet App",
                                        snapshot.app,
                                        snapshot.total,
                                        false,
                                        cx,
                                    )),
                            )
                            .children(workspace_sections)
                            .when(snapshot.workspaces.is_empty(), |content| {
                                content.child(
                                    div().py_3().child(
                                        Label::new("No active terminals")
                                            .size(LabelSize::Small)
                                            .color(Color::Muted),
                                    ),
                                )
                            }),
                    ),
            )
            .into_any_element()
    }
}

impl Focusable for ResourceMonitorPopover {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<DismissEvent> for ResourceMonitorPopover {}

fn workspace_label(workspace: &Entity<Workspace>, cx: &App) -> String {
    let project = workspace.read(cx).project().clone();
    let project = project.read(cx);
    let mut labels = Vec::new();

    for worktree in project.visible_worktrees(cx) {
        let worktree_id = worktree.read(cx).id();
        let settings_location = SettingsLocation {
            worktree_id,
            path: RelPath::empty(),
        };
        let settings = WorktreeSettings::get(Some(settings_location), cx);
        let label = settings
            .project_name
            .clone()
            .unwrap_or_else(|| worktree.read(cx).root_name_str().to_string());
        labels.push(label);
    }

    if labels.is_empty() {
        "empty project".to_string()
    } else {
        labels.join(", ")
    }
}

fn terminal_presentations(workspace: &Entity<Workspace>, cx: &App) -> Vec<TerminalPresentation> {
    let mut pane_entities = workspace.read(cx).panes().to_vec();
    if let Some(terminal_panel) = workspace.read(cx).panel::<TerminalPanel>(cx) {
        pane_entities.extend(terminal_panel.read(cx).panes().into_iter().cloned());
    }

    let mut seen_terminal_ids = HashSet::new();
    let mut seen_root_process_ids = HashSet::new();
    let mut terminals = Vec::new();

    for pane in pane_entities {
        for item in pane.read(cx).items() {
            let Some(terminal_view) = item.act_as::<TerminalView>(cx) else {
                continue;
            };

            let terminal_id = terminal_view.entity_id().as_u64();
            if !seen_terminal_ids.insert(terminal_id) {
                continue;
            }

            let terminal = terminal_view.read(cx).terminal().clone();
            let Some(root_process_id) = terminal
                .read(cx)
                .pid_getter()
                .map(|process_id_getter| process_id_getter.fallback_pid().as_u32())
            else {
                continue;
            };

            if !seen_root_process_ids.insert(root_process_id) {
                continue;
            }

            let label = terminal_view
                .read(cx)
                .custom_title()
                .filter(|custom_title| !custom_title.trim().is_empty())
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| terminal.read(cx).title(true));

            terminals.push(TerminalPresentation {
                request: TerminalSnapshotRequest {
                    terminal_id,
                    root_process_id,
                },
                label,
            });
        }
    }

    terminals
}

fn collect_resource_snapshot(
    snapshot_request: ResourceSnapshotRequest,
    current_process_id: Option<u32>,
    process_sampler: Arc<Mutex<ProcessSampler>>,
) -> ResourceSnapshot {
    let Ok(mut process_sampler) = process_sampler.lock() else {
        return ResourceSnapshot::default();
    };

    let (processes, host) = process_sampler.sample();
    build_resource_snapshot(snapshot_request, current_process_id, &processes, host)
}

fn build_resource_snapshot(
    snapshot_request: ResourceSnapshotRequest,
    current_process_id: Option<u32>,
    processes: &HashMap<u32, ProcessSnapshotEntry>,
    host: HostMetrics,
) -> ResourceSnapshot {
    let app = current_process_id
        .and_then(|process_id| processes.get(&process_id).copied())
        .map(process_usage)
        .unwrap_or_default();

    let mut seen_terminal_ids = HashSet::new();
    let mut seen_root_process_ids = HashSet::new();
    let mut workspaces = Vec::new();
    let mut terminal_total = UsageValues::default();

    for workspace_request in snapshot_request.workspaces {
        let mut workspace_usage = UsageValues::default();
        let mut terminals = Vec::new();

        for terminal in workspace_request.terminals {
            if !seen_terminal_ids.insert(terminal.request.terminal_id)
                || !seen_root_process_ids.insert(terminal.request.root_process_id)
            {
                continue;
            }

            let usage = aggregate_process_tree_usage(terminal.request.root_process_id, processes);
            workspace_usage.cpu += usage.cpu;
            workspace_usage.memory += usage.memory;
            terminals.push(TerminalMetrics {
                label: terminal.label,
                root_process_id: terminal.request.root_process_id,
                usage,
            });
        }

        if terminals.is_empty() {
            continue;
        }

        terminal_total.cpu += workspace_usage.cpu;
        terminal_total.memory += workspace_usage.memory;
        workspaces.push(WorkspaceMetrics {
            label: workspace_request.label,
            usage: workspace_usage,
            terminals,
        });
    }

    ResourceSnapshot {
        app,
        host,
        total: UsageValues {
            cpu: app.cpu + terminal_total.cpu,
            memory: app.memory + terminal_total.memory,
        },
        workspaces,
    }
}

fn aggregate_process_tree_usage(
    root_process_id: u32,
    processes: &HashMap<u32, ProcessSnapshotEntry>,
) -> UsageValues {
    let mut usage = UsageValues::default();

    for (process_id, process) in processes {
        if is_descendant_or_self(*process_id, root_process_id, processes) {
            let process_usage = process_usage(*process);
            usage.cpu += process_usage.cpu;
            usage.memory += process_usage.memory;
        }
    }

    usage
}

fn is_descendant_or_self(
    process_id: u32,
    root_process_id: u32,
    processes: &HashMap<u32, ProcessSnapshotEntry>,
) -> bool {
    let mut current_process_id = process_id;
    let mut visited_processes = HashSet::new();

    loop {
        if current_process_id == root_process_id {
            return true;
        }

        if !visited_processes.insert(current_process_id) {
            return false;
        }

        let Some(parent_process_id) = processes
            .get(&current_process_id)
            .and_then(|process| process.parent_process_id)
        else {
            return false;
        };

        current_process_id = parent_process_id;
    }
}

fn process_usage(process: ProcessSnapshotEntry) -> UsageValues {
    UsageValues {
        cpu: process.cpu_milli_percent as f32 / 1000.0,
        memory: process.memory,
    }
}

fn usage_severity(values: UsageValues, totals: UsageValues, include_share: bool) -> UsageSeverity {
    const GIBIBYTE: u64 = 1024 * 1024 * 1024;
    const MEBIBYTE: u64 = 1024 * 1024;

    if values.cpu >= 120.0 || values.memory >= 3 * GIBIBYTE {
        return UsageSeverity::High;
    }

    if values.cpu >= 70.0 || values.memory >= (3 * GIBIBYTE) / 2 {
        return UsageSeverity::Elevated;
    }

    if !include_share {
        return UsageSeverity::Normal;
    }

    let cpu_share = if totals.cpu > 0.0 {
        values.cpu / totals.cpu
    } else {
        0.0
    };
    let memory_share = if totals.memory > 0 {
        values.memory as f32 / totals.memory as f32
    } else {
        0.0
    };

    let high_share = (totals.cpu >= 60.0 && cpu_share >= 0.55 && values.cpu >= 25.0)
        || (totals.memory >= (3 * GIBIBYTE) / 2
            && memory_share >= 0.55
            && values.memory >= 768 * MEBIBYTE);
    if high_share {
        return UsageSeverity::High;
    }

    let elevated_share = (totals.cpu >= 60.0 && cpu_share >= 0.35 && values.cpu >= 15.0)
        || (totals.memory >= (3 * GIBIBYTE) / 2
            && memory_share >= 0.35
            && values.memory >= 512 * MEBIBYTE);
    if elevated_share {
        return UsageSeverity::Elevated;
    }

    UsageSeverity::Normal
}

fn host_memory_share_percent(total_memory: u64, host_total_memory: u64) -> f32 {
    if host_total_memory == 0 {
        0.0
    } else {
        (total_memory as f32 / host_total_memory as f32) * 100.0
    }
}

fn host_memory_share_severity(memory_share_percent: f32) -> UsageSeverity {
    if memory_share_percent >= 35.0 {
        UsageSeverity::High
    } else if memory_share_percent >= 20.0 {
        UsageSeverity::Elevated
    } else {
        UsageSeverity::Normal
    }
}

fn severity_color(severity: UsageSeverity) -> Color {
    match severity {
        UsageSeverity::Normal => Color::Muted,
        UsageSeverity::Elevated => Color::Warning,
        UsageSeverity::High => Color::Error,
    }
}

fn severity_border_color(severity: UsageSeverity, cx: &mut App) -> gpui::Hsla {
    match severity {
        UsageSeverity::Normal => cx.theme().colors().border,
        UsageSeverity::Elevated => cx.theme().status().warning_border,
        UsageSeverity::High => cx.theme().status().error_border,
    }
}

fn format_memory(memory: u64) -> String {
    const KIBIBYTE: f64 = 1024.0;
    const MEBIBYTE: f64 = 1024.0 * 1024.0;
    const GIBIBYTE: f64 = 1024.0 * 1024.0 * 1024.0;

    let memory = memory as f64;
    if memory < MEBIBYTE {
        format!("{:.0} KB", memory / KIBIBYTE)
    } else if memory < GIBIBYTE {
        format!("{:.1} MB", memory / MEBIBYTE)
    } else {
        format!("{:.2} GB", memory / GIBIBYTE)
    }
}

fn format_cpu(cpu: f32) -> String {
    format!("{cpu:.1}%")
}

fn format_percent(value: f32) -> String {
    format!("{value:.0}%")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(parent_process_id: Option<u32>, cpu: f32, memory: u64) -> ProcessSnapshotEntry {
        ProcessSnapshotEntry {
            parent_process_id,
            cpu_milli_percent: (cpu * 1000.0) as u32,
            memory,
        }
    }

    #[test]
    fn build_resource_snapshot_aggregates_app_and_terminal_usage() {
        let snapshot_request = ResourceSnapshotRequest {
            workspaces: vec![WorkspaceSnapshotRequest {
                label: "workspace-a".to_string(),
                terminals: vec![TerminalPresentation {
                    request: TerminalSnapshotRequest {
                        terminal_id: 1,
                        root_process_id: 20,
                    },
                    label: "server".to_string(),
                }],
            }],
        };
        let processes = HashMap::from([
            (10, process(None, 11.0, 256)),
            (20, process(None, 20.0, 512)),
            (21, process(Some(20), 15.0, 128)),
            (30, process(None, 9.0, 64)),
        ]);

        let snapshot = build_resource_snapshot(
            snapshot_request,
            Some(10),
            &processes,
            HostMetrics {
                total_memory: 4096,
                free_memory: 2048,
                used_memory: 2048,
                cpu_core_count: 8,
            },
        );

        assert_eq!(
            snapshot.app,
            UsageValues {
                cpu: 11.0,
                memory: 256
            }
        );
        assert_eq!(
            snapshot.workspaces[0].usage,
            UsageValues {
                cpu: 35.0,
                memory: 640
            }
        );
        assert_eq!(
            snapshot.total,
            UsageValues {
                cpu: 46.0,
                memory: 896
            }
        );
    }

    #[test]
    fn build_resource_snapshot_deduplicates_duplicate_terminal_roots() {
        let snapshot_request = ResourceSnapshotRequest {
            workspaces: vec![WorkspaceSnapshotRequest {
                label: "workspace-a".to_string(),
                terminals: vec![
                    TerminalPresentation {
                        request: TerminalSnapshotRequest {
                            terminal_id: 1,
                            root_process_id: 20,
                        },
                        label: "server".to_string(),
                    },
                    TerminalPresentation {
                        request: TerminalSnapshotRequest {
                            terminal_id: 2,
                            root_process_id: 20,
                        },
                        label: "server clone".to_string(),
                    },
                ],
            }],
        };
        let processes = HashMap::from([
            (20, process(None, 20.0, 512)),
            (21, process(Some(20), 15.0, 128)),
        ]);

        let snapshot =
            build_resource_snapshot(snapshot_request, None, &processes, HostMetrics::default());

        assert_eq!(snapshot.workspaces[0].terminals.len(), 1);
        assert_eq!(
            snapshot.workspaces[0].usage,
            UsageValues {
                cpu: 35.0,
                memory: 640
            }
        );
    }

    #[test]
    fn usage_severity_matches_superset_thresholds() {
        assert_eq!(
            usage_severity(
                UsageValues {
                    cpu: 10.0,
                    memory: 256,
                },
                UsageValues {
                    cpu: 10.0,
                    memory: 256,
                },
                false,
            ),
            UsageSeverity::Normal
        );
        assert_eq!(
            usage_severity(
                UsageValues {
                    cpu: 70.0,
                    memory: 256,
                },
                UsageValues {
                    cpu: 70.0,
                    memory: 256,
                },
                false,
            ),
            UsageSeverity::Elevated
        );
        assert_eq!(
            usage_severity(
                UsageValues {
                    cpu: 121.0,
                    memory: 256,
                },
                UsageValues {
                    cpu: 121.0,
                    memory: 256,
                },
                false,
            ),
            UsageSeverity::High
        );
    }

    #[test]
    fn usage_severity_considers_share_thresholds() {
        assert_eq!(
            usage_severity(
                UsageValues {
                    cpu: 25.0,
                    memory: 256,
                },
                UsageValues {
                    cpu: 60.0,
                    memory: 2 * 1024 * 1024 * 1024,
                },
                true,
            ),
            UsageSeverity::Elevated
        );

        assert_eq!(
            usage_severity(
                UsageValues {
                    cpu: 35.0,
                    memory: 900 * 1024 * 1024,
                },
                UsageValues {
                    cpu: 65.0,
                    memory: 2 * 1024 * 1024 * 1024,
                },
                true,
            ),
            UsageSeverity::High
        );
    }

    #[test]
    fn host_memory_share_percent_calculates_ram_share() {
        assert_eq!(host_memory_share_percent(0, 0), 0.0);
        assert_eq!(host_memory_share_percent(512, 2048), 25.0);
        assert_eq!(host_memory_share_severity(19.9), UsageSeverity::Normal);
        assert_eq!(host_memory_share_severity(20.0), UsageSeverity::Elevated);
        assert_eq!(host_memory_share_severity(35.0), UsageSeverity::High);
    }

    #[test]
    fn format_memory_formats_large_units() {
        assert_eq!(format_memory(512), "0 KB");
        assert_eq!(format_memory(1024 * 1024), "1.0 MB");
        assert_eq!(format_memory(2 * 1024 * 1024 * 1024), "2.00 GB");
    }
}
