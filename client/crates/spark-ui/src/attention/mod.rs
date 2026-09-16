//! Attention overlay: modal/overlay for permissions and ReadyForCheck.
//!
//! This renders as a floating overlay on top of the main workspace.
//!
//! Permission example:
//! ```text
//! ┌───────────────────────────────────────┐
//! │ ⚠ Permission required                 │
//! │                                       │
//! │ Agent wants to remove:                │
//! │                                       │
//! │ production.db                         │
//! │                                       │
//! │ Reason                                │
//! │ Clean obsolete local database         │
//! │                                       │
//! │          Cancel    Allow once         │
//! └───────────────────────────────────────┘
//! ```
//!
//! ReadyForCheck example:
//! ```text
//! ┌────────────────────────────────────────┐
//! │ ✓ Ready for check                      │
//! │                                        │
//! │ 4 files changed                        │
//! │ Tests passed                           │
//! │ Browser verified                       │
//! │                                        │
//! │ [Continue]      [Confirm complete]     │
//! └────────────────────────────────────────┘
//! ```

use gpui::{
    div, px, prelude::*, Context, Entity, IntoElement, Render, Window,
};
use crate::machine::MachinePanel;
use crate::stores::{AttentionStore, AttentionSnapshot, MachineStore, TaskStore};
use spark_transport::TransportCommand;

pub struct AttentionOverlay {
    attention: Entity<AttentionStore>,
    /// Machine-level attention (unknown / changed host key, auth failure).
    /// It takes precedence: a machine that is not trusted cannot run anything,
    /// so the user must answer this card first.
    machines: Option<Entity<MachineStore>>,
    tasks: Option<Entity<TaskStore>>,
    command_tx: Option<tokio::sync::mpsc::UnboundedSender<TransportCommand>>,
}

impl AttentionOverlay {
    pub fn new(attention: Entity<AttentionStore>, cx: &mut Context<Self>) -> Self {
        cx.observe(&attention, |_, _, cx| cx.notify()).detach();
        Self {
            attention,
            machines: None,
            tasks: None,
            command_tx: None,
        }
    }

    /// Overlay that also shows the machine host-key confirmation card.
    pub fn with_machines(
        attention: Entity<AttentionStore>,
        machines: Entity<MachineStore>,
        tasks: Entity<TaskStore>,
        command_tx: tokio::sync::mpsc::UnboundedSender<TransportCommand>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&machines, |_, _, cx| cx.notify()).detach();
        cx.observe(&tasks, |_, _, cx| cx.notify()).detach();
        let mut overlay = Self::new(attention, cx);
        overlay.machines = Some(machines);
        overlay.tasks = Some(tasks);
        overlay.command_tx = Some(command_tx);
        overlay
    }
}

impl AttentionOverlay {
    fn render_permission(
        &self,
        task_id: &str,
        permission_id: Option<&str>,
        tool: &str,
        reason: &str,
    ) -> impl IntoElement {
        let approve_tx = self.command_tx.clone().expect("task command channel");
        let deny_tx = approve_tx.clone();
        let approve_attention = self.attention.clone();
        let deny_attention = self.attention.clone();
        let task_id_approve = task_id.to_string();
        let task_id_deny = task_id.to_string();
        let permission_approve = permission_id.unwrap_or_default().to_string();
        let permission_deny = permission_approve.clone();

        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui::rgba(0x00000080))
            .child(
                div()
                    .w(px(420.0))
                    .rounded_lg()
                    .bg(gpui::rgb(0x1e293b))
                    .border_1()
                    .border_color(gpui::rgb(0xf59e0b))
                    .p_6()
                    .child(
                        div()
                            .text_lg()
                            .text_color(gpui::rgb(0xf59e0b))
                            .mb_4()
                            .child("⚠ Permission Required"),
                    )
                    .child(div().text_sm().mb_1().child(format!("Tool: {tool}")))
                    .child(
                        div()
                            .text_xs()
                            .text_color(gpui::rgb(0x9ca3af))
                            .mb_1()
                            .child("Reason"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .p_3()
                            .rounded_md()
                            .bg(gpui::rgb(0x0f172a))
                            .mb_4()
                            .child(reason.to_string()),
                    )
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap_3()
                            .child(
                                div()
                                    .px_4()
                                    .py_2()
                                    .rounded_md()
                                    .bg(gpui::rgb(0x374151))
                                    .cursor_pointer()
                                    .text_sm()
                                    .id("permission-deny")
                                    .on_click(move |_, _, cx| {
                                        let _ = deny_tx.send(TransportCommand::DenyPermission {
                                            task_id: task_id_deny.clone(),
                                            permission_id: permission_deny.clone(),
                                        });
                                        deny_attention.update(cx, |store, cx| store.clear(cx));
                                    })
                                    .child("Cancel"),
                            )
                            .child(
                                div()
                                    .px_4()
                                    .py_2()
                                    .rounded_md()
                                    .bg(gpui::rgb(0x059669))
                                    .cursor_pointer()
                                    .text_sm()
                                    .id("permission-approve")
                                    .on_click(move |_, _, cx| {
                                        let _ = approve_tx.send(TransportCommand::ApprovePermission {
                                            task_id: task_id_approve.clone(),
                                            permission_id: permission_approve.clone(),
                                        });
                                        approve_attention.update(cx, |store, cx| store.clear(cx));
                                    })
                                    .child("Allow Once"),
                            ),
                    ),
            )
    }

    fn render_ready_for_check(
        &self,
        task_id: &str,
        summary: &str,
        files_changed: u32,
        tests_passed: bool,
        browser_verified: bool,
    ) -> impl IntoElement {
        let confirm_tx = self.command_tx.clone().expect("task command channel");
        let continue_tx = confirm_tx.clone();
        let confirm_attention = self.attention.clone();
        let continue_attention = self.attention.clone();
        let confirm_id = task_id.to_string();
        let continue_id = task_id.to_string();
        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui::rgba(0x00000080))
            .child(
                div()
                    .w(px(420.0))
                    .rounded_lg()
                    .bg(gpui::rgb(0x1e293b))
                    .border_1()
                    .border_color(gpui::rgb(0x10b981))
                    .p_6()
                    .child(
                        div()
                            .text_lg()
                            .text_color(gpui::rgb(0x10b981))
                            .mb_4()
                            .child("✓ Ready for Check"),
                    )
                    .child(div().text_sm().mb_3().child(summary.to_string()))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .mb_4()
                            .text_xs()
                            .text_color(gpui::rgb(0x9ca3af))
                            .child(format!("{files_changed} files changed"))
                            .child(if tests_passed { "✓ Tests passed" } else { "○ Tests not run" })
                            .child(if browser_verified { "✓ Browser verified" } else { "○ Browser not verified" }),
                    )
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap_3()
                            .child(
                                div()
                                    .px_4()
                                    .py_2()
                                    .rounded_md()
                                    .bg(gpui::rgb(0x374151))
                                    .cursor_pointer()
                                    .text_sm()
                                    .id("ready-continue")
                                    .on_click(move |_, _, cx| {
                                        let _ = continue_tx.send(TransportCommand::SendTaskMessage {
                                            task_id: continue_id.clone(),
                                            content: "Continue with the task.".to_string(),
                                        });
                                        continue_attention.update(cx, |store, cx| store.clear(cx));
                                    })
                                    .child("Continue"),
                            )
                            .child(
                                div()
                                    .px_4()
                                    .py_2()
                                    .rounded_md()
                                    .bg(gpui::rgb(0x059669))
                                    .cursor_pointer()
                                    .text_sm()
                                    .id("ready-confirm")
                                    .on_click(move |_, _, cx| {
                                        let _ = confirm_tx.send(TransportCommand::ConfirmComplete {
                                            task_id: confirm_id.clone(),
                                        });
                                        confirm_attention.update(cx, |store, cx| store.clear(cx));
                                    })
                                    .child("Confirm Complete"),
                            ),
                    ),
            )
    }

    fn render_error(
        &self,
        task_id: &str,
        error: &str,
        recoverable: bool,
    ) -> impl IntoElement {
        let close_attention = self.attention.clone();
        let retry_attention = self.attention.clone();
        let retry_tx = self.command_tx.clone();
        let retry_id = task_id.to_string();
        let retry = div()
            .px_3()
            .py_2()
            .rounded_md()
            .bg(gpui::rgb(0x2563eb))
            .cursor_pointer()
            .id("error-retry")
            .on_click(move |_, _, cx| {
                if let Some(tx) = retry_tx.as_ref() {
                    let command = if retry_id == "host-agent" {
                        TransportCommand::Connect
                    } else {
                        TransportCommand::SendTaskMessage {
                            task_id: retry_id.clone(),
                            content: "Retry the failed operation.".to_string(),
                        }
                    };
                    let _ = tx.send(command);
                }
                retry_attention.update(cx, |store, cx| store.clear(cx));
            })
            .child(if task_id == "host-agent" { "Reconnect" } else { "Retry" });
        let close = div()
            .mt_4()
            .px_3()
            .py_2()
            .rounded_md()
            .bg(gpui::rgb(0x374151))
            .cursor_pointer()
            .id("error-dismiss")
            .on_click(move |_, _, cx| {
                close_attention.update(cx, |store, cx| store.clear(cx));
            })
            .child("Close");

        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui::rgba(0x00000060))
            .child(
                div()
                    .w(px(420.0))
                    .rounded_lg()
                    .bg(gpui::rgb(0x1e293b))
                    .border_1()
                    .border_color(gpui::rgb(0xef4444))
                    .p_6()
                    .child(
                        div()
                            .text_lg()
                            .text_color(gpui::rgb(0xef4444))
                            .mb_2()
                            .child("Error"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .p_3()
                            .rounded_md()
                            .bg(gpui::rgb(0x5f1e1e30))
                            .child(error.to_string()),
                    )
                    .child(
                        div()
                            .mt_4()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .when(recoverable, |d| d.child(retry))
                            .child(close),
                    ),
            )
    }

    fn render_waiting_input(&self, task_id: &str, prompt: &str) -> impl IntoElement {
        let response_attention = self.attention.clone();
        let response_tx = self.command_tx.clone();
        let response_id = task_id.to_string();
        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui::rgba(0x00000060))
            .child(
                div()
                    .w(px(420.0))
                    .rounded_lg()
                    .bg(gpui::rgb(0x1e293b))
                    .border_1()
                    .border_color(gpui::rgb(0x3b82f6))
                    .p_6()
                    .child(
                        div()
                            .text_lg()
                            .text_color(gpui::rgb(0x3b82f6))
                            .mb_2()
                            .child("Input Needed"),
                    )
                    .child(div().text_sm().mb_4().child(prompt.to_string()))
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .child(
                                div()
                                    .px_4()
                                    .py_2()
                                    .rounded_md()
                                    .bg(gpui::rgb(0x3b82f6))
                                    .cursor_pointer()
                                    .id("waiting-input-continue")
                                    .on_click(move |_, _, cx| {
                                        if let Some(tx) = response_tx.as_ref() {
                                            let _ = tx.send(TransportCommand::SendTaskMessage {
                                                task_id: response_id.clone(),
                                                content: "Continue with the task.".to_string(),
                                            });
                                        }
                                        response_attention.update(cx, |store, cx| store.clear(cx));
                                    })
                                    .child("Continue"),
                            ),
                    ),
            )
    }
}

impl Render for AttentionOverlay {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Machine attention first: [取消] / [信任并连接] is a prerequisite for
        // every task on that machine.
        let machine_attention = self
            .machines
            .as_ref()
            .and_then(|machines| machines.read(cx).attention.clone());
        if let Some(attention) = machine_attention {
            return div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(gpui::rgba(0x00000080))
                .child(MachinePanel::render_host_key_attention(
                    attention,
                    self.machines.clone().expect("machine entity"),
                    self.command_tx.clone().expect("machine command channel"),
                ))
                .into_any_element();
        }

        let dismiss_completed = self.attention.clone();
        let attention = self.attention.read(cx);

        match &attention.active {
            AttentionSnapshot::None => div().hidden().into_any_element(),

            AttentionSnapshot::PermissionRequired {
                task_id,
                permission_id,
                tool,
                reason,
            } => self
                .render_permission(task_id, permission_id.as_deref(), tool, reason)
                .into_any_element(),

            AttentionSnapshot::ReadyForCheck {
                task_id,
                summary,
                files_changed,
                tests_passed,
                browser_verified,
            } => self
                .render_ready_for_check(
                    task_id,
                    summary,
                    *files_changed,
                    *tests_passed,
                    *browser_verified,
                )
                .into_any_element(),

            AttentionSnapshot::Completed { summary, .. } => div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(gpui::rgba(0x00000060))
                .child(
                    div()
                        .w(px(360.0))
                        .rounded_lg()
                        .bg(gpui::rgb(0x1e293b))
                        .border_1()
                        .border_color(gpui::rgb(0x10b981))
                        .p_6()
                        .text_center()
                        .child(
                            div()
                                .text_lg()
                                .text_color(gpui::rgb(0x10b981))
                                .mb_2()
                                .child("✓ Task Completed"),
                        )
                        .child(div().text_sm().child(summary.clone()))
                        .child(
                            div()
                                .mt_4()
                                .px_3()
                                .py_2()
                                .rounded_md()
                                .bg(gpui::rgb(0x374151))
                                .cursor_pointer()
                                .id("completed-dismiss")
                                .on_click(move |_, _, cx| {
                                    dismiss_completed.update(cx, |store, cx| store.clear(cx));
                                })
                                .child("Close"),
                        ),
                )
                .into_any_element(),

            AttentionSnapshot::Error {
                task_id,
                error,
                recoverable,
            } => self
                .render_error(task_id, error, *recoverable)
                .into_any_element(),

            AttentionSnapshot::WaitingInput { task_id, prompt } => self
                .render_waiting_input(task_id, prompt)
                .into_any_element(),
        }
    }
}
