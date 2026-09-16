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

        if task.terminal_ids.is_empty() {
            return div()
                .flex()
                .items_center()
                .justify_center()
                .h_full()
                .text_color(gpui::rgb(0x6b7280))
                .child("No terminal sessions");
        }

        let mut content = div().flex().flex_col().h_full();

        // Tab bar for terminal sessions
        let mut tabs = div()
            .flex()
            .border_b_1()
            .border_color(gpui::rgb(0x2d3748));

        for (i, term_id) in task.terminal_ids.iter().enumerate() {
            tabs = tabs.child(
                div()
                    .px_3()
                    .py_1()
                    .text_xs()
                    .font_family("monospace")
                    .text_color(if i == 0 {
                        gpui::rgb(0xffffff)
                    } else {
                        gpui::rgb(0x9ca3af)
                    })
                    .border_b_2()
                    .when(i == 0, |d| d.border_color(gpui::rgb(0x3b82f6)))
                    .child(format!("{} bash", term_id)),
            );
        }

        content = content.child(tabs);

        // Terminal content area
        // In a real GPUI build, this would use spark-terminal's TerminalModel
        // to read the grid cells and render them with proper ANSI colors.
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
                ),
        );

        content
    }
}
