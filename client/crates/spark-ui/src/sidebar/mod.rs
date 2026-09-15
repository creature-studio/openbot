//! Sidebar view: Bot selector, Task list, Workbench list.
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
//! └──────────────┘
//! ```

use gpui::{
    div, prelude::*, Context, Entity, IntoElement, Render, SharedString,
    Stateful, WindowContext,
};
use crate::stores::{BotStore, TaskStore, WorkbenchStore};
use spark_model::TaskStatus;

pub struct Sidebar {
    bots: Entity<BotStore>,
    tasks: Entity<TaskStore>,
    workbenches: Entity<WorkbenchStore>,
}

impl Sidebar {
    pub fn new(
        bots: Entity<BotStore>,
        tasks: Entity<TaskStore>,
        workbenches: Entity<WorkbenchStore>,
        cx: &mut WindowContext,
    ) -> Self {
        // Subscribe to store changes.
        cx.observe(&bots, |_, _, cx| cx.notify()).detach();
        cx.observe(&tasks, |_, _, cx| cx.notify()).detach();
        cx.observe(&workbenches, |_, _, cx| cx.notify()).detach();

        Self {
            bots,
            tasks,
            workbenches,
        }
    }
}

impl Render for Sidebar {
    fn render(&mut self, cx: &mut ViewContext<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .h_full()
            .w(px(220.0))
            .bg(cx.theme().colors().panel_background)
            .border_r_1()
            .border_color(cx.theme().colors().border)
            .child(self.render_header(cx))
            .child(self.render_bots(cx))
            .child(self.render_tasks(cx))
            .child(self.render_workbenches(cx))
    }
}

impl Sidebar {
    fn render_header(&self, _cx: &mut ViewContext<Self>) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .justify_between()
            .px_3()
            .py_2()
            .child(
                div()
                    .text_sm()
                    .font_weight(gpui::FontWeight::BOLD)
                    .child("Spark"),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(gpui::rgb(0x10b981))
                    .child("● Connected"),
            )
    }

    fn render_bots(&self, cx: &mut ViewContext<Self>) -> impl IntoElement {
        let bots = self.bots.read(cx);

        let mut section = div()
            .px_3()
            .py_2()
            .child(
                div()
                    .text_xs()
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(gpui::rgb(0x9ca3af))
                    .uppercase()
                    .tracking_wider()
                    .child("BOT"),
            );

        for bot in &bots.bots {
            let is_selected = bots.selected.as_ref() == Some(&bot.id);
            section = section.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .cursor_pointer()
                    .when(is_selected, |d| d.bg(gpui::rgb(0x1e293b)))
                    .hover(|d| d.bg(gpui::rgb(0x1e293b)))
                    .child(
                        div()
                            .w(px(8.0))
                            .h(px(8.0))
                            .rounded_full()
                            .bg(gpui::rgb(0x10b981)),
                    )
                    .child(div().text_sm().child(bot.name.clone())),
            );
        }

        section
    }

    fn render_tasks(&self, cx: &mut ViewContext<Self>) -> impl IntoElement {
        let tasks = self.tasks.read(cx);

        let mut section = div()
            .px_3()
            .py_2()
            .child(
                div()
                    .text_xs()
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(gpui::rgb(0x9ca3af))
                    .uppercase()
                    .tracking_wider()
                    .child("TASKS"),
            );

        for task_id in &tasks.order {
            if let Some(task) = tasks.tasks.get(task_id) {
                let is_selected = tasks.selected.as_ref() == Some(task_id);
                let (icon, color) = task_status_icon(&task.status);

                section = section.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .cursor_pointer()
                        .when(is_selected, |d| d.bg(gpui::rgb(0x1e293b)))
                        .hover(|d| d.bg(gpui::rgb(0x1e293b)))
                        .child(div().text_sm().text_color(color).child(icon))
                        .child(
                            div()
                                .text_sm()
                                .overflow_hidden()
                                .text_ellipsis()
                                .max_w(px(160.0))
                                .child(task.goal.clone()),
                        ),
                );
            }
        }

        section
    }

    fn render_workbenches(&self, cx: &mut ViewContext<Self>) -> impl IntoElement {
        let wbs = self.workbenches.read(cx);

        let mut section = div()
            .px_3()
            .py_2()
            .child(
                div()
                    .text_xs()
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(gpui::rgb(0x9ca3af))
                    .uppercase()
                    .tracking_wider()
                    .child("WORKBENCH"),
            );

        for wb in &wbs.workbenches {
            section = section.child(
                div()
                    .px_2()
                    .py_1()
                    .text_sm()
                    .child(wb.name.clone()),
            );
        }

        section
    }
}

fn task_status_icon(status: &TaskStatus) -> (&'static str, gpui::Hsla) {
    match status {
        TaskStatus::Running => ("●", gpui::rgb(0x3b82f6)),       // blue
        TaskStatus::Completed => ("✓", gpui::rgb(0x10b981)),      // green
        TaskStatus::Failed => ("✗", gpui::rgb(0xef4444)),         // red
        TaskStatus::WaitingApproval => ("!", gpui::rgb(0xf59e0b)), // yellow
        TaskStatus::ReadyForCheck => ("◉", gpui::rgb(0x10b981)),  // green
        TaskStatus::Pending => ("○", gpui::rgb(0x6b7280)),        // gray
        TaskStatus::Cancelled => ("⊘", gpui::rgb(0x6b7280)),      // gray
    }
}
