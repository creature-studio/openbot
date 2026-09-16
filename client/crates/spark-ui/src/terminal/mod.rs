//! Terminal panel: displays terminal output using alacritty_terminal.
//!
//! GPUI only handles: surface, keyboard, mouse, selection, scroll, rendering.
//! VT state is managed by spark-terminal (wrapping alacritty_terminal).
//!
//! ```text
//! ┌──────────────────────────────┐
//! │ terminal-1  bash      ✕     │
//! ├──────────────────────────────┤
//! │ $ cargo test                 │
//! │ running 28 tests...          │
//! │ all tests passed ✓           │
//! │ $ █                          │
//! └──────────────────────────────┘
//! ```

use gpui::{div, prelude::*, IntoElement};
use crate::stores::task::TaskEntity;

pub struct TerminalPanel;

impl TerminalPanel {
    /// Render the terminal panel for a task.
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

        if task.terminal_ids.is_empty() {
            let mut empty = div()
                .flex()
                .items_center()
                .justify_center()
                .h_full()
                .flex_col()
                .gap_2()
                .text_color(gpui::rgb(0x6b7280))
                .child("No terminal sessions");
            if let Some(runtime_id) = task.runtime_id.clone() {
                let tx = command_tx.clone();
                let machine_id = task.machine_id.clone();
                let terminal_id = format!("terminal-{}", task.id.0);
                let runtime_for_command = runtime_id.clone();
                empty = empty.child(
                    div()
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .bg(gpui::rgb(0x2563eb))
                        .cursor_pointer()
                        .text_sm()
                        .id("terminal-open")
                        .on_click(move |_, _, _| {
                            let _ = tx.send(spark_transport::TransportCommand::OpenTerminal {
                                machine_id: machine_id.clone(),
                                runtime_id: runtime_for_command.clone(),
                                terminal_id: terminal_id.clone(),
                                cols: 100,
                                rows: 30,
                            });
                        })
                        .child("Open terminal"),
                );
            }
            return empty;
        }

        let mut content = div().flex().flex_col().h_full();

        // Tab bar for terminal sessions
        let mut tabs = div()
            .flex()
            .border_b_1()
            .border_color(gpui::rgb(0x2d3748));

        for (i, term_id) in task.terminal_ids.iter().enumerate() {
            let close_tx = command_tx.clone();
            let close_machine = task.machine_id.clone();
            let close_runtime = task.runtime_id.clone().unwrap_or_default();
            let close_terminal = term_id.clone();
            let label = div()
                .text_xs()
                .font_family("monospace")
                .text_color(if i == 0 {
                    gpui::rgb(0xffffff)
                } else {
                    gpui::rgb(0x9ca3af)
                })
                .child(format!("{} bash", term_id));
            tabs = tabs.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_1()
                    .border_b_2()
                    .when(i == 0, |d| d.border_color(gpui::rgb(0x3b82f6)))
                    .child(label)
                    .child(
                        div()
                            .text_xs()
                            .cursor_pointer()
                            .id(format!("terminal-close-{}", term_id))
                            .on_click(move |_, _, _| {
                                let _ = close_tx.send(spark_transport::TransportCommand::CloseTerminal {
                                    machine_id: close_machine.clone(),
                                    runtime_id: close_runtime.clone(),
                                    terminal_id: close_terminal.clone(),
                                });
                            })
                            .child("×"),
                    ),
            );
        }

        content = content.child(tabs);

        // Terminal content area. Input is sent to the same remote Runtime PTY;
        // it is never an interactive SSH shell.
        let active_terminal = task.terminal_ids.first().cloned().unwrap_or_default();
        let output = task
            .terminal_output
            .get(&active_terminal)
            .map(|data| String::from_utf8_lossy(data).to_string())
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| "$ █".to_string());
        let exit_suffix = task
            .terminal_exit
            .get(&active_terminal)
            .map(|code| format!("\n[process exited: {:?}]", code))
            .unwrap_or_default();
        let runtime_id = task.runtime_id.clone().unwrap_or_default();
        let machine_id = task.machine_id.clone();
        let terminal_id = active_terminal.clone();
        let tx = command_tx.clone();
        content = content.child(
            div()
                .flex_1()
                .p_2()
                .bg(gpui::rgb(0x0c0c0c))
                .font_family("monospace")
                .text_xs()
                .id("terminal-output-scroll")
                .overflow_y_scroll()
                .child(
                    div()
                        .text_color(gpui::rgb(0xcccccc))
                        .child(format!("{}{}", output, exit_suffix)),
                )
                .child(
                    div()
                        .mt_2()
                        .px_1()
                        .py_1()
                        .text_color(gpui::rgb(0xcccccc))
                        .bg(gpui::rgb(0x111111))
                        .focusable()
                        .id("terminal-input")
                        .on_key_down(move |event, _, _| {
                            let key = event.keystroke.key.as_str();
                            let data = if key.eq_ignore_ascii_case("enter") {
                                b"\r".to_vec()
                            } else if key.eq_ignore_ascii_case("backspace") {
                                vec![0x7f]
                            } else if key.eq_ignore_ascii_case("tab") {
                                b"\t".to_vec()
                            } else {
                                event.keystroke.key_char.as_deref().unwrap_or("").as_bytes().to_vec()
                            };
                            if data.is_empty() {
                                return;
                            }
                            let _ = tx.send(spark_transport::TransportCommand::WriteTerminal {
                                machine_id: machine_id.clone(),
                                runtime_id: runtime_id.clone(),
                                terminal_id: terminal_id.clone(),
                                data,
                            });
                        })
                        .child("type here…"),
                ),
        );

        content
    }
}
