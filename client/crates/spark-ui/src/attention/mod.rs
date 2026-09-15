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
use crate::stores::{AttentionStore, AttentionSnapshot, MachineStore};

pub struct AttentionOverlay {
    attention: Entity<AttentionStore>,
    /// Machine-level attention (unknown / changed host key, auth failure).
    /// It takes precedence: a machine that is not trusted cannot run anything,
    /// so the user must answer this card first.
    machines: Option<Entity<MachineStore>>,
}

impl AttentionOverlay {
    pub fn new(attention: Entity<AttentionStore>, cx: &mut Context<Self>) -> Self {
        cx.observe(&attention, |_, _, cx| cx.notify()).detach();
        Self {
            attention,
            machines: None,
        }
    }

    /// Overlay that also shows the machine host-key confirmation card.
    pub fn with_machines(
        attention: Entity<AttentionStore>,
        machines: Entity<MachineStore>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&machines, |_, _, cx| cx.notify()).detach();
        let mut overlay = Self::new(attention, cx);
        overlay.machines = Some(machines);
        overlay
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
                .child(MachinePanel::render_host_key_attention(attention));
        }

        let attention = self.attention.read(cx);

        match &attention.active {
            AttentionSnapshot::None => div().hidden(),

            AttentionSnapshot::PermissionRequired {
                task_id: _,
                tool,
                reason,
            } => div()
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
                                .flex()
                                .items_center()
                                .gap_2()
                                .mb_4()
                                .child(
                                    div()
                                        .text_lg()
                                        .text_color(gpui::rgb(0xf59e0b))
                                        .child("⚠ Permission Required"),
                                ),
                        )
                        .child(
                            div()
                                .text_sm()
                                .mb_1()
                                .child(format!("Tool: {}", tool)),
                        )
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
                                .child(reason.clone()),
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
                                        .hover(|d| d.bg(gpui::rgb(0x4b5563)))
                                        .text_sm()
                                        .child("Cancel"),
                                )
                                .child(
                                    div()
                                        .px_4()
                                        .py_2()
                                        .rounded_md()
                                        .bg(gpui::rgb(0x059669))
                                        .cursor_pointer()
                                        .hover(|d| d.bg(gpui::rgb(0x047857)))
                                        .text_sm()
                                        .child("Allow Once"),
                                ),
                        ),
                ),

            AttentionSnapshot::ReadyForCheck {
                task_id: _,
                summary,
                files_changed,
                tests_passed,
                browser_verified,
            } => div()
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
                                .flex()
                                .items_center()
                                .gap_2()
                                .mb_4()
                                .child(
                                    div()
                                        .text_lg()
                                        .text_color(gpui::rgb(0x10b981))
                                        .child("✓ Ready for Check"),
                                ),
                        )
                        .child(div().text_sm().mb_3().child(summary.clone()))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .mb_4()
                                .text_xs()
                                .text_color(gpui::rgb(0x9ca3af))
                                .child(format!("{} files changed", files_changed))
                                .child(if *tests_passed {
                                    "✓ Tests passed"
                                } else {
                                    "○ Tests not run"
                                })
                                .child(if *browser_verified {
                                    "✓ Browser verified"
                                } else {
                                    "○ Browser not verified"
                                }),
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
                                        .hover(|d| d.bg(gpui::rgb(0x4b5563)))
                                        .text_sm()
                                        .child("Continue"),
                                )
                                .child(
                                    div()
                                        .px_4()
                                        .py_2()
                                        .rounded_md()
                                        .bg(gpui::rgb(0x059669))
                                        .cursor_pointer()
                                        .hover(|d| d.bg(gpui::rgb(0x047857)))
                                        .text_sm()
                                        .child("Confirm Complete"),
                                ),
                        ),
                ),

            AttentionSnapshot::Completed { summary } => div()
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
                        .child(div().text_sm().child(summary.clone())),
                ),

            AttentionSnapshot::Error { error, .. } => div()
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
                                .child(error.clone()),
                        ),
                ),

            AttentionSnapshot::WaitingInput { prompt } => div()
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
                        .child(div().text_sm().mb_4().child(prompt.clone()))
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
                                        .hover(|d| d.bg(gpui::rgb(0x2563eb)))
                                        .text_sm()
                                        .child("Send Response"),
                                ),
                        ),
                ),
        }
    }
}
