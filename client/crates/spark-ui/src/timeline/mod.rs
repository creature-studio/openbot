//! Timeline view: the central panel showing agent activity.
//!
//! This is NOT a "chat" view. It's a rich timeline that displays:
//! - User messages
//! - Assistant messages (with streaming)
//! - Tool cards (collapsed by default, auto-expand when running)
//! - Permission requests
//! - Artifacts
//! - Errors
//! - ReadyForCheck cards
//!
//! CRITICAL: The timeline must be virtualized using GPUI's uniform_list
//! (or similar). Agent tasks can produce thousands of events. Only items
//! in the viewport should be rendered.
//!
//! Design:
//! ```text
//! ┌──────────────────────────────────────────┐
//! │ 修复登录页面                              │
//! │                                          │
//! │ 用户：                                   │
//! │ 帮我修复登录异常                          │
//! │                                          │
//! │ ● 正在工作                               │
//! │                                          │
//! │ ▶ 搜索文件  src/login...        16ms     │
//! │                                          │
//! │ ▶ 读取文件  src/login.tsx        3ms     │
//! │                                          │
//! │ ▼ 修改文件  src/login.tsx       12ms     │
//! │   +12 -3                                 │
//! │   (expanded diff preview)                │
//! │                                          │
//! │ ▶ cargo test                   1.8s      │
//! │   28 passed                              │
//! └──────────────────────────────────────────┘
//! ```

use gpui::{
    div, px, prelude::*, Context, Entity, IntoElement, Render, SharedString, Window,
};
use spark_model::*;
use crate::stores::TaskStore;

pub struct TaskTimeline {
    tasks: Entity<TaskStore>,
    command_tx: tokio::sync::mpsc::UnboundedSender<spark_transport::TransportCommand>,
    /// Set of tool item IDs that are manually expanded.
    expanded_tools: std::collections::HashSet<String>,
}

impl TaskTimeline {
    pub fn new(
        tasks: Entity<TaskStore>,
        command_tx: tokio::sync::mpsc::UnboundedSender<spark_transport::TransportCommand>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&tasks, |_, _, cx| cx.notify()).detach();

        Self {
            tasks,
            command_tx,
            expanded_tools: std::collections::HashSet::new(),
        }
    }

    fn toggle_tool(&mut self, id: &str) {
        if self.expanded_tools.contains(id) {
            self.expanded_tools.remove(id);
        } else {
            self.expanded_tools.insert(id.to_string());
        }
    }
}

impl Render for TaskTimeline {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let timeline = cx.entity();
        let tasks = self.tasks.read(cx);
        let selected = tasks.selected_task();

        let Some(task) = selected else {
            return div()
                .flex()
                .items_center()
                .justify_center()
                .h_full()
                .text_color(gpui::rgb(0x6b7280))
                .child("Select a task to view timeline").into_any_element();
        };

        // Goal header
        let header = div()
            .px_5()
            .py_4()
            .text_lg()
            .font_weight(gpui::FontWeight::BOLD)
            .text_color(gpui::rgb(0xf8fafc))
            .border_b_1()
            .border_color(gpui::rgb(0x1e293b))
            .child(task.goal.clone());

        // Status indicator
        let status = div()
            .px_5()
            .py_2()
            .flex()
            .items_center()
            .gap_2()
            .bg(gpui::rgb(0x0f172a))
            .child(task_status_badge(&task.status));

        // Timeline items (in production, use uniform_list for virtualization)
        let mut items_div = div().flex().flex_col().gap_1().px_4().py_2();

        for item in &task.timeline {
            items_div = items_div.child(self.render_timeline_item(&task.id.0, item, timeline.clone()));
        }

        div()
            .flex()
            .flex_col()
            .h_full()
            .bg(gpui::rgb(0x0b1220))
            .id("task-timeline-scroll")
            .overflow_y_scroll()
            .child(header)
            .child(status)
            .child(items_div).into_any_element()
    }
}

impl TaskTimeline {
    fn render_timeline_item(
        &self,
        task_id: &str,
        item: &TimelineItem,
        timeline: Entity<Self>,
    ) -> impl IntoElement {
        match item {
            TimelineItem::UserMessage(msg) => self.render_user_message(msg).into_any_element(),
            TimelineItem::AssistantMessage(msg) => self.render_assistant_message(msg).into_any_element(),
            TimelineItem::Status(s) => self.render_status(s).into_any_element(),
            TimelineItem::Tool(tool) => self.render_tool_card(tool, timeline).into_any_element(),
            TimelineItem::Permission(perm) => self.render_permission(task_id, perm).into_any_element(),
            TimelineItem::ReadyForCheck(rfc) => self.render_ready_for_check(task_id, rfc).into_any_element(),
            TimelineItem::Error(err) => self.render_error(err).into_any_element(),
            TimelineItem::Artifact(art) => self.render_artifact(art).into_any_element(),
        }
    }

    fn render_user_message(&self, msg: &UserMessage) -> impl IntoElement {
        div()
            .mx_1()
            .my_1()
            .px_3()
            .py_2()
            .rounded_md()
            .bg(gpui::rgb(0x111c32))
            .child(
                div()
                    .text_xs()
                    .text_color(gpui::rgb(0x60a5fa))
                    .mb_1()
                    .child("用户："),
            )
            .child(
                div()
                    .text_sm()
                    .child(msg.content.clone()),
            )
    }

    fn render_assistant_message(&self, msg: &AssistantMessage) -> impl IntoElement {
        div()
            .mx_1()
            .my_1()
            .px_3()
            .py_2()
            .rounded_md()
            .text_color(gpui::rgb(0xdbeafe))
            .child(
                div()
                    .text_sm()
                    .when(msg.streaming, |d| {
                        d.child(
                            div()
                                .w(px(2.0))
                                .h(px(14.0))
                                .bg(gpui::rgb(0x3b82f6)),
                        )
                    })
                    .child(msg.content.clone()),
            )
    }

    fn render_status(&self, status: &StatusItem) -> impl IntoElement {
        div()
            .mx_1()
            .my_1()
            .px_3()
            .py_2()
            .rounded_md()
            .bg(gpui::rgb(0x0f172a))
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .text_xs()
                    .text_color(gpui::rgb(0x3b82f6))
                    .child("●"),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(gpui::rgb(0x9ca3af))
                    .child(status.label.clone()),
            )
    }

    fn render_tool_card(&self, tool: &ToolItem, timeline: Entity<Self>) -> impl IntoElement {
        let is_running = tool.status == ToolItemStatus::Running;
        let is_expanded = self.expanded_tools.contains(&tool.id) || is_running;
        let is_error = tool.status == ToolItemStatus::Error;

        let (status_icon, status_color) = match &tool.status {
            ToolItemStatus::Running => ("●", gpui::rgb(0x3b82f6)),
            ToolItemStatus::Success => ("✓", gpui::rgb(0x10b981)),
            ToolItemStatus::Error => ("✗", gpui::rgb(0xef4444)),
            ToolItemStatus::PermissionRequired => ("⚠", gpui::rgb(0xf59e0b)),
            ToolItemStatus::Cancelled => ("⊘", gpui::rgb(0x6b7280)),
        };

        let duration_str = tool
            .duration_ms
            .map(|d| {
                if d >= 1000 {
                    format!("{:.1}s", d as f64 / 1000.0)
                } else {
                    format!("{}ms", d)
                }
            })
            .unwrap_or_default();

        let label = tool
            .label
            .clone()
            .unwrap_or_else(|| tool.tool_name.clone());

        let icon = tool.presentation.icon();

        let mut card = div()
            .rounded_md()
            .border_1()
            .when(is_running, |d| {
                d.border_color(gpui::rgb(0x3b82f6)).bg(gpui::rgba(0x1e3a5f20))
            })
            .when(is_error, |d| {
                d.border_color(gpui::rgb(0xef4444)).bg(gpui::rgba(0x5f1e1e20))
            })
            .when(!is_running && !is_error, |d| {
                d.border_color(gpui::rgb(0x24324a)).bg(gpui::rgb(0x111c32))
            })
            .p_3();

        // Header row (always visible)
        let tool_id = tool.id.clone();
        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .cursor_pointer()
            .id(format!("tool-toggle-{}", tool.id))
            .on_click(move |_, _, cx| {
                timeline.update(cx, |timeline, cx| {
                    timeline.toggle_tool(&tool_id);
                    cx.notify();
                });
            })
            .hover(|d| d.bg(gpui::rgba(0x1e3a5f30)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(status_color)
                            .child(status_icon),
                    )
                    .child(div().text_xs().child(icon))
                    .child(div().text_sm().child(label)),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(gpui::rgb(0x6b7280))
                    .child(duration_str),
            );

        card = card.child(header);

        // Expanded content
        if is_expanded {
            // Arguments (collapsed)
            if let Some(ref args) = tool.args_json {
                card = card.child(
                    div()
                        .mt_1()
                        .p_2()
                        .rounded_sm()
                        .bg(gpui::rgb(0x0f172a))
                        .text_xs()
                        .font_family("monospace")
                        .child(truncate_str(args, 200)),
                );
            }

            // Output / result
            if let Some(ref output) = tool.output {
                card = card.child(
                    div()
                        .mt_1()
                        .p_2()
                        .rounded_sm()
                        .bg(gpui::rgb(0x0f172a))
                        .text_xs()
                        .font_family("monospace")
                        .max_h(px(200.0))
                        .id(SharedString::from(format!("tool-output-{}", tool.call_id)))
                        .overflow_y_scroll()
                        .child(truncate_str(output, 2000)),
                );
            }

            // Diff summary
            if let Some(ref diff) = tool.diff_summary {
                card = card.child(
                    div()
                        .mt_1()
                        .flex()
                        .gap_2()
                        .text_xs()
                        .child(
                            div()
                                .text_color(gpui::rgb(0x10b981))
                                .child(format!("+{}", diff.additions)),
                        )
                        .child(
                            div()
                                .text_color(gpui::rgb(0xef4444))
                                .child(format!("-{}", diff.deletions)),
                        ),
                );
            }

            // Error
            if let Some(ref err) = tool.error {
                card = card.child(
                    div()
                        .mt_1()
                        .p_2()
                        .rounded_sm()
                        .bg(gpui::rgba(0x5f1e1e30))
                        .text_xs()
                        .text_color(gpui::rgb(0xef4444))
                        .child(err.clone()),
                );
            }
        }

        card
    }

    fn render_permission(&self, task_id: &str, perm: &PermissionItem) -> impl IntoElement {
        let allow_tx = self.command_tx.clone();
        let deny_tx = self.command_tx.clone();
        let allow_task = task_id.to_string();
        let deny_task = task_id.to_string();
        let permission_id = perm.id.clone();
        let deny_permission = permission_id.clone();
        div()
            .rounded_lg()
            .border_1()
            .border_color(gpui::rgb(0xf59e0b))
            .bg(gpui::rgba(0x451a0320))
            .p_4()
            .child(div().text_sm().text_color(gpui::rgb(0xf59e0b)).child("⚠ Permission Required"))
            .child(div().text_sm().child(format!("Tool: {}", perm.tool_name)))
            .child(div().text_xs().text_color(gpui::rgb(0x9ca3af)).mt_1().child(perm.reason.clone()))
            .when(perm.resolved.is_none(), |card| {
                card.child(
                    div()
                        .flex()
                        .gap_2()
                        .mt_3()
                        .child(
                            div()
                                .px_3()
                                .py_1()
                                .rounded_md()
                                .bg(gpui::rgb(0x334155))
                                .hover(|d| d.bg(gpui::rgb(0x475569)))
                                .cursor_pointer()
                                .text_sm()
                                .id(format!("timeline-permission-deny-{}", perm.id))
                                .on_click(move |_, _, _| {
                                    let _ = deny_tx.send(spark_transport::TransportCommand::DenyPermission {
                                        task_id: deny_task.clone(),
                                        permission_id: deny_permission.clone(),
                                    });
                                })
                                .child("Cancel"),
                        )
                        .child(
                            div()
                                .px_3()
                                .py_1()
                                .rounded_md()
                                .bg(gpui::rgb(0x047857))
                                .hover(|d| d.bg(gpui::rgb(0x059669)))
                                .cursor_pointer()
                                .text_sm()
                                .id(format!("timeline-permission-allow-{}", perm.id))
                                .on_click(move |_, _, _| {
                                    let _ = allow_tx.send(spark_transport::TransportCommand::ApprovePermission {
                                        task_id: allow_task.clone(),
                                        permission_id: permission_id.clone(),
                                    });
                                })
                                .child("Allow Once"),
                        ),
                )
            })
    }

    fn render_ready_for_check(&self, task_id: &str, rfc: &ReadyForCheckItem) -> impl IntoElement {
        let continue_tx = self.command_tx.clone();
        let confirm_tx = self.command_tx.clone();
        let continue_task = task_id.to_string();
        let confirm_task = task_id.to_string();
        div()
            .rounded_lg()
            .border_1()
            .border_color(gpui::rgb(0x10b981))
            .bg(gpui::rgba(0x064e3b20))
            .p_4()
            .child(div().text_sm().text_color(gpui::rgb(0x10b981)).child("✓ Ready for Check"))
            .child(div().text_sm().child(rfc.summary.clone()))
            .child(
                div()
                    .flex()
                    .gap_3()
                    .mt_2()
                    .text_xs()
                    .text_color(gpui::rgb(0x9ca3af))
                    .child(format!("{} files changed", rfc.files_changed))
                    .child(if rfc.tests_passed { "Tests passed".to_string() } else { "Tests not run".to_string() })
                    .child(if rfc.browser_verified { "Browser verified".to_string() } else { String::new() }),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .mt_3()
                    .child(
                        div()
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .bg(gpui::rgb(0x334155))
                            .hover(|d| d.bg(gpui::rgb(0x475569)))
                            .cursor_pointer()
                            .text_sm()
                            .id(format!("timeline-ready-continue-{}", rfc.id))
                            .on_click(move |_, _, _| {
                                let _ = continue_tx.send(spark_transport::TransportCommand::SendTaskMessage {
                                    task_id: continue_task.clone(),
                                    content: "Continue with the task.".to_string(),
                                });
                            })
                            .child("Continue"),
                    )
                    .child(
                        div()
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .bg(gpui::rgb(0x047857))
                            .hover(|d| d.bg(gpui::rgb(0x059669)))
                            .cursor_pointer()
                            .text_sm()
                            .id(format!("timeline-ready-confirm-{}", rfc.id))
                            .on_click(move |_, _, _| {
                                let _ = confirm_tx.send(spark_transport::TransportCommand::ConfirmComplete {
                                    task_id: confirm_task.clone(),
                                });
                            })
                            .child("Confirm Complete"),
                    ),
            )
    }

    fn render_error(&self, err: &ErrorItem) -> impl IntoElement {
        div()
            .rounded_md()
            .border_1()
            .border_color(gpui::rgb(0xef4444))
            .bg(gpui::rgba(0x5f1e1e20))
            .p_3()
            .child(
                div()
                    .text_sm()
                    .text_color(gpui::rgb(0xef4444))
                    .child("Error"),
            )
            .child(
                div()
                    .text_xs()
                    .mt_1()
                    .child(err.message.clone()),
            )
    }

    fn render_artifact(&self, art: &ArtifactItem) -> impl IntoElement {
        div()
            .rounded_md()
            .border_1()
            .border_color(gpui::rgb(0x2d3748))
            .p_3()
            .child(
                div()
                    .text_sm()
                    .child(format!("📎 {}", art.name)),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(gpui::rgb(0x6b7280))
                    .child(art.kind.clone()),
            )
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn task_status_badge(status: &TaskStatus) -> impl IntoElement {
    let (label, bg_color) = match status {
        TaskStatus::Running => ("● Working", gpui::rgb(0x1e3a5f)),
        TaskStatus::Pending => ("○ Pending", gpui::rgb(0x1e293b)),
        TaskStatus::WaitingApproval => ("⚠ Waiting", gpui::rgb(0x451a03)),
        TaskStatus::ReadyForCheck => ("◉ Ready", gpui::rgb(0x064e3b)),
        TaskStatus::Completed => ("✓ Completed", gpui::rgb(0x064e3b)),
        TaskStatus::Failed => ("✗ Failed", gpui::rgb(0x5f1e1e)),
        TaskStatus::Cancelled => ("⊘ Cancelled", gpui::rgb(0x1e293b)),
    };

    div()
        .px_2()
        .py_1()
        .rounded_full()
        .bg(bg_color)
        .text_xs()
        .child(label)
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}
