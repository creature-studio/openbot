//! Browser panel: screenshot stream + interaction overlay.
//!
//! First version displays screenshots from the agent's Chrome,
//! NOT an embedded Chromium/WebView.
//!
//! ```text
//! ┌────────────────────────┐
//! │ localhost:3000   ↻  ↗  │
//! ├────────────────────────┤
//! │                        │
//! │   screenshot image     │
//! │                        │
//! │       [Login]           │
//! │                        │
//! └────────────────────────┘
//! ```
//!
//! Agent actions are overlaid:
//! - clicked @e12 → ● click indicator
//! - filled @e8 "hello" → @e8 ← "hello"

use gpui::{div, prelude::*, IntoElement};
use crate::stores::task::TaskEntity;

pub struct BrowserPanel;

impl BrowserPanel {
    /// Render the browser panel for a task (static helper for non-entity usage).
    pub fn render_static(task: Option<&TaskEntity>) -> impl IntoElement {
        let Some(task) = task else {
            return div()
                .flex()
                .items_center()
                .justify_center()
                .h_full()
                .text_color(gpui::rgb(0x6b7280))
                .child("No task selected");
        };

        let url = task
            .browser_url
            .as_deref()
            .unwrap_or("No browser session");

        div()
            .flex()
            .flex_col()
            .h_full()
            // URL bar
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_2()
                    .py_1()
                    .bg(gpui::rgb(0x0f172a))
                    .rounded_md()
                    .mb_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(gpui::rgb(0x9ca3af))
                            .font_family("monospace")
                            .child(url.to_string()),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(div().text_xs().cursor_pointer().child("↻"))
                            .child(div().text_xs().cursor_pointer().child("↗")),
                    ),
            )
            // Screenshot area
            .child(
                div()
                    .flex_1()
                    .rounded_md()
                    .bg(gpui::rgb(0x0f172a))
                    .overflow_hidden()
                    .child(if task.browser_snapshot.is_some() {
                        // In real GPUI, render the JPEG/WebP frame as an image.
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .h_full()
                            .text_color(gpui::rgb(0x3b82f6))
                            .text_sm()
                            .child("Browser frame loaded")
                    } else {
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .h_full()
                            .text_color(gpui::rgb(0x6b7280))
                            .text_sm()
                            .child("No browser session active")
                    }),
            )
            // Recent actions overlay
            .when(!task.browser_actions.is_empty(), |d| {
                let actions: Vec<_> = task
                    .browser_actions
                    .iter()
                    .rev()
                    .take(5)
                    .collect();

                d.child(
                    div()
                        .mt_2()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .children(actions.into_iter().map(|action| {
                            div()
                                .text_xs()
                                .text_color(gpui::rgb(0x9ca3af))
                                .font_family("monospace")
                                .child(format!("→ {}", action))
                        })),
                )
            })
    }
}
