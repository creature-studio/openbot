//! Diff panel: shows file changes with unified/side-by-side diff.
//!
//! ```text
//! ┌──────────────────────────────┐
//! │ Changes (3 files)            │
//! ├──────────────────────────────┤
//! │ src/login.tsx    +12 -3      │
//! │ src/auth.ts      +5  -1      │
//! │ tests/login.test +8  -0     │
//! ├──────────────────────────────┤
//! │ (selected file diff)         │
//! │ - const old = useAuth()      │
//! │ + const { auth, error } =    │
//! │ +   useAuth({ retry: true }) │
//! └──────────────────────────────┘
//! ```

use gpui::{div, prelude::*, Entity, IntoElement};
use crate::stores::task::{TaskEntity, TaskStore};

pub struct DiffPanel;

impl DiffPanel {
    pub fn render_static(
        task: Option<&TaskEntity>,
        tasks: Entity<TaskStore>,
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

        if task.file_changes.is_empty() {
            return div()
                .flex()
                .items_center()
                .justify_center()
                .h_full()
                .text_color(gpui::rgb(0x6b7280))
                .child("No file changes");
        }

        let mut content = div().flex().flex_col().h_full();

        // Header
        content = content.child(
            div()
                .text_sm()
                .font_weight(gpui::FontWeight::BOLD)
                .mb_2()
                .child(format!("Changes ({} files)", task.file_changes.len())),
        );

        // File list
        let mut file_list = div().flex().flex_col().gap_1().mb_3();

        for change in &task.file_changes {
            let selected = task.selected_file.as_deref() == Some(change.path.as_str());
            let change_path = change.path.clone();
            let task_id = task.id.clone();
            let task_store = tasks.clone();
            let icon = match &change.kind {
                spark_model::FileChangeKind::Added => "A",
                spark_model::FileChangeKind::Modified => "M",
                spark_model::FileChangeKind::Deleted => "D",
                spark_model::FileChangeKind::Renamed { .. } => "R",
            };

            file_list = file_list.child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .when(selected, |d| d.bg(gpui::rgb(0x1e293b)))
                    .cursor_pointer()
                    .id(format!("diff-file-{}", change_path))
                    .on_click(move |_, _, cx| {
                        task_store.update(cx, |tasks, cx| {
                            tasks.select_file(&task_id, change_path.clone(), cx);
                        });
                    })
                    .hover(|d| d.bg(gpui::rgb(0x1e293b)))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_xs()
                                    .font_family("monospace")
                                    .text_color(match &change.kind {
                                        spark_model::FileChangeKind::Added => {
                                            gpui::rgb(0x10b981)
                                        }
                                        spark_model::FileChangeKind::Deleted => {
                                            gpui::rgb(0xef4444)
                                        }
                                        _ => gpui::rgb(0xf59e0b),
                                    })
                                    .child(icon),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .font_family("monospace")
                                    .child(change.path.clone()),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .text_xs()
                            .child(
                                div()
                                    .text_color(gpui::rgb(0x10b981))
                                    .child(format!("+{}", change.additions)),
                            )
                            .child(
                                div()
                                    .text_color(gpui::rgb(0xef4444))
                                    .child(format!("-{}", change.deletions)),
                            ),
                    ),
            );
        }

        content = content.child(file_list);

        // Diff view for the selected file; default to the first change.
        let selected_change = task
            .selected_file
            .as_deref()
            .and_then(|path| task.file_changes.iter().find(|change| change.path == path))
            .or_else(|| task.file_changes.first());
        if let Some(first) = selected_change {
            if let Some(ref diff_text) = first.diff {
                content = content.child(
                    div()
                        .flex_1()
                        .p_2()
                        .rounded_md()
                        .bg(gpui::rgb(0x0f172a))
                        .font_family("monospace")
                        .text_xs()
                        .id("diff-view-scroll")
                        .overflow_y_scroll()
                        .children(diff_text.lines().map(|line| {
                            let (bg, fg) = if line.starts_with('+') {
                                (gpui::rgba(0x064e3b30), gpui::rgb(0x10b981))
                            } else if line.starts_with('-') {
                                (gpui::rgba(0x5f1e1e30), gpui::rgb(0xef4444))
                            } else if line.starts_with("@@") {
                                (gpui::rgba(0x1e3a5f30), gpui::rgb(0x3b82f6))
                            } else {
                                (gpui::rgba(0x00000000), gpui::rgb(0xcccccc))
                            };
                            div()
                                .bg(bg)
                                .text_color(fg)
                                .px_2()
                                .child(line.to_string())
                        })),
                );
            }
        }

        content
    }
}
