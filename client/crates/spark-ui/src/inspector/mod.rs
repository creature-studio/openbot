//! Inspector: the right-side panel with tabs.
//!
//! ```text
//! ┌────────────────────────┐
//! │ Browser │ Files │ Term  │
//! ├────────────────────────┤
//! │                        │
//! │   (active tab content) │
//! │                        │
//! └────────────────────────┘
//! ```
//!
//! Tabs:
//! - Browser: screenshot stream + DOM snapshot + interaction overlay
//! - Files: changed files list + diff viewer
//! - Terminal: terminal output
//! - Runtime: runtime info (future)

use gpui::{
    div, prelude::*, Context, Entity, IntoElement, Render, ViewContext,
    WindowContext,
};
use crate::stores::TaskStore;
use crate::browser::BrowserPanel;
use crate::terminal::TerminalPanel;
use crate::diff::DiffPanel;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InspectorTab {
    Browser,
    Files,
    Terminal,
}

pub struct Inspector {
    tasks: Entity<TaskStore>,
    active_tab: InspectorTab,
}

impl Inspector {
    pub fn new(tasks: Entity<TaskStore>, cx: &mut WindowContext) -> Self {
        cx.observe(&tasks, |_, _, cx| cx.notify()).detach();

        Self {
            tasks,
            active_tab: InspectorTab::Browser,
        }
    }

    fn set_tab(&mut self, tab: InspectorTab, cx: &mut ViewContext<Self>) {
        self.active_tab = tab;
        cx.notify();
    }
}

impl Render for Inspector {
    fn render(&mut self, cx: &mut ViewContext<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .h_full()
            .w(px(360.0))
            .bg(cx.theme().colors().panel_background)
            .border_l_1()
            .border_color(cx.theme().colors().border)
            .child(self.render_tabs(cx))
            .child(self.render_content(cx))
    }
}

impl Inspector {
    fn render_tabs(&self, cx: &mut ViewContext<Self>) -> impl IntoElement {
        div()
            .flex()
            .border_b_1()
            .border_color(cx.theme().colors().border)
            .child(self.tab_button("Browser", InspectorTab::Browser, cx))
            .child(self.tab_button("Files", InspectorTab::Files, cx))
            .child(self.tab_button("Terminal", InspectorTab::Terminal, cx))
    }

    fn tab_button(
        &self,
        label: &str,
        tab: InspectorTab,
        cx: &ViewContext<Self>,
    ) -> impl IntoElement {
        let is_active = self.active_tab == tab;
        div()
            .px_3()
            .py_2()
            .text_sm()
            .cursor_pointer()
            .when(is_active, |d| {
                d.border_b_2()
                    .border_color(gpui::rgb(0x3b82f6))
                    .text_color(gpui::rgb(0xffffff))
            })
            .when(!is_active, |d| d.text_color(gpui::rgb(0x9ca3af)))
            .hover(|d| d.text_color(gpui::rgb(0xffffff)))
            .child(label)
    }

    fn render_content(&self, cx: &mut ViewContext<Self>) -> impl IntoElement {
        let tasks = self.tasks.read(cx);
        let task = tasks.selected_task();

        match self.active_tab {
            InspectorTab::Browser => {
                div()
                    .flex_1()
                    .p_3()
                    .child(BrowserPanel::render_static(task))
            }
            InspectorTab::Files => {
                div()
                    .flex_1()
                    .p_3()
                    .child(DiffPanel::render_static(task))
            }
            InspectorTab::Terminal => {
                div()
                    .flex_1()
                    .p_3()
                    .child(TerminalPanel::render_static(task))
            }
        }
    }
}
