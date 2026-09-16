//! Sidebar view: Bot selector, Task list, Workbench list, Machines list.
//!
//! Layout:
//! ```text
//! ┌──────────────┐
//! │ BOT          │
//! │ ● Coding Bot │
//! │              │
//! │ TASKS        │
//! │ ▶ Login Bug  │
//! │ ✓ API Fix    │
//! │ ! Deploy     │
//! │              │
//! │ WORKBENCH    │
//! │ Project A    │
//! │              │
//! │ MACHINES     │
//! │ ● Local      │
//! │ ◐ devbox 38ms│
//! │ ! old-box    │
//! │ + Machine    │
//! └──────────────┘
//! ```
//!
//! The MACHINES section is the entry point of the whole remote-machine
//! feature: it is where a machine is added, where its status is visible, and
//! where a degraded/unreachable machine explains itself.

use gpui::{
    div, px, prelude::*, App, Context, Entity, IntoElement, Render, SharedString, Window,
};
use spark_transport::{TransportCommand};
use crate::stores::{BotStore, TaskStore, WorkbenchStore, MachineStore};
use crate::machine::MachinePanel;
use spark_model::{TaskStatus, MachineStatus};

pub struct Sidebar {
    bots: Entity<BotStore>,
    tasks: Entity<TaskStore>,
    workbenches: Entity<WorkbenchStore>,
    machines: Entity<MachineStore>,
    command_tx: tokio::sync::mpsc::UnboundedSender<TransportCommand>,
}

impl Sidebar {
    pub fn new(
        bots: Entity<BotStore>,
        tasks: Entity<TaskStore>,
        workbenches: Entity<WorkbenchStore>,
        machines: Entity<MachineStore>,
        command_tx: tokio::sync::mpsc::UnboundedSender<TransportCommand>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&bots, |_, _, cx| cx.notify()).detach();
        cx.observe(&tasks, |_, _, cx| cx.notify()).detach();
        cx.observe(&workbenches, |_, _, cx| cx.notify()).detach();
        cx.observe(&machines, |_, _, cx| cx.notify()).detach();
        Self {
            bots,
            tasks,
            workbenches,
            machines,
            command_tx,
        }
    }
}

impl Render for Sidebar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .h_full()
            .w(px(244.0))
            .bg(gpui::rgb(0x0f172a))
            .border_r_1()
            .border_color(gpui::rgb(0x1e293b))
            .p_3()
            .gap_5()
            .child(self.render_bots(cx))
            .child(self.render_tasks(cx))
            .child(self.render_workbenches(cx))
            .child(self.render_machines(cx))
    }
}

impl Sidebar {
    fn section(title: &str, body: impl IntoElement) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .px_2()
                    .text_xs()
                    .text_color(gpui::rgb(0x64748b))
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(SharedString::from(title.to_string())),
            )
            .child(body)
    }

    fn render_bots(&self, cx: &App) -> impl IntoElement {
        let bots = self.bots.read(cx);
        let mut list = div().flex().flex_col().gap_1();
        for bot in &bots.bots {
            let bot_id = bot.id.clone();
            let bot_store = self.bots.clone();
            let selected = bots.selected.as_ref() == Some(&bot.id);
            list = list.child(
                div()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .when(selected, |d| d.bg(gpui::rgb(0x1e293b)))
                    .hover(|d| d.bg(gpui::rgb(0x172554)))
                    .cursor_pointer()
                    .id(format!("bot-row-{}", bot.id.0))
                    .on_click(move |_, _, cx| {
                        bot_store.update(cx, |bots, cx| bots.select(Some(bot_id.clone()), cx));
                    })
                    .text_sm()
                    .child(SharedString::from(format!("● {}", bot.name))),
            );
        }

        let create_bot_store = self.bots.clone();
        list = list.child(
            div()
                .mt_1()
                .px_3()
                .py_2()
                .rounded_md()
                .border_1()
                .border_color(gpui::rgb(0x24324a))
                .text_xs()
                .text_color(gpui::rgb(0x60a5fa))
                .cursor_pointer()
                .hover(|d| d.bg(gpui::rgb(0x172554)))
                .id("create-bot")
                .on_click(move |_, _, cx| {
                    create_bot_store.update(cx, |bots, cx| {
                        bots.create_default(cx);
                    });
                })
                .child("+ New bot"),
        );
        Self::section("BOT", list)
    }

    fn render_tasks(&self, cx: &App) -> impl IntoElement {
        let tasks = self.tasks.read(cx);
        let mut list = div().flex().flex_col().gap_1();
        for id in &tasks.order {
            if let Some(task) = tasks.tasks.get(id) {
                let marker = match task.status {
                    TaskStatus::Running => "▶",
                    TaskStatus::Completed => "✓",
                    TaskStatus::Failed => "✗",
                    TaskStatus::WaitingApproval | TaskStatus::ReadyForCheck => "!",
                    _ => "○",
                };
                let selected = tasks.selected.as_ref() == Some(id);
                let task_id = id.clone();
                let task_store = self.tasks.clone();
                list = list.child(
                    div()
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .text_sm()
                        .cursor_pointer()
                        .id(format!("task-row-{}", task_id.0))
                        .when(selected, |d| d.bg(gpui::rgb(0x1e293b)))
                        .hover(|d| d.bg(gpui::rgb(0x172554)))
                        .on_click(move |_, _, cx| {
                            task_store.update(cx, |tasks, cx| {
                                tasks.select(Some(task_id.clone()), cx);
                            });
                        })
                        .child(SharedString::from(format!(
                            "{} {}",
                            marker, task.goal
                        ))),
                );
            }
        }
        if tasks.order.is_empty() {
            list = list.child(
                div()
                    .px_3()
                    .py_2()
                    .text_xs()
                    .text_color(gpui::rgb(0x64748b))
                    .child("输入目标后点击 Send"),
            );
        }
        Self::section("TASKS", list)
    }

    fn render_workbenches(&self, cx: &App) -> impl IntoElement {
        let workbenches = self.workbenches.read(cx);
        let mut list = div().flex().flex_col().gap_1();
        for workbench in &workbenches.workbenches {
            // A workbench keeps its machine for its whole life: the runtime,
            // PTYs and browser of a project all live on the same machine.
            let workbench_id = workbench.id.clone();
            let workbench_store = self.workbenches.clone();
            let selected = workbenches.selected.as_ref() == Some(&workbench.id);
            list = list.child(
                div()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .when(selected, |d| d.bg(gpui::rgb(0x1e293b)))
                    .hover(|d| d.bg(gpui::rgb(0x172554)))
                    .cursor_pointer()
                    .id(format!("workbench-row-{}", workbench.id))
                    .on_click(move |_, _, cx| {
                        workbench_store.update(cx, |workbenches, cx| {
                            workbenches.select(Some(workbench_id.clone()), cx)
                        });
                    })
                    .text_sm()
                    .child(SharedString::from(format!(
                        "{}  ({})",
                        workbench.name,
                        workbench.machine_id.as_str()
                    ))),
            );
        }
        if workbenches.workbenches.is_empty() {
            list = list.child(
                div()
                    .px_3()
                    .py_2()
                    .text_xs()
                    .text_color(gpui::rgb(0x64748b))
                    .child("任务创建后显示项目工作区"),
            );
        }
        Self::section("WORKBENCH", list)
    }

    fn render_machines(&self, cx: &App) -> impl IntoElement {
        let machines = self.machines.read(cx);

        let mut list = div().flex().flex_col().gap_1();
        let mut header = div()
            .flex()
            .items_center()
            .justify_between()
            .px_2()
            .child(
                div()
                    .text_xs()
                    .text_color(gpui::rgb(0x6b7280))
                    .child("MACHINES"),
            );

        // A blocked machine is a global condition; make it visible at the top
        // of the section, not only in the modal card.
        let blocked = machines
            .machines
            .iter()
            .find(|m| m.status.requires_user_action());
        if blocked.is_some() {
            header = header.child(
                div()
                    .text_xs()
                    .text_color(gpui::rgb(0xef4444))
                    .child("需要确认"),
            );
        }
        list = list.child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(MachinePanel::render_sidebar_list(
                    machines.machines.clone(),
                    machines.selected.clone(),
                    self.machines.clone(),
                )),
        );

        // Status detail line for the selected machine: why it is not connected.
        if let Some(machine) = machines.selected() {
            let detail = match machine.status {
                MachineStatus::Connected => None,
                MachineStatus::Degraded => Some("延迟较高，工具调用会变慢".to_string()),
                MachineStatus::Connecting => Some("正在建立 SSH 连接…".to_string()),
                MachineStatus::Bootstrapping => Some("正在安装 / 启动远端 sandd…".to_string()),
                MachineStatus::Disconnected => {
                    Some("已断开。远端 runtime / PTY / Chrome 仍然存活。".to_string())
                }
                MachineStatus::Unreachable => Some("暂时不可达，正在按退避重试".to_string()),
                MachineStatus::Error => Some("连接失败，可在 Runtime 面板重新 bootstrap".to_string()),
                MachineStatus::RequiresUserAction => {
                    Some("主机密钥或认证需要你确认".to_string())
                }
            };
            if let Some(detail) = detail {
                list = list.child(
                    div()
                        .px_2()
                        .pt_1()
                        .text_xs()
                        .text_color(gpui::rgb(0x6b7280))
                        .child(SharedString::from(detail)),
                );
            }
        }

        Self::section_header(header, list)
    }
}

impl Sidebar {
    fn section_header(header: impl IntoElement + 'static, body: impl IntoElement) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(header)
            .child(body)
    }
}

/// The "+ Machine" form, rendered as a centred card by the app shell.
pub fn render_add_machine_form(
    form: crate::stores::machine::MachineForm,
    store: Entity<MachineStore>,
    command_tx: tokio::sync::mpsc::UnboundedSender<TransportCommand>,
) -> impl IntoElement {
    div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(gpui::rgba(0x00000080))
        .child(MachinePanel::render_add_form(form, store, command_tx))
}
