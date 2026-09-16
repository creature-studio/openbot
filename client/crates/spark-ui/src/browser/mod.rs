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

use std::sync::Arc;

use gpui::{div, img, prelude::*, ImageFormat, IntoElement};
use crate::stores::task::TaskEntity;

pub struct BrowserPanel;

impl BrowserPanel {
    /// Render the browser panel for a task (static helper for non-entity usage).
    pub fn render_static(
        task: Option<&TaskEntity>,
        command_tx: &tokio::sync::mpsc::UnboundedSender<spark_transport::TransportCommand>,
    ) -> impl IntoElement {
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
        let (refresh_button, open_button) = if let Some(runtime_id) = task.runtime_id.clone() {
            let refresh_tx = command_tx.clone();
            let open_tx = command_tx.clone();
            let refresh_runtime = runtime_id.clone();
            let open_runtime = runtime_id;
            let open_url = url.to_string();
            (
                div()
                    .text_xs()
                    .cursor_pointer()
                    .child("↻")
                    .id("browser-refresh")
                    .on_click(move |_, _, _| {
                        let _ = refresh_tx.send(spark_transport::TransportCommand::BrowserAction {
                            runtime_id: refresh_runtime.clone(),
                            action: spark_transport::BrowserAction::Screenshot {
                                format: "jpeg".to_string(),
                                quality: 80,
                            },
                        });
                    })
                    .into_any_element(),
                div()
                    .text_xs()
                    .cursor_pointer()
                    .child("↗")
                    .id("browser-open")
                    .on_click(move |_, _, _| {
                        let _ = open_tx.send(spark_transport::TransportCommand::BrowserAction {
                            runtime_id: open_runtime.clone(),
                            action: spark_transport::BrowserAction::Open {
                                url: open_url.clone(),
                            },
                        });
                    })
                    .into_any_element(),
            )
        } else {
            (
                div().text_xs().cursor_pointer().child("↻").into_any_element(),
                div().text_xs().cursor_pointer().child("↗").into_any_element(),
            )
        };

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
                            .child(refresh_button)
                            .child(open_button),
                    ),
            )
            // Screenshot area
            .child(
                div()
                    .flex_1()
                    .rounded_md()
                    .bg(gpui::rgb(0x0f172a))
                    .overflow_hidden()
                    .child(if let Some(bytes) = task.browser_snapshot.as_ref() {
                        let format = match task.browser_format.to_ascii_lowercase().as_str() {
                            "jpg" | "jpeg" => ImageFormat::Jpeg,
                            "webp" => ImageFormat::Webp,
                            "gif" => ImageFormat::Gif,
                            _ => ImageFormat::Png,
                        };
                        let image = Arc::new(gpui::Image::from_bytes(format, bytes.clone()));
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .h_full()
                            .child(img(image).max_w_full())
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
