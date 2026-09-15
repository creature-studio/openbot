//! Composer: the bottom input bar.
//!
//! ```text
//! ┌────────────────────────────────────────────────────┐
//! │  Ask Spark...                          Stop   Send  │
//! └────────────────────────────────────────────────────┘
//! ```
//!
//! The composer handles:
//! - Text input (single-line or expandable multi-line)
//! - Send button (submits message to selected task)
//! - Stop button (aborts running task)
//! - ⌘K shortcut for command palette (future)

use gpui::{
    div, prelude::*, App, Context, Entity, IntoElement, Render, Window,
};
use spark_model::TaskId;
use crate::stores::TaskStore;
use crate::ConnectionStore;

pub struct Composer {
    tasks: Entity<TaskStore>,
    connection: Entity<ConnectionStore>,
    input_text: String,
    is_focused: bool,
}

#[derive(Debug, Clone)]
pub enum ComposerEvent {
    Sent { task_id: TaskId, content: String },
    Stopped { task_id: TaskId },
}

impl Composer {
    pub fn new(
        tasks: Entity<TaskStore>,
        connection: Entity<ConnectionStore>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&tasks, |_, _, cx| cx.notify()).detach();

        Self {
            tasks,
            connection,
            input_text: String::new(),
            is_focused: false,
        }
    }

    fn has_running_task(&self, cx: &App) -> bool {
        self.tasks
            .read(cx)
            .selected_task()
            .map(|t| t.status == spark_model::TaskStatus::Running)
            .unwrap_or(false)
    }

    fn can_send(&self) -> bool {
        !self.input_text.trim().is_empty()
    }
}

impl Render for Composer {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let is_running = self.has_running_task(cx);
        let can_send = self.can_send();

        div()
            .flex()
            .items_center()
            .gap_2()
            .px_4()
            .py_3()
            .bg(gpui::rgb(0x111827))
            .border_t_1()
            .border_color(gpui::rgb(0x2d3748))
            .child(
                // Text input
                div()
                    .flex_1()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(gpui::rgb(0x0f172a))
                    .border_1()
                    .border_color(if self.is_focused {
                        gpui::rgb(0x3b82f6)
                    } else {
                        gpui::rgb(0x2d3748)
                    })
                    .text_sm()
                    .child(if self.input_text.is_empty() {
                        div()
                            .text_color(gpui::rgb(0x6b7280))
                            .child("Ask Spark...")
                    } else {
                        div().child(self.input_text.clone())
                    }),
            )
            .when(is_running, |d| {
                // Stop button
                d.child(
                    div()
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .bg(gpui::rgb(0x7f1d1d))
                        .cursor_pointer()
                        .text_sm()
                        .hover(|d| d.bg(gpui::rgb(0x991b1b)))
                        .child("Stop"),
                )
            })
            .child(
                // Send button
                div()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(if can_send {
                        gpui::rgb(0x3b82f6)
                    } else {
                        gpui::rgb(0x1e293b)
                    })
                    .cursor_pointer()
                    .text_sm()
                    .when(!can_send, |d| d.opacity(0.5))
                    .hover(|d| d.bg(gpui::rgb(0x2563eb)))
                    .child("Send"),
            )
    }
}
